//! `warm`, sin language servers reales.
//!
//! **Los servidores de este archivo son de mentira**: el `PATH` del proceso apunta
//! a un directorio propio, con un `rust-analyzer` que nunca contesta el handshake y
//! un `jdtls` que se muere apenas arranca. Los otros dos no están. Es el mismo
//! directorio para todos los tests del archivo —el `PATH` es del proceso, y los
//! tests corren en paralelo—, así que se arma una sola vez.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use lspd::lsp_manager::LspManager;

fn fake_path() -> &'static Path {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let script = |name: &str, body: &str| {
                let p = dir.path().join(name);
                std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            };
            // Arranca y no contesta nunca: un handshake eterno.
            // Con ruta: el `PATH` de estos tests es sólo este directorio.
            script("rust-analyzer", "exec /bin/sleep 60");
            // Arranca y se muere antes de contestar, diciendo por qué en stderr. En un
            // workspace con `.vive` arranca y no contesta nunca: el servidor arreglado.
            script("jdtls", "[ -f .vive ] && exec /bin/sleep 60\n\
                echo \"error: required option '--stdio' not specified\" >&2\n\
                exit 1");
        }
        std::env::set_var("PATH", dir.path());
        dir
    })
    .path()
}

fn workspace_with(files: &[&str]) -> tempfile::TempDir {
    fake_path();
    let dir = tempfile::tempdir().unwrap();
    for f in files { std::fs::write(dir.path().join(f), "").unwrap(); }
    dir
}

fn langs(v: &[&str]) -> Vec<String> { v.iter().map(|s| s.to_string()).collect() }

async fn status_names(m: &LspManager) -> Vec<String> {
    let mut v: Vec<String> = m.status().await.into_iter().map(|s| s.name).collect();
    v.sort();
    v
}

/// Un workspace sin marcadores no tiene nada que calentar, y `warm` no arranca nada.
#[tokio::test]
async fn sin_lenguajes_ni_marcadores_no_arranca_nada() {
    let ws = workspace_with(&["README.md"]);
    let m = LspManager::new(ws.path().to_path_buf());

    assert!(m.warm(&[]).await.is_empty());
    assert!(m.status().await.is_empty());
}

/// **El ejecutable que no está es un error de ese lenguaje**, que vuelve en la
/// respuesta con su porqué, y no deja el lugar tomado: `status` no lo lista.
#[tokio::test]
async fn un_ejecutable_que_no_esta_vuelve_con_su_error() {
    let ws = workspace_with(&[]);
    let m = LspManager::new(ws.path().to_path_buf());

    let r = m.warm(&langs(&["python"])).await;
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].name, "jedi-language-server");
    let e = r[0].error.as_deref().expect("tenía que fallar");
    assert!(e.contains("not found"), "el error dice por qué: {e}");

    assert!(m.status().await.is_empty(), "un arranque fallido no deja el lugar tomado");
}

/// Un lenguaje que no está en la tabla es un error de ese lenguaje, no de `warm`.
#[tokio::test]
async fn un_lenguaje_desconocido_vuelve_con_su_error() {
    let ws = workspace_with(&[]);
    let m = LspManager::new(ws.path().to_path_buf());

    let r = m.warm(&langs(&["cobol", "typescript"])).await;
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].name, "cobol");
    assert!(r[0].error.is_some());
    assert_eq!(r[1].name, "typescript-language-server");
    assert!(r[1].error.is_some(), "typescript-language-server no está en el PATH de prueba");
}

/// **`warm` no espera el handshake.** El `rust-analyzer` de prueba no contesta
/// nunca, y `warm` vuelve igual: el servidor queda arrancando, y `status` lo lista.
#[cfg(unix)]
#[tokio::test]
async fn warm_vuelve_sin_esperar_el_handshake() {
    let ws = workspace_with(&[]);
    let m = LspManager::new(ws.path().to_path_buf());

    let r = tokio::time::timeout(Duration::from_secs(2), m.warm(&langs(&["rust"]))).await
        .expect("warm no puede esperar un handshake que no llega");
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].name, "rust-analyzer");
    assert!(r[0].error.is_none(), "{:?}", r[0].error);

    let st = m.status().await;
    assert_eq!(st.len(), 1);
    assert_eq!(st[0].state, "STARTING", "en el handshake todavía no hay readiness");
}

/// **Un servidor en pleno handshake también dice hace cuánto**: desde que empezó a
/// arrancar, porque todavía no pudo reportar nada.
#[cfg(unix)]
#[tokio::test]
async fn en_el_handshake_el_progreso_cuenta_desde_el_arranque() {
    let ws = workspace_with(&[]);
    let m = LspManager::new(ws.path().to_path_buf());

    assert!(m.warm(&langs(&["rust"])).await[0].error.is_none());
    tokio::time::sleep(Duration::from_millis(300)).await;
    let st = m.status().await;
    assert_eq!(st.len(), 1);
    assert!(st[0].since_progress_ms >= 250, "{}", st[0].since_progress_ms);
}

