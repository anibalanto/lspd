use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tokio::sync::RwLock;

use crate::language::Language;
use crate::lsp_client::LspClient;
use crate::types::{CalleeInfo, DefinitionInfo, LspStatus, SymbolInfo};

pub struct LspManager {
    workspace: PathBuf,
    clients:   RwLock<HashMap<Language, Arc<LspClient>>>,
}

impl LspManager {
    pub fn new(workspace: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            workspace,
            clients: RwLock::new(HashMap::new()),
        })
    }

    async fn client_for(&self, file: &str) -> anyhow::Result<Arc<LspClient>> {
        let ext = Path::new(file)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let lang = Language::from_extension(ext)
            .ok_or_else(|| anyhow::anyhow!("no LSP for extension .{ext}"))?;

        {
            let r = self.clients.read().await;
            if let Some(c) = r.get(&lang) {
                return Ok(Arc::clone(c));
            }
        }

        let client = LspClient::spawn(lang, &self.workspace).await?;
        let mut w = self.clients.write().await;
        // another task may have spawned it while we were waiting for the write lock
        Ok(Arc::clone(w.entry(lang).or_insert(client)))
    }

    pub async fn callees(&self, file: &str, line: u32, col: u32) -> anyhow::Result<Vec<CalleeInfo>> {
        let client = self.client_for(file).await?;
        let abs = abs_path(file, &self.workspace);
        client.callees(&abs, line, col).await
    }

    pub async fn callers(&self, file: &str, line: u32, col: u32) -> anyhow::Result<Vec<CalleeInfo>> {
        let client = self.client_for(file).await?;
        let abs = abs_path(file, &self.workspace);
        client.callers(&abs, line, col).await
    }

    pub async fn symbol_at(
        &self,
        file: &str,
        line: u32,
        col: u32,
    ) -> anyhow::Result<Option<SymbolInfo>> {
        let client = self.client_for(file).await?;
        let abs = abs_path(file, &self.workspace);
        client.symbol_at(&abs, line, col).await
    }

    /// Dónde están declarados los tipos que se mencionan en esta posición.
    pub async fn definitions(
        &self,
        file: &str,
        line: u32,
        col: u32,
    ) -> anyhow::Result<Vec<DefinitionInfo>> {
        let client = self.client_for(file).await?;
        let abs = abs_path(file, &self.workspace);
        client.definitions(&abs, line, col).await
    }

    pub async fn shutdown(&self) {
        let clients: Vec<Arc<LspClient>> = {
            let mut w = self.clients.write().await;
            w.drain().map(|(_, c)| c).collect()
        };
        for c in clients {
            c.shutdown().await;
        }
    }

    pub async fn status(&self) -> Vec<LspStatus> {
        let r = self.clients.read().await;
        r.values()
            .map(|c| LspStatus {
                name:    c.lang.name().to_string(),
                state:   "RUNNING".to_string(),
                queries: c.queries.load(Ordering::Relaxed),
            })
            .collect()
    }
}

fn abs_path(file: &str, workspace: &Path) -> std::path::PathBuf {
    let p = Path::new(file);
    if p.is_absolute() { p.to_path_buf() } else { workspace.join(file) }
}
