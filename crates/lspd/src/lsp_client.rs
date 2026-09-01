use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use async_lsp::LanguageServer;
use async_lsp::lsp_types::notification as notif;
use async_lsp::lsp_types::*;
use tokio::sync::Mutex;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::language::Language;
use crate::types::{CalleeInfo, SymbolInfo};

pub struct LspClient {
    // ServerSocket requires &mut self → protect with Mutex for shared async access
    server:      Mutex<async_lsp::ServerSocket>,
    _task:       tokio::task::JoinHandle<()>,
    pub lang:    Language,
    pub queries: AtomicU64,
    workspace:   PathBuf,
}

impl LspClient {
    pub async fn spawn(lang: Language, workspace: &Path) -> Result<Arc<Self>> {
        let exe = lang.find_executable()?;

        let (mainloop, mut server) = async_lsp::MainLoop::new_client(|_server| {
            let mut router = async_lsp::router::Router::new(());
            router
                // Server→client notifications (ignore all)
                .notification::<notif::LogMessage>(|_, _| ControlFlow::Continue(()))
                .notification::<notif::ShowMessage>(|_, _| ControlFlow::Continue(()))
                .notification::<notif::Progress>(|_, _| ControlFlow::Continue(()))
                .notification::<notif::PublishDiagnostics>(|_, _| ControlFlow::Continue(()))
                // Server→client requests: must respond or async-lsp closes the connection
                .request::<request::RegisterCapability, _>(|_, _| async { Ok(()) })
                .request::<request::UnregisterCapability, _>(|_, _| async { Ok(()) })
                .request::<request::WorkspaceConfiguration, _>(|_, _| async {
                    Ok::<Vec<serde_json::Value>, _>(vec![])
                })
                .request::<request::WorkDoneProgressCreate, _>(|_, _| async { Ok(()) });
            tower::ServiceBuilder::new().service(router)
        });

        // Use tokio::process and bridge to futures_io via tokio_util::compat
        let mut child = tokio::process::Command::new(&exe)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| anyhow::anyhow!("failed to spawn {exe}: {e}"))?;

        let stdin  = child.stdin.take().unwrap().compat_write();
        let stdout = child.stdout.take().unwrap().compat();

        let task = tokio::spawn(async move {
            // child must live as long as the mainloop — drop kills the process
            let _child = child;
            if let Err(e) = mainloop.run_buffered(stdout, stdin).await {
                eprintln!("[lsp] mainloop exited: {e:?}");
            }
        });

        let workspace_url = Url::from_file_path(workspace)
            .map_err(|_| anyhow::anyhow!("invalid workspace path: {}", workspace.display()))?;

        server.initialize(InitializeParams {
            workspace_folders: Some(vec![WorkspaceFolder {
                uri: workspace_url,
                name: workspace
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default(),
            }]),
            capabilities: ClientCapabilities::default(),
            ..Default::default()
        })
        .await
        .map_err(|e| anyhow::anyhow!("LSP initialize: {e:?}"))?;

        // initialized is a notification — returns Result synchronously, no .await
        server
            .initialized(InitializedParams {})
            .map_err(|e| anyhow::anyhow!("LSP initialized: {e:?}"))?;