/// **Uno por lenguaje, también calentando.** Dos `warm` seguidos sobre un servidor
/// que todavía no terminó el handshake encuentran el mismo lugar, y el segundo lo
/// cuenta como presente, sin arrancar otro.
#[cfg(unix)]
#[tokio::test]
async fn dos_warm_no_arrancan_dos_servidores() {
    let ws = workspace_with(&[]);
    let m = LspManager::new(ws.path().to_path_buf());

    let a = m.warm(&langs(&["rust"])).await;
    let b = m.warm(&langs(&["rust", "rust"])).await;
    assert!(a[0].error.is_none());
    assert_eq!(b.len(), 1, "un lenguaje repetido es un lenguaje");
    assert!(b[0].error.is_none());

    assert_eq!(status_names(&m).await, vec!["rust-analyzer".to_string()]);
}

/// Con `languages` vacío, los lenguajes salen de los marcadores del workspace del
/// daemon.
#[cfg(unix)]
#[tokio::test]
async fn sin_lenguajes_calienta_los_de_los_marcadores() {
    let ws = workspace_with(&["Cargo.toml", "requirements.txt"]);
    let m = LspManager::new(ws.path().to_path_buf());

    let r = m.warm(&[]).await;
    let got: Vec<(&str, bool)> = r.iter().map(|w| (w.name.as_str(), w.error.is_none())).collect();
    assert_eq!(got, vec![("rust-analyzer", true), ("jedi-language-server", false)]);
}

/// **Un proceso que se muere en el handshake libera el lugar solo**, aunque nadie
/// esté esperando su arranque: con `warm` no hay quién espere, y si el lugar quedara
/// tomado el lenguaje no se podría volver a arrancar sin reiniciar el daemon.
///
/// **Y no desaparece sin decir nada**: `status` lo muestra `FAILED`, con el error y lo
/// que el servidor dijo en stderr, hasta que otro arranque lo reemplaza.
#[cfg(unix)]
#[tokio::test]
async fn un_arranque_que_se_cae_queda_failed_y_libera_el_lugar() {
    let ws = workspace_with(&[]);
    let m = LspManager::new(ws.path().to_path_buf());

    let r = m.warm(&langs(&["java"])).await;
    assert!(r[0].error.is_none(), "el ejecutable está: el arranque empieza");

    let mut failed = None;
    for _ in 0..100 {
        if let Some(s) = m.status().await.into_iter().find(|s| s.state == "FAILED") {
            failed = Some(s);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let failed = failed.expect("el jdtls que se murió tiene que figurar FAILED");
    assert_eq!(failed.name, "jdtls");
    let e = failed.error.expect("FAILED lleva su error");
    assert!(e.contains("required option '--stdio' not specified"), "con su stderr: {e}");

    // Y el lugar se puede volver a ocupar, y el arranque nuevo reemplaza al FAILED.
    std::fs::write(ws.path().join(".vive"), "").unwrap();
    let again = m.warm(&langs(&["java"])).await;
    assert!(again[0].error.is_none());
    let st = m.status().await;
    assert_eq!(st.len(), 1, "{:?}", st.iter().map(|s| (&s.name, &s.state)).collect::<Vec<_>>());
    assert_eq!(st[0].state, "STARTING");
    assert!(st[0].error.is_none());
}

/// Por el protocolo: `warm` es un método, y su resultado es `[{name, error}]`.
#[tokio::test]
async fn warm_por_el_protocolo() {
    use std::sync::Arc;
    use tokio::sync::Notify;

    let ws = workspace_with(&[]);
    let sock_dir = tempfile::tempdir().unwrap();
    let ep = if cfg!(windows) {
        lspd_client::Endpoint::Pipe(format!(r"\\.\pipe\lspd-test-warm-{}", std::process::id()))
    } else {
        lspd_client::Endpoint::Socket(PathBuf::from(sock_dir.path()).join("daemon.sock"))
    };

    let shutdown = Arc::new(Notify::new());
    let manager = LspManager::new(ws.path().to_path_buf());
    let server = tokio::spawn({
        let ep = ep.clone();
        let s = Arc::clone(&shutdown);
        async move { let _ = lspd::ipc::serve(manager, &ep, s).await; }
    });

    let ep2 = ep.clone();
    let result = tokio::task::spawn_blocking(move || {
        for _ in 0..100 {
            if lspd_client::rpc_at(&ep2, "ping", serde_json::json!({})).is_ok() { break; }
            std::thread::sleep(Duration::from_millis(20));
        }
        lspd_client::rpc_at(&ep2, "warm", serde_json::json!({"languages": ["python"]}))
    })
    .await
    .unwrap()
    .unwrap();

    let list = result.as_array().expect("una lista");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["name"], "jedi-language-server");
    assert!(list[0]["error"].is_string());

    shutdown.notify_one();
    let _ = server.await;
}
