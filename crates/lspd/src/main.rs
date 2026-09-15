//! `lspd` — multiplexa language servers.
//!
//! Mantiene un servidor por lenguaje vivo entre invocaciones y contesta por un
//! socket local las preguntas que necesitan un índice. **No es de nadie**: lattice
//! le pide el call graph, bilinker le va a pedir los tipos de una firma.
//!
//! La spec vive en `subsystems/lspd/`.

use lspd::{ipc, lsp_manager};
use lspd::language::Language;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use lspd_client::ServerState;
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
    Start(StartArgs),
    /// Termina el daemon y los language servers que tenga vivos
    Stop,
    /// Estado del daemon y de los language servers activos
    Status,
}

#[derive(clap::Args)]
struct StartArgs {
    /// Toma el daemon que haya, calienta los servidores del workspace y espera a que
    /// estén listos
    #[arg(long)]
    wait: bool,

    /// Qué servidores calentar, en vez de los que dicen los marcadores del workspace
    /// (rust, java, typescript, python). Repetible
    #[arg(long, value_name = "LENGUAJE", requires = "wait", value_parser = language_name)]
    lang: Vec<String>,

    /// Tope de la espera, en segundos. Vencido, retorna 1 y el daemon queda vivo
    #[arg(long, value_name = "SEGUNDOS", requires = "wait")]
    timeout: Option<u64>,
}

