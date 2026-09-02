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

use crate::language::{Language, Readiness, ReadinessCell};
use crate::types::{CalleeInfo, DefinitionInfo, SymbolInfo};

/// Qué readiness declara una notificación, si declara alguna.
///
/// **Las dos formas son de cada servidor y no del protocolo**, así que se traducen
/// acá y de acá para arriba no hay dos vocabularios. `None` es *"esta notificación
/// no habla de readiness"*, que es el caso de casi todas.
///
/// De un servidor que informa no sale nunca `Running`: `Running` significa *"este
/// servidor no informa"*, y uno que mandó la notificación ya se contradijo.
fn readiness_from(method: &str, params: &serde_json::Value) -> Option<Readiness> {
    match method {
        // rust-analyzer: `quiescent` es *"no tengo trabajo pendiente"*. `health` habla
        // de errores del proyecto —un `cargo check` que falla— y no de si puede
        // contestar, así que no se mira: un workspace con errores igual resuelve
        // tipos.
        "experimental/serverStatus" => match params.get("quiescent")?.as_bool()? {
            true  => Some(Readiness::Ready),
            false => Some(Readiness::Indexing),
        },
        // jdtls: `ServiceReady` es el único que promete que el classpath está armado.
        // `Started` sale antes y no alcanza — es el que hacía que un `definitions`
        // pareciera contestado.
        "language/status" => match params.get("type")?.as_str()? {
            "ServiceReady" => Some(Readiness::Ready),
            _              => Some(Readiness::Indexing),
        },
        _ => None,
    }
}

pub struct LspClient {
    // ServerSocket requires &mut self → protect with Mutex for shared async access
    server:      Mutex<async_lsp::ServerSocket>,
    _task:       tokio::task::JoinHandle<()>,
    pub lang:    Language,
    pub queries: AtomicU64,
    workspace:   PathBuf,
    /// Lo que el servidor dijo de sí mismo. Ver `language.rs`.
    pub readiness: Arc<ReadinessCell>,
}

