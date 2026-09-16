//! Esperar a que los servidores de un daemon estén listos.
//!
//! **Es mecanismo y no política**, como [`spawn`](crate::spawn): qué servidores
//! esperar, cuánto silencio tolerar y qué mostrar mientras tanto es de quien llama.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

/// Un servidor en `status`: su nombre, su readiness y hace cuánto reportó progreso.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerState {
    pub name:  String,
    pub state: String,
    /// `None` si el daemon no lo dice: uno anterior a `since_progress_ms`.
    pub since_progress: Option<Duration>,
    /// Por qué no arrancó o se cayó, en un servidor `FAILED`.
    pub error: Option<String>,
}

/// Los estados que se esperan: el handshake y la indexación. `READY` terminó, y
/// `RUNNING` no informa: no hay a qué esperar.
const WAITED: [&str; 2] = ["STARTING", "INDEXING"];

/// El servidor cuyo arranque falló o cuyo proceso terminó.
const FAILED: &str = "FAILED";

/// Cada cuánto se consulta `status`.
pub const POLL: Duration = Duration::from_millis(500);

/// Cuánto silencio se le tolera a un servidor que se espera antes de darlo por
/// estancado.
pub const STALL: Duration = Duration::from_secs(120);

/// Cómo terminó la espera.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Waited {
    /// Los que desaparecieron de `status` sin decir por qué: un daemon anterior a
    /// `FAILED`, o uno que otro arranque ya reemplazó.
    pub gone:    Vec<String>,
    /// Los que `status` muestra `FAILED`, con su error.
    pub failed:  Vec<(String, String)>,
    /// Los que siguieron `STARTING` o `INDEXING` sin reportar progreso durante la
    /// ventana.
    pub stalled: Vec<String>,
}

impl Waited {
    /// Todos listos: ninguno caído ni estancado.
    pub fn is_ok(&self) -> bool {
        self.gone.is_empty() && self.failed.is_empty() && self.stalled.is_empty()
    }
}

/// Lo que dice una foto de `status` de los servidores que se esperan.
#[derive(Debug, Default)]
struct Assessment {
    indexing: Vec<String>,
    gone:     Vec<String>,
    failed:   Vec<(String, String)>,
    stalled:  Vec<String>,
}

impl Assessment {
    fn settled(&self) -> bool { self.indexing.is_empty() }
}

fn assess(stall: Duration, servers: &[String], status: &[ServerState]) -> Assessment {
    let mut a = Assessment::default();
    for name in servers {
        match status.iter().find(|s| &s.name == name) {
            None => a.gone.push(name.clone()),
            Some(s) if s.state == FAILED => a.failed.push(
                (name.clone(), s.error.clone().unwrap_or_else(|| "sin detalle".into()))),
            Some(s) if WAITED.contains(&s.state.as_str()) => match s.since_progress {
                Some(quiet) if quiet >= stall => a.stalled.push(name.clone()),
                _                             => a.indexing.push(name.clone()),
            },
            Some(_) => {}
        }
    }
    a
}

/// Lee lo que contesta `status`.
pub fn parse_status(v: &serde_json::Value) -> Result<Vec<ServerState>> {
    let list = v.as_array().with_context(|| format!("`status` no contestó una lista: {v}"))?;
    Ok(list.iter()
        .map(|s| ServerState {
            name:  s["name"].as_str().unwrap_or("?").to_string(),
            state: s["state"].as_str().unwrap_or("?").to_string(),
            since_progress: s["since_progress_ms"].as_u64().map(Duration::from_millis),
            error: s["error"].as_str().map(str::to_string),
        })
        .collect())
}

/// Espera a que ninguno de `servers` siga `STARTING` o `INDEXING` en el daemon de este
/// workspace.
///
/// `servers` son nombres como los da `status`. `on_progress` recibe, en cada
/// consulta, el tiempo desde que empezó la espera y el estado de los que se esperan y
/// se siguen esperando.
///
/// **No hay tope total**: a un servidor que reporta progreso se lo espera lo que haga
/// falta. El que sigue esperándose y lleva `stall` sin reportarlo deja de esperarse y
/// vuelve en [`Waited::stalled`]; [`STALL`] es la ventana por defecto.
///
/// Un `Err` es que el daemon dejó de contestar. Un servidor que no arranca o cuyo
/// proceso termina no es un `Err`: vuelve en [`Waited::failed`] con su error, o en
/// [`Waited::gone`] si ya no está, y los demás se siguen esperando.
pub fn wait_ready(
    workspace:   &Path,
    servers:     &[String],
    stall:       Duration,
    on_progress: impl FnMut(Duration, &[ServerState]),
) -> Result<Waited> {
    wait_with(
        || parse_status(&crate::rpc(workspace, "status", serde_json::json!({}))?),
        servers, stall, POLL, on_progress,
    )
}

