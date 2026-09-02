//! El servidor: acepta conexiones y despacha el [protocolo](../../../concepts/protocol.md).
//!
//! **El protocolo está escrito una sola vez.** [`handle_conn`] es genérico sobre
//! cualquier par de streams asincrónicos, así que el socket Unix y el named pipe de
//! Windows entran por [`listen`] y de ahí para arriba no hay diferencia.

use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::Notify;

use lspd_client::Endpoint;

use crate::lsp_manager::LspManager;
use crate::types::{RpcRequest, RpcResponse};

pub async fn serve(
    manager:  Arc<LspManager>,
    endpoint: &Endpoint,
    shutdown: Arc<Notify>,
) -> anyhow::Result<()> {
    listen(manager, endpoint, shutdown).await
}

// ─── el transporte, que es lo único que cambia por sistema operativo ──────────

#[cfg(unix)]
async fn listen(
    manager:  Arc<LspManager>,
    endpoint: &Endpoint,
    shutdown: Arc<Notify>,
) -> anyhow::Result<()> {
    use tokio::net::UnixListener;

    let Endpoint::Socket(path) = endpoint else { unreachable!("en unix es un socket") };
    // Un socket que quedó de un daemon que murió: nadie escucha, y bindear encima
    // falla. Sacarlo es lo que hace que un arranque después de un crash funcione.
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    if let Some(dir) = path.parent() { std::fs::create_dir_all(dir)?; }
    let listener = UnixListener::bind(path)?;

    loop {
        tokio::select! {
            res = listener.accept() => {
                let (stream, _) = res?;
                let (reader, writer) = stream.into_split();
                tokio::spawn(handle_conn(reader, writer,
                    Arc::clone(&manager), Arc::clone(&shutdown)));
            }
            _ = shutdown.notified() => break,
        }
    }
    Ok(())
}

/// En Windows no hay listener: hay **instancias** de pipe.
///
/// Cada `connect()` atiende a un cliente y se consume, así que hay que crear la
/// siguiente instancia antes de ponerse a atender la actual — si no, entre una y
/// otra el pipe no existe y un cliente que llegue en esa ventana recibe un error en
/// vez de esperar.
#[cfg(windows)]
async fn listen(
    manager:  Arc<LspManager>,
    endpoint: &Endpoint,
    shutdown: Arc<Notify>,
) -> anyhow::Result<()> {
    use tokio::net::windows::named_pipe::ServerOptions;

    let Endpoint::Pipe(name) = endpoint else { unreachable!("en windows es un pipe") };
    let mut server = ServerOptions::new().first_pipe_instance(true).create(name)?;

    loop {
        tokio::select! {
            res = server.connect() => {
                res?;
                let conn = server;
                server = ServerOptions::new().create(name)?;
                let (reader, writer) = tokio::io::split(conn);
                tokio::spawn(handle_conn(reader, writer,
                    Arc::clone(&manager), Arc::clone(&shutdown)));
            }
            _ = shutdown.notified() => break,
        }
    }
    Ok(())
}

// ─── el protocolo, que no cambia ──────────────────────────────────────────────

async fn handle_conn<R, W>(
    reader:   R,
    mut writer: W,
    manager:  Arc<LspManager>,
    shutdown: Arc<Notify>,
)
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut lines = BufReader::new(reader).lines();

    while let Ok(Some(line)) = lines.next_line().await {
        let (resp, stop) = dispatch(&manager, &shutdown, &line).await;
        let json = match serde_json::to_string(&resp) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if writer.write_all(json.as_bytes()).await.is_err() { break; }
        if writer.write_all(b"\n").await.is_err() { break; }
        if writer.flush().await.is_err() { break; }
        if stop { break; }
    }
}

async fn dispatch(
    manager:  &LspManager,
    shutdown: &Notify,
    line:     &str,
) -> (RpcResponse, bool) {
    let req: RpcRequest = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => return (RpcResponse::parse_error(e.to_string()), false),
    };

    let id = req.id.clone();

    /// Los tres métodos posicionales toman lo mismo.
    #[derive(serde::Deserialize)]
    struct Pos { file: String, line: u32, col: u32 }

    macro_rules! positional {
        ($call:ident) => {{
            let p: Pos = match serde_json::from_value(req.params) {
                Ok(v) => v,
                Err(e) => return (RpcResponse::invalid_params(id, e.to_string()), false),
            };
            let resp = match manager.$call(&p.file, p.line, p.col).await {
                Ok(v)  => RpcResponse::ok(id, serde_json::json!(v)),
                // **`-32001` se separa antes de caer en el error genérico.** Un
                // servidor que todavía indexa no falló, y quien pregunta necesita
                // poder distinguirlo para no leer el vacío como "no hay".
                Err(e) if e.downcast_ref::<crate::lsp_manager::NotReady>().is_some() =>
                    RpcResponse::not_ready(id, e.to_string()),
                Err(e) => RpcResponse::server_error(id, e.to_string()),
            };
            (resp, false)
        }};
    }

    match req.method.as_str() {
        "ping"      => (RpcResponse::ok(id, serde_json::json!("pong")), false),
        "callees"   => positional!(callees),
        "callers"   => positional!(callers),
        "symbol_at" => positional!(symbol_at),
        "definitions" => positional!(definitions),
        "status"    => {
            let status = manager.status().await;
            (RpcResponse::ok(id, serde_json::json!(status)), false)
        }
        "shutdown"  => {
            shutdown.notify_one();
            (RpcResponse::ok(id, serde_json::Value::Null), true)
        }
        method => (RpcResponse::method_not_found(id, method), false),
    }
}