impl LspClient {
    pub async fn spawn(lang: Language, workspace: &Path) -> Result<Arc<Self>> {
        let exe = lang.find_executable()?;

        // **Se crea antes que el mainloop y antes que el cliente**, porque los dos la
        // necesitan y ninguno de los dos existe cuando se arma el otro: el router
        // vive en un closure que no puede ver un `LspClient` que todavía no está.
        let readiness = Arc::new(ReadinessCell::new(lang));
        let for_router = Arc::clone(&readiness);

        let (mainloop, mut server) = async_lsp::MainLoop::new_client(|_server| {
            let readiness = Arc::clone(&for_router);
            let mut router = async_lsp::router::Router::new(());
            router
                // Server→client notifications (ignore all)
                .notification::<notif::LogMessage>(|_, _| ControlFlow::Continue(()))
                .notification::<notif::ShowMessage>(|_, _| ControlFlow::Continue(()))
                .notification::<notif::Progress>(|_, _| ControlFlow::Continue(()))
                .notification::<notif::PublishDiagnostics>(|_, _| ControlFlow::Continue(()))
                // **Y cualquier otra**, que es lo que "ignore all" quería decir.
                //
                // Las extensiones son la norma: `jdtls` manda `language/status` en el
                // handshake, rust-analyzer manda `experimental/serverStatus`. Sin esto
                // la primera desconocida cae como error de ruteo y **mata el
                // mainloop** — Java no llegaba ni a inicializar.
                //
                // Una notificación no espera respuesta: ignorar una que no se entiende
                // es lo que el protocolo pide, no una concesión.
                //
                // **Pero esas dos justamente no se ignoran**: son las únicas que dicen
                // cuándo el servidor terminó de indexar, y sin ellas un vacío mientras
                // indexa es indistinguible de "no hay". Van acá y no en un
                // `.notification::<T>` porque no son del protocolo: no hay tipo en
                // `lsp_types` que las nombre, y despacharlas por string es lo que son.
                .unhandled_notification(move |_, n: async_lsp::AnyNotification| {
                    if let Some(r) = readiness_from(&n.method, &n.params) {
                        readiness.set(r);
                    }
                    ControlFlow::Continue(())
                })
                // Server→client requests: must respond or async-lsp closes the connection
                .request::<request::RegisterCapability, _>(|_, _| async { Ok(()) })
                .request::<request::UnregisterCapability, _>(|_, _| async { Ok(()) })
                .request::<request::WorkspaceConfiguration, _>(|_, _| async {
                    Ok::<Vec<serde_json::Value>, _>(vec![])
                })
                .request::<request::WorkDoneProgressCreate, _>(|_, _| async { Ok(()) })
                // Con una request es al revés: ignorarla **cuelga al servidor**, que
                // se queda esperando. Se contesta un error y la conexión sigue.
                .unhandled_request(|_, req: async_lsp::AnyRequest| async move {
                    Err(async_lsp::ResponseError::new(
                        async_lsp::ErrorCode::METHOD_NOT_FOUND,
                        format!("lspd no implementa `{}`", req.method),
                    ))
                });
            tower::ServiceBuilder::new().service(router)
        });

        // Use tokio::process and bridge to futures_io via tokio_util::compat
        let mut child = tokio::process::Command::new(&exe)
            // **El servidor se arranca parado en el workspace, no donde esté el
            // daemon.** `workspace_folders` en el `initialize` no alcanza: el
            // launcher de `jdtls` deriva su `-data` de `basename(getcwd())` antes de
            // que LSP exista, así que con el cwd del daemon indexa un proyecto que
            // no es —y dos workspaces con el mismo nombre de directorio comparten
            // datos—. Es lo que hacía que `definitions` devolviera `[]` con el
            // servidor ya listo.
            //
            // Y no es sólo de jdtls: el cwd de un proceso hijo que hereda el de
            // quien lo lanzó es un dato que nadie eligió. Acá sí se elige.
            .current_dir(workspace)
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
            // **`serverStatusNotification` hay que pedirlo.** rust-analyzer no manda
            // `experimental/serverStatus` a un cliente que no lo declara, y sin esa
            // notificación su readiness no se puede saber: se quedaría en `Indexing`
            // para siempre y ninguna pregunta se contestaría nunca.
            //
            // `jdtls` no necesita nada: `language/status` la manda igual.
            capabilities: ClientCapabilities {
                experimental: Some(serde_json::json!({
                    "serverStatusNotification": true,
                })),
                ..ClientCapabilities::default()
            },
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
            readiness,
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

    /// `textDocument/definition` sobre una posición.
    ///
    /// **Un salto y ninguna interpretación.** Devuelve dónde está declarado lo que
    /// se menciona ahí; qué se hace con eso es de quien pregunta. Es la pregunta que
    /// el cierre de firma de bilinker necesita, y la única que `lspd` sabe contestar
    /// que no es del call graph.
    pub async fn definitions(&self, file: &Path, line: u32, col: u32) -> Result<Vec<DefinitionInfo>> {
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

        let resp = server
            .definition(GotoDefinitionParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position { line, character: col },
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            })
            .await
            .map_err(|e| anyhow::anyhow!("definition: {e:?}"))?;

        // Las tres formas que la respuesta puede tomar se aplanan acá: para quien
        // pregunta son ubicaciones, y cuál de las tres usó el servidor no es un dato
        // suyo.
        let locations: Vec<Location> = match resp {
            None => Vec::new(),
            Some(GotoDefinitionResponse::Scalar(l)) => vec![l],
            Some(GotoDefinitionResponse::Array(v))  => v,
            Some(GotoDefinitionResponse::Link(v))   => v.into_iter()
                .map(|l| Location { uri: l.target_uri, range: l.target_range })
                .collect(),
        };

        Ok(locations.into_iter().filter_map(|l| {
            let path = l.uri.to_file_path().ok()?;
            Some(DefinitionInfo {
                name:     path.file_stem()?.to_string_lossy().to_string(),
                file:     path.to_string_lossy().to_string(),
                line:     l.range.start.line,
                col:      l.range.start.character,
                end_line: l.range.end.line,
                end_col:  l.range.end.character,
            })
        }).collect())
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

#[cfg(test)]
mod readiness_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rust_analyzer_dice_listo_con_quiescent() {
        assert_eq!(readiness_from("experimental/serverStatus", &json!({"quiescent": true})),
                   Some(Readiness::Ready));
        assert_eq!(readiness_from("experimental/serverStatus", &json!({"quiescent": false})),
                   Some(Readiness::Indexing));
    }

    /// `health` habla de errores del proyecto, no de si puede contestar: un workspace
    /// que no compila igual resuelve tipos.
    #[test]
    fn el_health_de_rust_analyzer_no_es_readiness() {
        assert_eq!(readiness_from("experimental/serverStatus",
                                  &json!({"quiescent": true, "health": "error"})),
                   Some(Readiness::Ready));
    }

    /// `Started` sale antes de que el classpath esté armado, y es el que hacía que un
    /// `definitions` pareciera contestado.
    #[test]
    fn jdtls_solo_esta_listo_en_service_ready() {
        assert_eq!(readiness_from("language/status", &json!({"type": "ServiceReady"})),
                   Some(Readiness::Ready));
        assert_eq!(readiness_from("language/status", &json!({"type": "Started"})),
                   Some(Readiness::Indexing));
        assert_eq!(readiness_from("language/status", &json!({"type": "Starting"})),
                   Some(Readiness::Indexing));
    }

    /// Casi todas las notificaciones no hablan de esto, y una que no la entendemos no
    /// puede mover el estado.
    #[test]
    fn cualquier_otra_notificacion_no_dice_nada() {
        assert_eq!(readiness_from("window/logMessage", &json!({"type": 3})), None);
        assert_eq!(readiness_from("$/progress", &json!({})), None);
        // Y una con el método correcto pero sin el campo tampoco inventa un estado.
        assert_eq!(readiness_from("experimental/serverStatus", &json!({})), None);
        assert_eq!(readiness_from("language/status", &json!({})), None);
    }

    /// El que avisa nace `Indexing`; el que no avisa nace `Running` y se queda ahí.
    #[test]
    fn el_estado_inicial_sale_de_si_el_servidor_avisa() {
        assert_eq!(Readiness::initial(Language::Rust),       Readiness::Indexing);
        assert_eq!(Readiness::initial(Language::Java),       Readiness::Indexing);
        assert_eq!(Readiness::initial(Language::TypeScript), Readiness::Running);
        assert_eq!(Readiness::initial(Language::Python),     Readiness::Running);
    }

    #[test]
    fn la_celda_arranca_en_el_inicial_y_sube() {
        let c = ReadinessCell::new(Language::Rust);
        assert_eq!(c.get(), Readiness::Indexing);
        c.set(Readiness::Ready);
        assert_eq!(c.get(), Readiness::Ready);
    }
}
