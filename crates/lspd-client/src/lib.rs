//! El cliente de `lspd`, compartido.
//!
//! **Es un crate y no un módulo de nadie** porque hay más de un consumidor: lattice
//! le pide el call graph, bilinker le va a pedir los tipos de una firma. Un cliente
//! por consumidor sería el mismo protocolo escrito dos veces — y ya pasó: antes de
//! que existiera un cliente único, bilinker tenía dos implementaciones del mismo
//! `rpc` en dos archivos.
//!
//! Habla JSON-RPC 2.0 con framing por líneas sobre el [transporte](endpoint) que
//! corresponda al sistema operativo, y no expone cuál es.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};

mod transport;
pub use transport::{connect, connect_to, Endpoint};

/// El directorio del daemon. Se **deriva**: no hay nada que configurar.
pub fn dir() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".lspd")
}

/// Dónde escucha el daemon en este sistema. Ver `concepts/transport.md`.
pub fn endpoint() -> Endpoint {
    transport::endpoint()
}

/// El archivo con el pid del daemon.
///
/// **No es cómo se sabe si está vivo** — para eso está [`responds`]. Es para poder
/// decir *qué* proceso es cuando ya se sabe que sí.
pub fn pid_path() -> PathBuf {
    dir().join("daemon.pid")
}

pub fn pid() -> u32 {
    std::fs::read_to_string(pid_path())
        .ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0)
}

/// Cuánto se espera una respuesta antes de darla por perdida.
pub const TIMEOUT: Duration = Duration::from_secs(5);

/// Una pregunta y su respuesta.
///
/// Un `Err` acá es **o** que no se pudo llegar al daemon **o** que el daemon
/// contestó con `error`. Las dos son fallas de la consulta desde donde está parado
/// quien pregunta; distinguirlas es [`responds`], que es una pregunta aparte.
pub fn rpc(method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
    rpc_at(&endpoint(), method, params)
}

/// La misma pregunta, contra un endpoint dado. Ver [`connect_to`].
pub fn rpc_at(ep: &Endpoint, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
    let mut stream = connect_to(ep).with_context(|| format!("no hay daemon en {ep}"))?;

    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });
    let line = serde_json::to_string(&req)? + "\n";
    stream.write_all(line.as_bytes())?;
    stream.flush()?;

    let mut resp_line = String::new();
    BufReader::new(stream).read_line(&mut resp_line)?;

    let resp: serde_json::Value = serde_json::from_str(resp_line.trim())
        .with_context(|| format!("respuesta que no es JSON: {resp_line:?}"))?;
    if let Some(err) = resp.get("error") {
        anyhow::bail!("{}", err["message"].as_str().unwrap_or("unknown error"));
    }
    Ok(resp["result"].clone())
}

/// ¿Hay daemon?
///
/// Es un `ping`, y **contesta que sí antes de que los language servers estén
/// listos**. Eso es del protocolo y no un descuido: quien pregunte tiene que
/// distinguir *"todavía no sé"* de *"no hay"*, y juntarlas en este booleano sería
/// decidir por él.
pub fn responds() -> bool {
    rpc("ping", serde_json::json!({}))
        .map(|v| v == serde_json::json!("pong"))
        .unwrap_or(false)
}

/// Dónde está el binario del daemon.
///
/// Primero al lado del ejecutable actual —el caso de un build local, donde los dos
/// salen del mismo `target/`— y si no, en PATH.
pub fn binary() -> PathBuf {
    const NAME: &str = if cfg!(windows) { "lspd.exe" } else { "lspd" };
    std::env::current_exe().ok()
        .and_then(|p| p.parent().map(|d| d.join(NAME)))
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from(NAME))
}

/// Lo levanta en background y espera a que conteste.
///
/// **Cuándo llamarlo es del consumidor, no de acá.** Lattice lo hace apenas el
/// proveedor `lsp` hace falta; bilinker no lo hace nunca y degrada a *no
/// verificado*. Lo que esta función aporta es el mecanismo, no la política.
pub fn spawn(workspace: &std::path::Path) -> Result<u32> {
    let bin = binary();
    let child = std::process::Command::new(&bin)
        .arg("--workspace").arg(workspace)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("no se pudo arrancar el daemon ({}): {e}", bin.display()))?;

    // Esperar a que conteste, y no sólo a que el proceso exista: reportar
    // "arrancado" sobre un proceso que murió en el handshake es peor que esperar.
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(100));
        if responds() { return Ok(child.id()); }
    }
    anyhow::bail!("el daemon no respondió en 5s")
}