fn language_name(s: &str) -> Result<String, String> {
    Language::from_name(s).map(|l| l.id().to_string()).ok_or_else(|| {
        format!("no hay language server para `{s}`: los lenguajes son {}",
            Language::ALL.map(|l| l.id()).join(", "))
    })
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    match args.cmd {
        // Sin subcomando **es el daemon**, no un error de uso: es como lo levanta
        // un consumidor, y lo que `start` hace es exactamente esto en background.
        None            => run(args.workspace),
        Some(Cmd::Start(s)) => start(&args.workspace, &s),
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

fn start(workspace: &std::path::Path, args: &StartArgs) -> anyhow::Result<()> {
    if lspd_client::responds(workspace) {
        // **Con `--wait`, uno vivo no es un error**: lo que se pide es el workspace
        // listo, y ya estar levantado es la mitad del camino.
        if !args.wait {
            eprintln!("el daemon de este workspace ya está corriendo  pid={}",
                lspd_client::pid(workspace));
            std::process::exit(1);
        }
        println!("lspd ya estaba corriendo  pid={}  endpoint={}",
            lspd_client::pid(workspace), lspd_client::endpoint(workspace));
    } else {
        let pid = lspd_client::spawn(workspace)?;
        println!("lspd started  pid={pid}  endpoint={}", lspd_client::endpoint(workspace));
    }

    if args.wait && !warm_and_wait(workspace, args)? {
        std::process::exit(1);
    }
    Ok(())
}

/// Calienta y espera. `false` si algún servidor no arrancó o si venció el tope: el
/// daemon queda vivo en los dos casos.
fn warm_and_wait(workspace: &std::path::Path, args: &StartArgs) -> anyhow::Result<bool> {
    let warmed = lspd_client::rpc(workspace, "warm",
        serde_json::json!({ "languages": args.lang }))?;
    let warmed = warmed.as_array().cloned().unwrap_or_default();

    if warmed.is_empty() {
        eprintln!("no hay marcadores de proyecto en la raíz de {}: nada que calentar",
            workspace.display());
        return Ok(true);
    }

    let mut ok = true;
    let mut servers = Vec::new();
    for w in &warmed {
        let name = w["name"].as_str().unwrap_or("?").to_string();
        match w["error"].as_str() {
            // **No corta la espera de los demás**: se dice, y se sigue.
            Some(e) => { eprintln!("  {name} no arrancó: {e}"); ok = false; }
            None    => servers.push(name),
        }
    }

    let mut progress = Progress::default();
    let waited = lspd_client::wait_ready(
        workspace, &servers, args.timeout.map(Duration::from_secs),
        |elapsed, seen| for line in progress.observe(elapsed, seen) { eprintln!("{line}") },
    )?;

    for name in &waited.gone {
        eprintln!("  {name} no arrancó: se cayó antes de terminar el handshake, y el daemon ya no lo tiene");
    }
    if waited.timed_out {
        eprintln!("venció el tope de {}s; siguen indexando: {}. El daemon queda vivo.",
            args.timeout.unwrap_or_default(), waited.indexing.join(", "));
    }
    Ok(ok && waited.is_ok())
}

/// Cada cuánto se recuerda un servidor que sigue indexando.
const HEARTBEAT: Duration = Duration::from_secs(30);

/// Qué decir del avance: una línea cuando un servidor cambia de estado, y otra cada
/// [`HEARTBEAT`] mientras siga `INDEXING`.
///
/// **Ni cada consulta ni sólo los cambios.** Una línea por consulta son cientos en
/// los minutos de un `rust-analyzer` en frío; sólo los cambios son esos mismos
/// minutos en silencio, y un silencio largo no se distingue de un cuelgue.
#[derive(Default)]
struct Progress {
    /// Por servidor: el último estado dicho, y cuándo se dijo.
    said: HashMap<String, (String, Duration)>,
}

impl Progress {
    fn observe(&mut self, elapsed: Duration, seen: &[ServerState]) -> Vec<String> {
        let mut lines = Vec::new();
        for s in seen {
            let due = match self.said.get(&s.name) {
                None => true,
                Some((state, _)) if *state != s.state => true,
                Some((_, at)) => s.state == "INDEXING" && elapsed.saturating_sub(*at) >= HEARTBEAT,
            };
            if due {
                lines.push(format!("  {:<28}{:<10}{}s", s.name, s.state, elapsed.as_secs()));
                self.said.insert(s.name.clone(), (s.state.clone(), elapsed));
            }
        }
        lines
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use lspd_client::ServerState;
    use std::time::Duration;

    fn parse(args: &[&str]) -> Result<Args, clap::Error> {
        Args::try_parse_from(std::iter::once("lspd").chain(args.iter().copied()))
    }

    #[test]
    fn start_sin_wait_es_el_de_siempre() {
        let Some(Cmd::Start(s)) = parse(&["start"]).unwrap().cmd else { panic!() };
        assert!(!s.wait);
        assert!(s.lang.is_empty());
        assert_eq!(s.timeout, None);
    }

    #[test]
    fn wait_con_lang_repetido_y_timeout() {
        let Some(Cmd::Start(s)) =
            parse(&["start", "--wait", "--lang", "rust", "--lang", "java", "--timeout", "30"])
                .unwrap().cmd else { panic!() };
        assert!(s.wait);
        assert_eq!(s.lang, vec!["rust".to_string(), "java".to_string()]);
        assert_eq!(s.timeout, Some(30));
    }

    /// Un lenguaje que no está en la tabla es un error de uso, antes de tocar el
    /// daemon.
    #[test]
    fn un_lang_desconocido_es_un_error_de_uso() {
        assert!(parse(&["start", "--wait", "--lang", "cobol"]).is_err());
    }

    /// `--lang` y `--timeout` son de la espera: sin `--wait` no dicen nada.
    #[test]
    fn lang_y_timeout_piden_wait() {
        assert!(parse(&["start", "--lang", "rust"]).is_err());
        assert!(parse(&["start", "--timeout", "10"]).is_err());
    }

    fn st(name: &str, state: &str) -> ServerState {
        ServerState { name: name.into(), state: state.into() }
    }

    const S: fn(u64) -> Duration = Duration::from_secs;

    /// **Una línea cuando cambia, y otra cada tanto mientras indexa**: siete minutos
    /// de `rust-analyzer` no son cuatrocientas líneas, ni siete minutos de silencio.
    #[test]
    fn el_avance_se_dice_cuando_cambia_y_cada_tanto() {
        let mut r = Progress::default();
        let ra = |state| vec![st("rust-analyzer", state)];

        assert_eq!(r.observe(S(0), &ra("INDEXING")).len(), 1, "el primer estado se dice");
        assert!(r.observe(S(1), &ra("INDEXING")).is_empty(), "sin cambios no se repite");
        assert!(r.observe(S(29), &ra("INDEXING")).is_empty());
        let beat = r.observe(S(30), &ra("INDEXING"));
        assert_eq!(beat.len(), 1, "cada 30 segundos se recuerda");
        assert!(beat[0].contains("30s"), "{beat:?}");
        assert!(r.observe(S(31), &ra("INDEXING")).is_empty());

        let ready = r.observe(S(40), &ra("READY"));
        assert_eq!(ready.len(), 1);
        assert!(ready[0].contains("rust-analyzer") && ready[0].contains("READY")
                && ready[0].contains("40s"), "{ready:?}");
        assert!(r.observe(S(90), &ra("READY")).is_empty(), "un listo no se recuerda");
    }
}
