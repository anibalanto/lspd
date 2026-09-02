use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tokio::sync::RwLock;

use crate::language::Language;
use crate::lsp_client::LspClient;
use crate::language::Readiness;
use crate::types::{CalleeInfo, DefinitionInfo, LspStatus, SymbolInfo};

/// El lugar de un lenguaje en el mapa, que se ocupa **antes** de que el servidor
/// exista.
///
/// Es lo que hace que *"uno por lenguaje"* sea una invariante y no una probabilidad:
/// sin él, entre el `spawn` y el `insert` hay una ventana —el handshake entero, que
/// con `jdtls` son segundos— en la que el mapa está vacío y el servidor ya arrancando,
/// y toda query que caiga ahí levanta el suyo.
///
/// **Guarda el resultado, no sólo el éxito.** Un arranque que falló tiene que llegarle
/// a todos los que esperaban y no sólo al que lo pidió primero; por eso el error viaja
/// como `String` —`anyhow::Error` no es `Clone`— y se reconstruye de este lado.
struct LspSlot {
    tx: tokio::sync::watch::Sender<Option<Result<Arc<LspClient>, String>>>,
}

impl LspSlot {
    fn pending() -> Self {
        Self { tx: tokio::sync::watch::channel(None).0 }
    }

    /// **`send_replace` y no `send`.** `send` falla cuando no queda ningún receiver
    /// —el que pidió se fue mientras arrancaba— y en esa falla *no escribe el valor*:
    /// el slot quedaría `Pending` para siempre, con el servidor ya arriba y todo el
    /// que llegue después esperando un arranque que ya terminó. El resultado se guarda
    /// haya alguien escuchando o no.
    fn settle(&self, r: anyhow::Result<Arc<LspClient>>) {
        self.tx.send_replace(Some(r.map_err(|e| e.to_string())));
    }

    /// Espera a que el arranque termine. Los que llegan después de que terminó no
    /// esperan nada.
    async fn get(&self) -> anyhow::Result<Arc<LspClient>> {
        let mut rx = self.tx.subscribe();
        loop {
            {
                let cur = rx.borrow_and_update();
                if let Some(r) = cur.as_ref() {
                    return r.clone().map_err(anyhow::Error::msg);
                }
            }
            rx.changed()
                .await
                .map_err(|_| anyhow::anyhow!("el arranque del language server se abandonó"))?;
        }
    }

    /// Lo que ya está, sin esperar. Para quien no puede bloquearse — `status`.
    fn ready(&self) -> Option<Arc<LspClient>> {
        self.tx.borrow().as_ref().and_then(|r| r.as_ref().ok()).map(Arc::clone)
    }
}

pub struct LspManager {
    workspace: PathBuf,
    /// **El censo de lo que corre, y el dueño de cada proceso.** Sacar un slot de acá
    /// es matar al servidor: el `Arc<LspClient>` que el slot guarda es el último, y
    /// [`Drop`](LspClient) hace el resto. Ver `concepts/language-servers.md` § *"Uno
    /// por lenguaje es una invariante"*.
    clients:   RwLock<HashMap<Language, Arc<LspSlot>>>,
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
                return c.get().await;
            }
        }

        // **El lugar se reserva antes del handshake, y lo que se comparte es la
        // promesa.** El que llega segundo encuentra el `Pending` y espera *ese*
        // arranque; no hay ventana en la que el mapa esté vacío y el servidor ya
        // arrancando.
        //
        // Al revés —levantar y después ver si alguien ganó de mano— el `or_insert`
        // conserva uno y descarta los otros, y **eso son N-1 servidores de más**: con
        // `jdtls` el handshake son segundos y `check` pregunta en paralelo, así que la
        // ventana no es teórica. Es lo que dejó nueve JVMs vivas para un solo daemon.
        let slot = {
            let mut w = self.clients.write().await;
            // Entre el `read` de arriba y este `write` puede haber entrado otro.
            if let Some(c) = w.get(&lang) {
                Arc::clone(c)
            } else {
                let slot = Arc::new(LspSlot::pending());
                w.insert(lang, Arc::clone(&slot));
                let workspace = self.workspace.clone();
                let mine = Arc::clone(&slot);
                // El arranque va afuera de todo lock: sostenerlo durante el handshake
                // dejaría al daemon entero sin poder contestar `status`.
                tokio::spawn(async move {
                    mine.settle(LspClient::spawn(lang, &workspace).await);
                });
                slot
            }
        };

        let started = slot.get().await;

        // **Un arranque que falló no deja el lugar tomado.** Si quedara, el error se
        // volvería permanente: instalar el ejecutable que faltaba no cambiaría nada
        // hasta reiniciar el daemon.
        if started.is_err() {
            let mut w = self.clients.write().await;
            if let Some(c) = w.get(&lang) {
                if Arc::ptr_eq(c, &slot) { w.remove(&lang); }
            }
        }

        started
    }

    pub async fn callees(&self, file: &str, line: u32, col: u32) -> anyhow::Result<Vec<CalleeInfo>> {
        let client = self.client_for(file).await?;
        ready_or_bail(&client)?;
        let abs = abs_path(file, &self.workspace);
        client.callees(&abs, line, col).await
    }

    pub async fn callers(&self, file: &str, line: u32, col: u32) -> anyhow::Result<Vec<CalleeInfo>> {
        let client = self.client_for(file).await?;
        ready_or_bail(&client)?;
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
        ready_or_bail(&client)?;
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
        ready_or_bail(&client)?;
        let abs = abs_path(file, &self.workspace);
        client.definitions(&abs, line, col).await
    }

    pub async fn shutdown(&self) {
        let slots: Vec<Arc<LspSlot>> = {
            let mut w = self.clients.write().await;
            w.drain().map(|(_, s)| s).collect()
        };
        for s in slots {
            // **Un slot que todavía está arrancando se espera, no se saltea.** Es el
            // caso que deja huérfano de verdad: si el daemon termina primero, el
            // proceso que arranca después ya no tiene quién lo mate, y `kill_on_drop`
            // no corre en un padre que murió.
            if let Ok(c) = s.get().await {
                c.shutdown().await;
            }
        }
        // Y acá se sueltan los últimos `Arc`: lo que el `shutdown` por LSP no haya
        // cerrado, lo cierra el `Drop`.
    }

    pub async fn status(&self) -> Vec<LspStatus> {
        let r = self.clients.read().await;
        r.iter()
            .map(|(lang, slot)| match slot.ready() {
                Some(c) => LspStatus {
                    name:    c.lang.name().to_string(),
                    state:   c.readiness.get().as_str().to_string(),
                    queries: c.queries.load(Ordering::Relaxed),
                },
                // **Un servidor en pleno handshake se reporta**, porque está corriendo
                // y el mapa lo tiene. Su estado es el que ese mismo cliente va a
                // reportar un instante después: `status` no espera a nadie —quien
                // pregunta cómo viene el arranque no puede quedarse colgado en él— y
                // por eso no hay un cuarto valor de readiness que signifique
                // *"arrancando"*.
                None => LspStatus {
                    name:    lang.name().to_string(),
                    state:   Readiness::initial(*lang).as_str().to_string(),
                    queries: 0,
                },
            })
            .collect()
    }
}

