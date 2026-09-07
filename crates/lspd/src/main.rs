//! `lspd` — multiplexa language servers.
//!
//! Mantiene un servidor por lenguaje vivo entre invocaciones y contesta por un
//! socket local las preguntas que necesitan un índice. **No es de nadie**: lattice
//! le pide el call graph, bilinker le va a pedir los tipos de una firma.
//!
//! La spec vive en `subsystems/lspd/`.

use lspd::{ipc, lsp_manager};

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use tokio::sync::Notify;

#[derive(Parser)]
#[command(name = "lspd", about = "multiplexa language servers: call graph en vivo")]
struct Args {
    #[command(subcommand)]
    cmd: Option<Cmd>,

    /// Raíz del workspace con la que se inicializan los language servers
    #[arg(long, default_value = ".", global = true)]
    workspace: PathBuf,
}

#[derive(Subcommand)]
enum Cmd {
    /// Arranca el daemon en background
    Start,
    /// Termina el daemon y los language servers que tenga vivos
    Stop,
    /// Estado del daemon y de los language servers activos
    Status,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    match args.cmd {
        // Sin subcomando **es el daemon**, no un error de uso: es como lo levanta
        // un consumidor, y lo que `start` hace es exactamente esto en background.
        None            => run(args.workspace),
        Some(Cmd::Start)  => start(&args.workspace),
        // **Con una puerta por workspace, "el daemon" es ambiguo.** Los dos
        // toman el suyo: preguntar por "el" daemon dejaria de tener sentido.
        Some(Cmd::Stop)   => stop(&args.workspace),
        Some(Cmd::Status) => status(&args.workspace),
    }
}

#[tokio::main]
async fn run(workspace: PathBuf) -> anyhow::Result<()> {
    let workspace = workspace.canonicalize().unwrap_or(workspace);

    let dir = lspd_client::dir();
    std::fs::create_dir_all(&dir)?;
    std::fs::write(lspd_client::pid_path(&workspace), std::process::id().to_string())?;

    let endpoint = lspd_client::endpoint(&workspace);
    let manager  = lsp_manager::LspManager::new(workspace.clone());
    let shutdown = Arc::new(Notify::new());

    let result = tokio::select! {
        r = ipc::serve(Arc::clone(&manager), &endpoint, Arc::clone(&shutdown)) => r,
        _ = tokio::signal::ctrl_c() => Ok(()),
    };

    manager.shutdown().await;
    // Un named pipe se va con el proceso; un socket Unix queda.
    if let Some(path) = endpoint.path() { let _ = std::fs::remove_file(path); }
    let _ = std::fs::remove_file(lspd_client::pid_path(&workspace));

    result
}

fn start(workspace: &std::path::Path) -> anyhow::Result<()> {
    if lspd_client::responds(workspace) {
        eprintln!("el daemon de este workspace ya está corriendo  pid={}",
            lspd_client::pid(workspace));
        std::process::exit(1);
    }
    let pid = lspd_client::spawn(workspace)?;
    println!("lspd started  pid={pid}  endpoint={}", lspd_client::endpoint(workspace));
    Ok(())
}

fn stop(workspace: &std::path::Path) -> anyhow::Result<()> {
    if !lspd_client::responds(workspace) {
        // **Dice de cuál**, y no "el daemon": con una puerta por workspace,
        // que no haya uno acá no dice nada de los otros.
        eprintln!("no hay daemon en {}", lspd_client::endpoint(workspace));
        std::process::exit(1);
    }
    lspd_client::rpc(workspace, "shutdown", serde_json::json!({}))?;
    println!("lspd stopped");
    Ok(())
}

fn status(workspace: &std::path::Path) -> anyhow::Result<()> {
    if !lspd_client::responds(workspace) {
        eprintln!("no hay daemon en {}", lspd_client::endpoint(workspace));
        std::process::exit(1);
    }
    println!("lspd  pid={}  endpoint={}",
        lspd_client::pid(workspace), lspd_client::endpoint(workspace));

    let servers = lspd_client::rpc(workspace, "status", serde_json::json!({}))?;
    println!("\nlanguage servers:");
    match servers.as_array() {
        Some(list) if !list.is_empty() => {
            for s in list {
                println!("  {:<28}{:<9}queries={}",
                    s["name"].as_str().unwrap_or("?"),
                    s["state"].as_str().unwrap_or("?"),
                    s["queries"]);
            }
        }
        // Se levantan por lenguaje y a demanda: ninguno todavía es normal.
        _ => println!("  (ninguno arrancado todavía)"),
    }
    Ok(())
}
