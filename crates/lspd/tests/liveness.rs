//! Cuándo llegó el último progreso, y el servidor cuyo proceso termina.
//!
//! **Los servidores de este archivo son de mentira, y sí terminan el handshake**:
//! leen el `initialize`, contestan con el mismo `id`, y después hacen lo suyo. El
//! `rust-analyzer` manda un `$/progress` y se queda vivo; el `jdtls` se muere un
//! momento después del handshake, que es el caso de un servidor que se cae indexando.
//! Como en `warm.rs`, el `PATH` es del proceso y se arma una sola vez.

#![cfg(unix)]

use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use lspd::lsp_manager::LspManager;

/// Lee un mensaje LSP de stdin, contesta el `initialize` con su `id`, y define `send`.
///
/// El `PATH` del test es sólo el directorio de los falsos: el script pone el suyo.
const HANDSHAKE: &str = r#"
PATH=/usr/bin:/bin
send() { printf 'Content-Length: %d\r\n\r\n%s' "${#1}" "$1"; }
len=0
while IFS= read -r line; do
  line=$(printf '%s' "$line" | tr -d '\r')
  [ -z "$line" ] && break
  case "$line" in Content-Length:*) len=$(printf '%s' "$line" | tr -dc '0-9');; esac
done
body=$(head -c "$len")
id=$(printf '%s' "$body" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
send "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"capabilities\":{}}}"
"#;

fn fake_path() -> &'static Path {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    DIR.get_or_init(|| {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = |name: &str, body: &str| {
            let p = dir.path().join(name);
            std::fs::write(&p, format!("#!/bin/sh\n{HANDSHAKE}\n{body}\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        // Reporta progreso una vez, a los 1,5 s, y se queda vivo e `INDEXING`.
        script("rust-analyzer", r#"
sleep 1.5
send '{"jsonrpc":"2.0","method":"$/progress","params":{"token":"t","value":{"kind":"report","message":"x"}}}'
exec sleep 60 </dev/null"#);
        // Termina el handshake y se muere indexando.
        script("jdtls", "sleep 0.5\nexit 0");
        // Las dos notificaciones de readiness también son progreso. Ningún servidor
        // real las manda con este nombre: el lenguaje es sólo el lugar en el mapa.
        script("typescript-language-server", r#"
sleep 1.5
send '{"jsonrpc":"2.0","method":"experimental/serverStatus","params":{"quiescent":false}}'
exec sleep 60 </dev/null"#);
        script("jedi-language-server", r#"
sleep 1.5
send '{"jsonrpc":"2.0","method":"language/status","params":{"type":"Starting","message":"x"}}'
exec sleep 60 </dev/null"#);
        std::env::set_var("PATH", dir.path());
        dir
    })
    .path()
}

fn manager() -> (tempfile::TempDir, std::sync::Arc<LspManager>) {
    fake_path();
    let ws = tempfile::tempdir().unwrap();
    let m = LspManager::new(ws.path().to_path_buf());
    (ws, m)
}

fn langs(v: &[&str]) -> Vec<String> { v.iter().map(|s| s.to_string()).collect() }

/// Calienta `lang`, cuyo servidor falso manda su señal a los 1,5 s, y comprueba que
/// `since_progress_ms` cuenta desde el arranque y que la señal lo vuelve a cero.
async fn la_senal_vuelve_a_cero(lang: &str) {
    let (_ws, m) = manager();
    assert!(m.warm(&langs(&[lang])).await[0].error.is_none());

    tokio::time::sleep(Duration::from_millis(1000)).await;
    let before = m.status().await;
    assert_eq!(before.len(), 1);
    assert!(before[0].since_progress_ms >= 900,
        "{lang}: sin señal todavía, cuenta desde el arranque: {}", before[0].since_progress_ms);

    tokio::time::sleep(Duration::from_millis(1000)).await;
    let after = m.status().await;
    assert!(after[0].since_progress_ms < 900,
        "{lang}: la señal de los 1,5 s lo vuelve a cero: {}", after[0].since_progress_ms);
}

/// **`since_progress_ms` cuenta desde el arranque, y vuelve a cero con `$/progress`.**
#[tokio::test]
async fn status_dice_hace_cuanto_llego_el_ultimo_progreso() {
    la_senal_vuelve_a_cero("rust").await;
}

/// `experimental/serverStatus` es señal de avance.
#[tokio::test]
async fn server_status_es_progreso() {
    la_senal_vuelve_a_cero("typescript").await;
}

/// `language/status` es señal de avance.
#[tokio::test]
async fn language_status_es_progreso() {
    la_senal_vuelve_a_cero("python").await;
}

/// **Por el protocolo, `status` lleva `since_progress_ms`**, y el cliente lo lee.
#[tokio::test]
async fn since_progress_ms_viaja_por_el_protocolo() {
    use std::sync::Arc;
    use tokio::sync::Notify;

    let (ws, m) = manager();
    let sock_dir = tempfile::tempdir().unwrap();
    let ep = lspd_client::Endpoint::Socket(sock_dir.path().join("daemon.sock"));

    let shutdown = Arc::new(Notify::new());
    let server = tokio::spawn({
        let ep = ep.clone();
        let s = Arc::clone(&shutdown);
        async move { let _ = lspd::ipc::serve(m, &ep, s).await; }
    });

    let ep2 = ep.clone();
    let status = tokio::task::spawn_blocking(move || {
        for _ in 0..100 {
            if lspd_client::rpc_at(&ep2, "ping", serde_json::json!({})).is_ok() { break; }
            std::thread::sleep(Duration::from_millis(20));
        }
        lspd_client::rpc_at(&ep2, "warm", serde_json::json!({"languages": ["rust"]})).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        lspd_client::rpc_at(&ep2, "status", serde_json::json!({}))
    })
    .await
    .unwrap()
    .unwrap();

    let list = lspd_client::parse_status(&status).unwrap();
    assert_eq!(list.len(), 1);
    let since = list[0].since_progress.expect("status tiene que decir since_progress_ms");
    assert!(since >= Duration::from_millis(250), "{since:?}");

    shutdown.notify_one();
    let _ = server.await;
    drop(ws);
}

/// **El servidor cuyo proceso termina después del handshake sale del mapa**: `status`
/// deja de listarlo, y el lugar se puede volver a ocupar.
#[tokio::test]
async fn un_servidor_que_muere_indexando_sale_del_mapa() {
    let (_ws, m) = manager();
    assert!(m.warm(&langs(&["java"])).await[0].error.is_none());

    let mut listed = false;
    for _ in 0..50 {
        if m.status().await.iter().any(|s| s.name == "jdtls") { listed = true; break; }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(listed, "jdtls tenía que aparecer mientras vivía");

    let mut gone = false;
    for _ in 0..300 {
        if m.status().await.is_empty() { gone = true; break; }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(gone, "el jdtls muerto sigue en el mapa: {:?}",
        m.status().await.iter().map(|s| (&s.name, &s.state)).collect::<Vec<_>>());

    assert!(m.warm(&langs(&["java"])).await[0].error.is_none(), "el lugar quedó libre");
}