fn abs_path(file: &str, workspace: &Path) -> std::path::PathBuf {
    let p = Path::new(file);
    if p.is_absolute() { p.to_path_buf() } else { workspace.join(file) }
}

/// El error que un servidor todavía indexando produce, y que `dispatch` traduce a
/// `-32001`.
///
/// **Tipo propio y no un `anyhow!` con un mensaje**, porque hay que reconocerlo del
/// otro lado: un código de error que dependa de matchear un string es un código de
/// error que se rompe cuando alguien mejora la redacción.
#[derive(Debug)]
pub struct NotReady {
    pub lang: &'static str,
}

impl std::fmt::Display for NotReady {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} todavía está indexando: volvé a preguntar. `lspd status` dice cuándo.",
               self.lang)
    }
}

impl std::error::Error for NotReady {}

/// Se niega a preguntarle a un servidor que dijo que todavía no puede contestar.
///
/// **Es acá y no en `LspClient` porque acá está la decisión y allá la conexión.** Y
/// no se espera: los siete minutos de un `rust-analyzer` en frío no entran en el
/// timeout de ningún cliente, y tapar el vacío con una espera es lo mismo que
/// taparlo con un reintento.
fn ready_or_bail(client: &crate::lsp_client::LspClient) -> anyhow::Result<()> {
    match client.readiness.get() {
        Readiness::Indexing => Err(NotReady { lang: client.lang.name() }.into()),
        // `Running` pasa: es *"este servidor no informa"*, y negarle la pregunta a un
        // lenguaje que nunca va a avisar lo dejaría mudo para siempre.
        Readiness::Ready | Readiness::Running => Ok(()),
    }
}

#[cfg(test)]
mod slot_tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::timeout;

    /// **Lo que fija el test es que el segundo espera**, que es la mitad de la
    /// invariante que se puede probar sin levantar un language server: mientras el
    /// slot está pendiente, `get` no contesta. Si contestara —o si el lugar no
    /// estuviera tomado— el que llega segundo arrancaría su propio servidor.
    #[tokio::test]
    async fn un_slot_pendiente_hace_esperar() {
        let slot = LspSlot::pending();
        assert!(timeout(Duration::from_millis(50), slot.get()).await.is_err());
    }

    /// Un arranque que falló les llega a **todos** los que esperaban, y no sólo al que
    /// lo pidió primero: los otros no tienen de dónde enterarse.
    #[tokio::test]
    async fn el_error_del_arranque_llega_a_los_que_esperan() {
        let slot = Arc::new(LspSlot::pending());
        let esperando: Vec<_> = (0..3)
            .map(|_| {
                let s = Arc::clone(&slot);
                tokio::spawn(async move { s.get().await.map_err(|e| e.to_string()) })
            })
            .collect();

        slot.settle(Err(anyhow::anyhow!("rust-analyzer no está en PATH")));

        for h in esperando {
            let e = h.await.unwrap().err().expect("tenía que fallar");
            assert!(e.contains("rust-analyzer no está en PATH"), "llegó: {e}");
        }
    }

    /// **El resultado se guarda aunque no lo espere nadie.** Es el caso de la task que
    /// se canceló mientras el servidor arrancaba: si el slot se quedara pendiente, el
    /// siguiente esperaría para siempre un arranque que ya terminó.
    #[tokio::test]
    async fn el_resultado_sobrevive_a_que_nadie_lo_escuche() {
        let slot = LspSlot::pending();
        slot.settle(Err(anyhow::anyhow!("jdtls no está en PATH")));

        let r = timeout(Duration::from_millis(50), slot.get()).await
            .expect("no debería esperar: el arranque ya terminó");
        assert!(r.err().expect("tenía que fallar").to_string().contains("jdtls"));
    }

    /// Un slot que todavía no resolvió **no tiene cliente**, y `status` no se cuelga
    /// preguntándolo.
    #[tokio::test]
    async fn ready_no_espera() {
        let slot = LspSlot::pending();
        assert!(slot.ready().is_none());
        slot.settle(Err(anyhow::anyhow!("falló")));
        assert!(slot.ready().is_none());
    }
}
