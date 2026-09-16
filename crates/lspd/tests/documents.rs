//! El documento abierto antes de preguntar, y la posición sin ítem de call hierarchy.
//!
//! **El servidor de este archivo es de mentira**: un `typescript-language-server` que
//! anota en `lsp.log`, en el workspace, el método de cada mensaje que le llega, y
//! contesta `null` a toda request salvo el `initialize`. Un `prepareCallHierarchy`
//! que vuelve `null` es lo que contesta `tsserver` sobre un documento que no conoce.
//! Como en `warm.rs`, el `PATH` es del proceso y se arma una sola vez.

#![cfg(unix)]

use std::path::Path;
use std::sync::OnceLock;

use lspd::lsp_manager::LspManager;

const SERVER: &str = r#"#!/bin/sh
PATH=/usr/bin:/bin
send() { printf 'Content-Length: %d\r\n\r\n%s' "${#1}" "$1"; }
while :; do
  len=0
  while IFS= read -r line; do
    line=$(printf '%s' "$line" | tr -d '\r')
    [ -z "$line" ] && break
    case "$line" in Content-Length:*) len=$(printf '%s' "$line" | tr -dc '0-9');; esac
  done
  [ "$len" = 0 ] && exit 0
  body=$(head -c "$len")
  method=$(printf '%s' "$body" | sed -n 's/.*"method":"\([^"]*\)".*/\1/p')
  printf '%s\n' "$method" >> lsp.log
  id=$(printf '%s' "$body" | sed -n 's/^{"jsonrpc":"2.0","id":\([0-9]*\),.*/\1/p')
  [ -z "$id" ] && id=$(printf '%s' "$body" | sed -n 's/.*,"id":\([0-9]*\)}$/\1/p')
  [ -z "$id" ] && continue
  case "$method" in
    initialize) send "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"capabilities\":{}}}";;
    *)          send "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":null}";;
  esac
done
"#;

fn fake_path() -> &'static Path {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    DIR.get_or_init(|| {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("typescript-language-server");
        std::fs::write(&p, SERVER).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_var("PATH", dir.path());
        dir
    })
    .path()
}

fn log(ws: &Path) -> Vec<String> {
    std::fs::read_to_string(ws.join("lsp.log")).unwrap_or_default()
        .lines().filter(|l| !l.is_empty()).map(str::to_string).collect()
}

fn count(log: &[String], method: &str) -> usize {
    log.iter().filter(|m| *m == method).count()
}

/// **El documento se abre antes de preguntar**, se actualiza cuando cambia en disco,
/// y no se vuelve a mandar si no cambió.
#[tokio::test]
async fn el_documento_se_abre_y_sigue_al_disco() {
    fake_path();
    let ws = tempfile::tempdir().unwrap();
    let file = ws.path().join("a.ts");
    std::fs::write(&file, "export function f() {}\n").unwrap();
    let m = LspManager::new(ws.path().to_path_buf());
    let file = file.to_string_lossy().to_string();

    let _ = m.callers(&file, 0, 16).await;
    let l = log(ws.path());
    let open = l.iter().position(|m| m == "textDocument/didOpen").expect("se abrió");
    let prepare = l.iter().position(|m| m == "textDocument/prepareCallHierarchy").expect("se preguntó");
    assert!(open < prepare, "se abre antes de preguntar: {l:?}");

    let _ = m.callees(&file, 0, 16).await;
    let l = log(ws.path());
    assert_eq!(count(&l, "textDocument/didOpen"), 1, "abierto una vez: {l:?}");
    assert_eq!(count(&l, "textDocument/didChange"), 0, "sin cambios no se manda: {l:?}");

    std::fs::write(&file, "export function f() { g() }\n").unwrap();
    let _ = m.callers(&file, 0, 16).await;
    let l = log(ws.path());
    assert_eq!(count(&l, "textDocument/didChange"), 1, "el cambio en disco se manda: {l:?}");
    let change = l.iter().position(|m| m == "textDocument/didChange").unwrap();
    let last_prepare = l.iter().rposition(|m| m == "textDocument/prepareCallHierarchy").unwrap();
    assert!(change < last_prepare, "antes de preguntar: {l:?}");
}

/// **Un `prepareCallHierarchy` sin ítem es un error, y no `[]`**: `[]` diría que nadie
/// llama a un símbolo que el servidor no encontró.
#[tokio::test]
async fn sin_item_de_call_hierarchy_es_un_error() {
    fake_path();
    let ws = tempfile::tempdir().unwrap();
    let file = ws.path().join("a.ts");
    std::fs::write(&file, "const x = 1;\n").unwrap();
    let m = LspManager::new(ws.path().to_path_buf());
    let file = file.to_string_lossy().to_string();

    for r in [m.callers(&file, 0, 6).await, m.callees(&file, 0, 6).await] {
        let e = r.expect_err("sin ítem no hay `[]`");
        assert!(e.downcast_ref::<lspd::lsp_client::NoCallHierarchy>().is_some(), "{e}");
    }
}