/// La espera, con la consulta a `status` y el intervalo como parámetros.
fn wait_with(
    mut fetch:       impl FnMut() -> Result<Vec<ServerState>>,
    servers:         &[String],
    stall:           Duration,
    poll:            Duration,
    mut on_progress: impl FnMut(Duration, &[ServerState]),
) -> Result<Waited> {
    let start = Instant::now();
    let mut pending: Vec<String> = servers.to_vec();
    let mut out = Waited::default();

    while !pending.is_empty() {
        let status = fetch()?;
        let a = assess(stall, &pending, &status);

        let seen: Vec<ServerState> = status.into_iter()
            .filter(|s| servers.contains(&s.name)
                && !out.gone.contains(&s.name) && !out.stalled.contains(&s.name)
                && !out.failed.iter().any(|(n, _)| n == &s.name))
            .collect();
        on_progress(start.elapsed(), &seen);

        // **Una vez caído o estancado, queda así.** Si otro lo levanta de nuevo, o
        // vuelve a avanzar, ya no es el arranque que esta espera estaba mirando.
        out.gone.extend(a.gone.iter().cloned());
        out.failed.extend(a.failed.iter().cloned());
        out.stalled.extend(a.stalled.iter().cloned());
        if a.settled() { break; }
        pending = a.indexing;

        std::thread::sleep(poll);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn st(name: &str, state: &str) -> ServerState {
        ServerState { name: name.into(), state: state.into(), since_progress: Some(Duration::ZERO), error: None }
    }

    /// Un servidor que lleva `secs` segundos sin reportar progreso.
    fn quiet(name: &str, state: &str, secs: u64) -> ServerState {
        ServerState { since_progress: Some(Duration::from_secs(secs)), ..st(name, state) }
    }

    const STALL_TEST: Duration = Duration::from_secs(60);

    fn servers(v: &[&str]) -> Vec<String> { v.iter().map(|s| s.to_string()).collect() }

    // ─── assess: una foto de `status` ────────────────────────────────────────

    #[test]
    fn ready_y_running_estan_listos_e_indexing_no() {
        let a = assess(STALL_TEST, &servers(&["rust-analyzer", "jdtls", "typescript-language-server"]), &[
            st("rust-analyzer", "READY"),
            st("jdtls", "INDEXING"),
            st("typescript-language-server", "RUNNING"),
        ]);
        assert_eq!(a.indexing, servers(&["jdtls"]));
        assert!(a.gone.is_empty());
        assert!(!a.settled());
    }

    /// **Un servidor que se esperaba y no está en `status` se cayó**: el daemon saca
    /// del mapa el arranque que falla.
    #[test]
    fn el_que_no_esta_en_status_se_cayo() {
        let a = assess(STALL_TEST, &servers(&["rust-analyzer", "jdtls"]), &[st("rust-analyzer", "READY")]);
        assert_eq!(a.gone, servers(&["jdtls"]));
        assert!(a.indexing.is_empty());
        assert!(a.settled());
    }

    /// Lo que `status` lista y nadie esperaba no cuenta: un servidor que otro levantó
    /// para una pregunta no es de esta espera.
    #[test]
    fn lo_que_no_se_espera_no_cuenta() {
        let a = assess(STALL_TEST, &servers(&["rust-analyzer"]), &[
            st("rust-analyzer", "READY"),
            st("jdtls", "INDEXING"),
        ]);
        assert!(a.settled());
    }

    // ─── wait_with: la secuencia ─────────────────────────────────────────────

    /// Una secuencia de fotos de `status`, que se repite en la última.
    fn script(seq: Vec<Vec<ServerState>>) -> impl FnMut() -> Result<Vec<ServerState>> {
        let seq = RefCell::new(seq.into_iter().collect::<std::collections::VecDeque<_>>());
        move || {
            let mut q = seq.borrow_mut();
            let snap = if q.len() > 1 { q.pop_front().unwrap() } else { q.front().unwrap().clone() };
            Ok(snap)
        }
    }

    const TICK: Duration = Duration::from_millis(1);

    #[test]
    fn espera_hasta_que_ninguno_indexa() {
        let mut vistas = 0;
        let w = wait_with(
            script(vec![
                vec![st("rust-analyzer", "INDEXING"), st("jdtls", "INDEXING")],
                vec![st("rust-analyzer", "READY"),    st("jdtls", "INDEXING")],
                vec![st("rust-analyzer", "READY"),    st("jdtls", "READY")],
            ]),
            &servers(&["rust-analyzer", "jdtls"]), STALL_TEST, TICK,
            |_, _| vistas += 1,
        ).unwrap();
        assert!(w.is_ok(), "{w:?}");
        assert_eq!(vistas, 3, "el llamador ve cada foto");
    }

    /// **`RUNNING` no se espera**: no informa readiness, así que cuenta como listo
    /// apenas aparece, y la espera vuelve en la primera foto.
    #[test]
    fn running_no_se_espera() {
        let mut vistas = 0;
        let w = wait_with(
            script(vec![vec![st("typescript-language-server", "RUNNING")]]),
            &servers(&["typescript-language-server"]), STALL_TEST, TICK,
            |_, _| vistas += 1,
        ).unwrap();
        assert!(w.is_ok());
        assert_eq!(vistas, 1);
    }

    /// **Un servidor que se cae no corta la espera de los demás**, y la espera
    /// termina mal.
    #[test]
    fn uno_que_se_cae_no_corta_a_los_demas() {
        let w = wait_with(
            script(vec![
                vec![st("rust-analyzer", "INDEXING"), st("jdtls", "INDEXING")],
                vec![st("rust-analyzer", "INDEXING")],
                vec![st("rust-analyzer", "READY")],
            ]),
            &servers(&["rust-analyzer", "jdtls"]), STALL_TEST, TICK,
            |_, _| {},
        ).unwrap();
        assert!(!w.is_ok());
        assert_eq!(w.gone, servers(&["jdtls"]));
        assert!(w.stalled.is_empty(), "rust-analyzer se siguió esperando hasta READY");
    }

    /// **Una vez caído, queda caído**: si otro lo vuelve a levantar, ya no es el
    /// arranque que esta espera estaba mirando.
    #[test]
    fn el_que_se_cayo_no_vuelve() {
        let w = wait_with(
            script(vec![
                vec![],
                vec![st("jdtls", "READY")],
            ]),
            &servers(&["jdtls"]), STALL_TEST, TICK,
            |_, _| {},
        ).unwrap();
        assert_eq!(w.gone, servers(&["jdtls"]));
    }

    /// **Un `INDEXING` sin progreso durante la ventana está estancado**, y uno que
    /// reportó hace menos no.
    #[test]
    fn indexing_sin_progreso_en_la_ventana_esta_estancado() {
        let a = assess(STALL_TEST, &servers(&["rust-analyzer", "jdtls"]), &[
            quiet("rust-analyzer", "INDEXING", 60),
            quiet("jdtls", "INDEXING", 59),
        ]);
        assert_eq!(a.stalled, servers(&["rust-analyzer"]));
        assert_eq!(a.indexing, servers(&["jdtls"]));
    }

    /// El silencio sólo cuenta indexando: `READY` terminó y `RUNNING` no informa.
    #[test]
    fn el_silencio_de_un_listo_no_es_estancamiento() {
        let a = assess(STALL_TEST, &servers(&["rust-analyzer", "pylsp"]), &[
            quiet("rust-analyzer", "READY", 600),
            quiet("pylsp", "RUNNING", 600),
        ]);
        assert!(a.stalled.is_empty());
        assert!(a.settled());
    }

    /// **Un daemon que no dice cuándo llegó el progreso no deja ver el silencio**, y
    /// con él no se corta la espera.
    #[test]
    fn sin_since_progress_no_hay_estancamiento() {
        let s = ServerState { since_progress: None, ..st("rust-analyzer", "INDEXING") };
        let a = assess(STALL_TEST, &servers(&["rust-analyzer"]), &[s]);
        assert!(a.stalled.is_empty());
        assert_eq!(a.indexing, servers(&["rust-analyzer"]));
    }

    /// **El estancado deja de esperarse, los demás no**, y la espera termina mal.
    #[test]
    fn el_estancado_no_corta_a_los_demas() {
        let w = wait_with(
            script(vec![
                vec![st("rust-analyzer", "INDEXING"), st("jdtls", "INDEXING")],
                vec![st("rust-analyzer", "INDEXING"), quiet("jdtls", "INDEXING", 60)],
                vec![st("rust-analyzer", "INDEXING"), quiet("jdtls", "INDEXING", 61)],
                vec![st("rust-analyzer", "READY"),    quiet("jdtls", "INDEXING", 62)],
            ]),
            &servers(&["rust-analyzer", "jdtls"]), STALL_TEST, TICK,
            |_, _| {},
        ).unwrap();
        assert!(!w.is_ok());
        assert_eq!(w.stalled, servers(&["jdtls"]));
        assert!(w.gone.is_empty());
    }

    /// **Mientras reporte progreso, no hay tope**: un `INDEXING` que avanza se espera
    /// aunque la espera dure mucho más que la ventana.
    #[test]
    fn mientras_avanza_no_hay_tope() {
        let mut fotos = vec![vec![st("rust-analyzer", "INDEXING")]; 50];
        fotos.push(vec![st("rust-analyzer", "READY")]);
        let w = wait_with(
            script(fotos),
            &servers(&["rust-analyzer"]), Duration::from_millis(5), TICK,
            |_, _| {},
        ).unwrap();
        assert!(w.is_ok(), "50 consultas superan la ventana, y el progreso no para: {w:?}");
    }

    /// Sin servidores que esperar no hay espera: ni una consulta.
    #[test]
    fn sin_servidores_no_consulta() {
        let w = wait_with(
            || -> Result<Vec<ServerState>> { panic!("no tenía que consultar") },
            &[], STALL_TEST, TICK, |_, _| {},
        ).unwrap();
        assert!(w.is_ok());
    }

    /// El daemon que deja de contestar es un error de la espera, no un servidor
    /// caído: no hay de quién leer el estado.
    #[test]
    fn un_daemon_que_no_contesta_es_un_error() {
        let r = wait_with(
            || -> Result<Vec<ServerState>> { anyhow::bail!("no hay daemon") },
            &servers(&["rust-analyzer"]), STALL_TEST, TICK, |_, _| {},
        );
        assert!(r.is_err());
    }

    /// **`STARTING` se espera como `INDEXING`**: en el handshake el servidor todavía no
    /// dijo nada, ni siquiera que no informa readiness.
    #[test]
    fn starting_se_espera() {
        let mut vistas = 0;
        let w = wait_with(
            script(vec![
                vec![st("typescript-language-server", "STARTING")],
                vec![st("typescript-language-server", "RUNNING")],
            ]),
            &servers(&["typescript-language-server"]), STALL_TEST, TICK,
            |_, _| vistas += 1,
        ).unwrap();
        assert!(w.is_ok(), "{w:?}");
        assert_eq!(vistas, 2, "el STARTING no cuenta como listo");
    }

    /// Un handshake que no termina también es silencio.
    #[test]
    fn un_starting_sin_progreso_en_la_ventana_esta_estancado() {
        let a = assess(STALL_TEST, &servers(&["jdtls"]), &[quiet("jdtls", "STARTING", 60)]);
        assert_eq!(a.stalled, servers(&["jdtls"]));
    }

    /// **Un `FAILED` es un servidor caído, con su porqué**, y la espera termina mal. Es
    /// el `typescript-language-server` que muere en el handshake.
    #[test]
    fn el_que_se_arranca_y_falla_vuelve_con_su_error() {
        let failed = ServerState {
            error: Some("LSP initialize: ServiceStopped".into()),
            ..st("typescript-language-server", "FAILED")
        };
        let w = wait_with(
            script(vec![
                vec![st("typescript-language-server", "STARTING")],
                vec![failed],
            ]),
            &servers(&["typescript-language-server"]), STALL_TEST, TICK,
            |_, _| {},
        ).unwrap();
        assert!(!w.is_ok());
        assert_eq!(w.failed, vec![("typescript-language-server".to_string(),
                                   "LSP initialize: ServiceStopped".to_string())]);
        assert!(w.gone.is_empty());
    }

    // ─── parse_status ────────────────────────────────────────────────────────

    #[test]
    fn status_se_lee_de_lo_que_contesta_el_daemon() {
        let v = serde_json::json!([
            {"name": "rust-analyzer", "state": "READY", "queries": 3, "since_progress_ms": 0},
            {"name": "jdtls", "state": "INDEXING", "queries": 0, "since_progress_ms": 1500},
            {"name": "pylsp", "state": "RUNNING", "queries": 0},
            {"name": "typescript-language-server", "state": "FAILED", "queries": 0,
             "since_progress_ms": 0, "error": "se cayó"},
        ]);
        assert_eq!(parse_status(&v).unwrap(), vec![
            st("rust-analyzer", "READY"),
            ServerState { since_progress: Some(Duration::from_millis(1500)), ..st("jdtls", "INDEXING") },
            ServerState { since_progress: None, ..st("pylsp", "RUNNING") },
            ServerState { error: Some("se cayó".into()), ..st("typescript-language-server", "FAILED") },
        ]);
        assert!(parse_status(&serde_json::json!({"no": "lista"})).is_err());
    }
}