        Ok(Arc::new(Self {
            server: Mutex::new(server),
            _task: task,
            lang,
            queries: AtomicU64::new(0),
            workspace: workspace.to_path_buf(),
        }))
    }

    pub async fn callees(&self, file: &Path, line: u32, col: u32) -> Result<Vec<CalleeInfo>> {
        self.queries.fetch_add(1, Ordering::Relaxed);

        let uri = self.file_url(file)?;
        let mut server = self.server.lock().await;

        // Ensure the file is loaded into the LSP's VFS before querying
        let content = std::fs::read_to_string(file)
            .map_err(|e| anyhow::anyhow!("read {}: {e}", file.display()))?;
        server
            .did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: lang_id(file).to_string(),
                    version: 0,
                    text: content,
                },
            })
            .map_err(|e| anyhow::anyhow!("didOpen: {e:?}"))?;

        let items = server
            .prepare_call_hierarchy(CallHierarchyPrepareParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position { line, character: col },
                },
                work_done_progress_params: Default::default(),
            })
            .await
            .map_err(|e| anyhow::anyhow!("prepareCallHierarchy: {e:?}"))?
            .unwrap_or_default();

        let mut result = Vec::new();

        for item in items {
            let calls = server
                .outgoing_calls(CallHierarchyOutgoingCallsParams {
                    item,
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                })
                .await
                .map_err(|e| anyhow::anyhow!("outgoingCalls: {e:?}"))?
                .unwrap_or_default();

            for call in calls {
                let t = call.to;
                let file_path = t
                    .uri
                    .to_file_path()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| t.uri.to_string());
                result.push(CalleeInfo {
                    symbol: t.detail.clone().unwrap_or_else(|| t.name.clone()),
                    name: t.name,
                    file: file_path,
                    line: t.selection_range.start.line,
                    col: t.selection_range.start.character,
                });
            }
        }

        Ok(result)
    }

    pub async fn callers(&self, file: &Path, line: u32, col: u32) -> Result<Vec<CalleeInfo>> {
        self.queries.fetch_add(1, Ordering::Relaxed);

        let uri = self.file_url(file)?;
        let mut server = self.server.lock().await;

        let content = std::fs::read_to_string(file)
            .map_err(|e| anyhow::anyhow!("read {}: {e}", file.display()))?;
        server
            .did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: lang_id(file).to_string(),
                    version: 0,
                    text: content,
                },
            })
            .map_err(|e| anyhow::anyhow!("didOpen: {e:?}"))?;

        let items = server
            .prepare_call_hierarchy(CallHierarchyPrepareParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position { line, character: col },
                },
                work_done_progress_params: Default::default(),
            })
            .await
            .map_err(|e| anyhow::anyhow!("prepareCallHierarchy: {e:?}"))?
            .unwrap_or_default();

        let mut result = Vec::new();

        for item in items {
            let calls = server
                .incoming_calls(CallHierarchyIncomingCallsParams {
                    item,
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                })
                .await
                .map_err(|e| anyhow::anyhow!("incomingCalls: {e:?}"))?
                .unwrap_or_default();

            for call in calls {
                let f = call.from;
                let file_path = f
                    .uri
                    .to_file_path()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| f.uri.to_string());
                result.push(CalleeInfo {
                    symbol: f.detail.clone().unwrap_or_else(|| f.name.clone()),
                    name: f.name,
                    file: file_path,
                    line: f.selection_range.start.line,
                    col: f.selection_range.start.character,
                });
            }
        }

        Ok(result)
    }

    pub async fn symbol_at(&self, file: &Path, line: u32, col: u32) -> Result<Option<SymbolInfo>> {
        self.queries.fetch_add(1, Ordering::Relaxed);

        let uri = self.file_url(file)?;
        let mut server = self.server.lock().await;

        let content = std::fs::read_to_string(file)
            .map_err(|e| anyhow::anyhow!("read {}: {e}", file.display()))?;
        server
            .did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: lang_id(file).to_string(),
                    version: 0,
                    text: content,
                },
            })
            .map_err(|e| anyhow::anyhow!("didOpen: {e:?}"))?;

        let hover = server
            .hover(HoverParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position { line, character: col },
                },
                work_done_progress_params: Default::default(),
            })
            .await
            .map_err(|e| anyhow::anyhow!("hover: {e:?}"))?;

        let Some(hover) = hover else { return Ok(None) };

        let text = hover_to_text(&hover);
        let name = extract_name(&text);

        Ok(Some(SymbolInfo { symbol: text, name, kind: "function".to_string() }))
    }

    pub async fn shutdown(&self) {
        let mut server = self.server.lock().await;
        let _ = server.shutdown(()).await;
        let _ = server.exit(());
    }

    fn file_url(&self, file: &Path) -> Result<Url> {
        let abs = if file.is_absolute() {
            file.to_path_buf()
        } else {
            self.workspace.join(file)
        };
        Url::from_file_path(&abs)
            .map_err(|_| anyhow::anyhow!("invalid file path: {}", abs.display()))
    }
}

fn lang_id(file: &Path) -> &'static str {
    match file.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "rs"           => "rust",
        "ts" | "tsx"   => "typescript",
        "js" | "jsx"   => "javascript",
        "py"           => "python",
        "java"         => "java",
        _              => "plaintext",
    }
}

fn hover_to_text(hover: &Hover) -> String {
    match &hover.contents {
        HoverContents::Scalar(ms) => match ms {
            MarkedString::String(s) => s.clone(),
            MarkedString::LanguageString(ls) => ls.value.clone(),
        },
        HoverContents::Array(arr) => arr
            .iter()
            .map(|ms| match ms {
                MarkedString::String(s) => s.as_str(),
                MarkedString::LanguageString(ls) => ls.value.as_str(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        HoverContents::Markup(mc) => mc.value.clone(),
    }
}

fn extract_name(text: &str) -> String {
    text.lines()
        .next()
        .and_then(|l| l.split_whitespace().last())
        .unwrap_or("unknown")
        .trim_end_matches('(')
        .to_string()
}
