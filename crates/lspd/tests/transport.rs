//! El transporte, contra el que corre en este sistema operativo.
//!
//! **Es el único test que hace falta correr en los tres.** El protocolo está escrito
//! una sola vez y es genérico sobre los streams; lo que cambia por sistema es cómo
//! se obtiene ese par de streams, y eso es lo que se prueba acá.
//!
//! El endpoint es propio y no el derivado: un test no puede pisarle el socket al
//! daemon del usuario.

use std::sync::Arc;
use std::time::Duration;

use lspd::{ipc, lsp_manager::LspManager};
use lspd_client::Endpoint;
use tokio::sync::Notify;

/// Un endpoint que no es el del usuario, único por proceso y por test.
fn scratch(name: &str) -> (Endpoint, Option<tempfile::TempDir>) {
    let unique = format!("{}-{}-{name}", std::process::id(), std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
    if cfg!(windows) {
        (Endpoint::Pipe(format!(r"\\.\pipe\lspd-test-{unique}")), None)
    } else {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.sock");
        (Endpoint::Socket(path), Some(dir))
    }
}

/// Levanta el servidor y devuelve con qué apagarlo.
fn serve(endpoint: Endpoint) -> (Arc<Notify>, std::thread::JoinHandle<()>) {
    let shutdown = Arc::new(Notify::new());
    let s = Arc::clone(&shutdown);
    let handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let manager = LspManager::new(std::env::current_dir().unwrap());
            let _ = ipc::serve(manager, &endpoint, s).await;
        });
    });
    (shutdown, handle)
}

fn wait_until_up(ep: &Endpoint) {
    for _ in 0..100 {
        if lspd_client::rpc_at(ep, "ping", serde_json::json!({})).is_ok() { return; }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("el daemon de prueba no levantó en {ep}");
}

/// Una pregunta y su respuesta, sobre el transporte real de este sistema.
#[test]
fn a_ping_goes_and_comes_back() {
    let (ep, _dir) = scratch("ping");
    let (shutdown, handle) = serve(ep.clone());
    wait_until_up(&ep);

    let pong = lspd_client::rpc_at(&ep, "ping", serde_json::json!({})).unwrap();
    assert_eq!(pong, serde_json::json!("pong"));

    shutdown.notify_one();
    let _ = lspd_client::rpc_at(&ep, "shutdown", serde_json::json!({}));
    let _ = handle.join();
}

/// Varias preguntas por la misma conexión, y varias conexiones seguidas: en Windows
/// cada `connect()` consume una instancia del pipe, así que la segunda conexión sólo
/// funciona si el servidor creó la siguiente antes de ponerse a atender la primera.
#[test]
fn a_second_client_is_served_too() {
    let (ep, _dir) = scratch("second");
    let (shutdown, handle) = serve(ep.clone());
    wait_until_up(&ep);

    for _ in 0..3 {
        let pong = lspd_client::rpc_at(&ep, "ping", serde_json::json!({})).unwrap();
        assert_eq!(pong, serde_json::json!("pong"));
    }

    shutdown.notify_one();
    let _ = lspd_client::rpc_at(&ep, "shutdown", serde_json::json!({}));
    let _ = handle.join();
}

/// Un método que no existe es un error **del método**, no del transporte: el daemon
/// está vivo y contesta que esa pregunta no la sabe.
#[test]
fn an_unknown_method_is_an_error_and_not_a_broken_connection() {
    let (ep, _dir) = scratch("unknown");
    let (shutdown, handle) = serve(ep.clone());
    wait_until_up(&ep);

    let err = lspd_client::rpc_at(&ep, "no-existe", serde_json::json!({})).unwrap_err();
    assert!(err.to_string().contains("no-existe") || err.to_string().to_lowercase().contains("method"),
            "el error nombra el método: {err}");

    // Y sigue vivo.
    assert!(lspd_client::rpc_at(&ep, "ping", serde_json::json!({})).is_ok());

    shutdown.notify_one();
    let _ = lspd_client::rpc_at(&ep, "shutdown", serde_json::json!({}));
    let _ = handle.join();
}

/// Sin daemon, conectar falla — y falla nombrando el endpoint, que es lo que hay que
/// mirar cuando algo no conecta.
#[test]
fn without_a_daemon_it_says_where_it_looked() {
    let (ep, _dir) = scratch("absent");
    let err = lspd_client::rpc_at(&ep, "ping", serde_json::json!({})).unwrap_err();
    assert!(format!("{err:#}").contains(&ep.to_string()), "{err:#}");
}
