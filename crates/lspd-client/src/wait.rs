//! Esperar a que los servidores de un daemon estén listos.
//!
//! **Es mecanismo y no política**, como [`spawn`](crate::spawn): qué servidores
//! esperar, con qué tope y qué mostrar mientras tanto es de quien llama.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

/// Un servidor en `status`: su nombre y su readiness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerState {
    pub name:  String,
    pub state: String,
}

/// El único estado que se espera. `READY` terminó, y `RUNNING` no informa: no hay a
/// qué esperar.
const INDEXING: &str = "INDEXING";

/// Cada cuánto se consulta `status`.
pub const POLL: Duration = Duration::from_millis(500);

/// Cómo terminó la espera.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Waited {
    /// Los que desaparecieron de `status`: el daemon saca del mapa el arranque que
    /// falla.
    pub gone:      Vec<String>,
    /// Los que seguían indexando cuando venció el tope. Vacío si no venció.
    pub indexing:  Vec<String>,
    pub timed_out: bool,
}

impl Waited {
    /// Todos listos, ninguno caído, y sin tope vencido.
    pub fn is_ok(&self) -> bool {
        self.gone.is_empty() && !self.timed_out
    }
}

/// Lo que dice una foto de `status` de los servidores que se esperan.
#[derive(Debug, Default)]
struct Assessment {
    indexing: Vec<String>,
    gone:     Vec<String>,
}

impl Assessment {
    fn settled(&self) -> bool { self.indexing.is_empty() }
}

fn assess(servers: &[String], status: &[ServerState]) -> Assessment {
    let mut a = Assessment::default();
    for name in servers {
        match status.iter().find(|s| &s.name == name) {
            None                         => a.gone.push(name.clone()),
            Some(s) if s.state == INDEXING => a.indexing.push(name.clone()),
            Some(_)                      => {}
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
        })
        .collect())
}

/// Espera a que ninguno de `servers` siga `INDEXING` en el daemon de este workspace.
///
/// `servers` son nombres como los da `status`. `on_progress` recibe, en cada
/// consulta, el tiempo desde que empezó la espera y el estado de los que se esperan y
/// siguen en el daemon. Sin `timeout` se espera lo que haga falta.
///
/// Un `Err` es que el daemon dejó de contestar. Un servidor que no arranca no es un
/// `Err`: vuelve en [`Waited::gone`], y los demás se siguen esperando.
pub fn wait_ready(
    workspace:   &Path,
    servers:     &[String],
    timeout:     Option<Duration>,
    on_progress: impl FnMut(Duration, &[ServerState]),
) -> Result<Waited> {
    wait_with(
        || parse_status(&crate::rpc(workspace, "status", serde_json::json!({}))?),
        servers, timeout, POLL, on_progress,
    )
}

/// La espera, con la consulta a `status` y el intervalo como parámetros.
fn wait_with(
    mut fetch:       impl FnMut() -> Result<Vec<ServerState>>,
    servers:         &[String],
    timeout:         Option<Duration>,
    poll:            Duration,
    mut on_progress: impl FnMut(Duration, &[ServerState]),
) -> Result<Waited> {
    let start = Instant::now();
    let mut pending: Vec<String> = servers.to_vec();
    let mut out = Waited::default();

    while !pending.is_empty() {
        let status = fetch()?;
        let a = assess(&pending, &status);

        let seen: Vec<ServerState> = status.into_iter()
            .filter(|s| servers.contains(&s.name) && !out.gone.contains(&s.name))
            .collect();
        on_progress(start.elapsed(), &seen);

        // **Una vez caído, queda caído.** Si otro lo levanta de nuevo, ya no es el
        // arranque que esta espera estaba mirando.
        out.gone.extend(a.gone.iter().cloned());
        if a.settled() { break; }
        pending = a.indexing;

        let mut nap = poll;
        if let Some(t) = timeout {
            let left = t.saturating_sub(start.elapsed());
            if left.is_zero() {
                out.timed_out = true;
                out.indexing  = pending;
                break;
            }
            nap = nap.min(left);
        }
        std::thread::sleep(nap);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn st(name: &str, state: &str) -> ServerState {
        ServerState { name: name.into(), state: state.into() }
    }

    fn servers(v: &[&str]) -> Vec<String> { v.iter().map(|s| s.to_string()).collect() }

    // ─── assess: una foto de `status` ────────────────────────────────────────

    #[test]
    fn ready_y_running_estan_listos_e_indexing_no() {
        let a = assess(&servers(&["rust-analyzer", "jdtls", "typescript-language-server"]), &[
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
        let a = assess(&servers(&["rust-analyzer", "jdtls"]), &[st("rust-analyzer", "READY")]);
        assert_eq!(a.gone, servers(&["jdtls"]));
        assert!(a.indexing.is_empty());
        assert!(a.settled());
    }

    /// Lo que `status` lista y nadie esperaba no cuenta: un servidor que otro levantó
    /// para una pregunta no es de esta espera.
    #[test]
    fn lo_que_no_se_espera_no_cuenta() {
        let a = assess(&servers(&["rust-analyzer"]), &[
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
            &servers(&["rust-analyzer", "jdtls"]), None, TICK,
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
            &servers(&["typescript-language-server"]), None, TICK,
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
            &servers(&["rust-analyzer", "jdtls"]), None, TICK,
            |_, _| {},
        ).unwrap();
        assert!(!w.is_ok());
        assert_eq!(w.gone, servers(&["jdtls"]));
        assert!(w.indexing.is_empty(), "rust-analyzer se siguió esperando hasta READY");
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
            &servers(&["jdtls"]), None, TICK,
            |_, _| {},
        ).unwrap();
        assert_eq!(w.gone, servers(&["jdtls"]));
    }

    /// **El tope corta la espera**, y dice quiénes seguían indexando.
    #[test]
    fn el_tope_corta_y_dice_quien_seguia() {
        let w = wait_with(
            script(vec![vec![st("rust-analyzer", "INDEXING"), st("jdtls", "READY")]]),
            &servers(&["rust-analyzer", "jdtls"]), Some(Duration::from_millis(30)), TICK,
            |_, _| {},
        ).unwrap();
        assert!(!w.is_ok());
        assert!(w.timed_out);
        assert_eq!(w.indexing, servers(&["rust-analyzer"]));
    }

    /// Sin servidores que esperar no hay espera: ni una consulta.
    #[test]
    fn sin_servidores_no_consulta() {
        let w = wait_with(
            || -> Result<Vec<ServerState>> { panic!("no tenía que consultar") },
            &[], None, TICK, |_, _| {},
        ).unwrap();
        assert!(w.is_ok());
    }

    /// El daemon que deja de contestar es un error de la espera, no un servidor
    /// caído: no hay de quién leer el estado.
    #[test]
    fn un_daemon_que_no_contesta_es_un_error() {
        let r = wait_with(
            || -> Result<Vec<ServerState>> { anyhow::bail!("no hay daemon") },
            &servers(&["rust-analyzer"]), None, TICK, |_, _| {},
        );
        assert!(r.is_err());
    }

    // ─── parse_status ────────────────────────────────────────────────────────

    #[test]
    fn status_se_lee_de_lo_que_contesta_el_daemon() {
        let v = serde_json::json!([
            {"name": "rust-analyzer", "state": "READY", "queries": 3},
            {"name": "jdtls", "state": "INDEXING", "queries": 0},
        ]);
        assert_eq!(parse_status(&v).unwrap(),
                   vec![st("rust-analyzer", "READY"), st("jdtls", "INDEXING")]);
        assert!(parse_status(&serde_json::json!({"no": "lista"})).is_err());
    }
}
