//! Cómo se llega al daemon, que es lo único que cambia por sistema operativo.
//!
//! **Dos implementaciones del transporte y una sola del protocolo.** Lo de arriba
//! sólo necesita algo que sea `Read + Write`, y no sabe cuál de los dos es.
//!
//! La ruta se **deriva**: no hay flag, ni variable de entorno, ni archivo de
//! configuración. Ver `concepts/transport.md`.

use std::fmt;
use std::io::{Read, Write};
use std::path::PathBuf;

use anyhow::Result;

/// Dónde escucha el daemon, en la forma que tenga en este sistema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// Un socket Unix, que es un archivo.
    Socket(PathBuf),
    /// Un named pipe de Windows, que no lo es.
    Pipe(String),
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Endpoint::Socket(p) => write!(f, "{}", p.display()),
            Endpoint::Pipe(n)   => write!(f, "{n}"),
        }
    }
}

impl Endpoint {
    /// El archivo que hay que borrar al terminar, si el endpoint es uno.
    ///
    /// Un named pipe desaparece con el proceso; un socket Unix queda en el disco y
    /// hay que sacarlo, o el próximo arranque encuentra uno stale.
    pub fn path(&self) -> Option<&std::path::Path> {
        match self {
            Endpoint::Socket(p) => Some(p),
            Endpoint::Pipe(_)   => None,
        }
    }
}

/// El nombre de la puerta de este workspace.
///
/// **El basename para leerlo, un hash corto para distinguirlo.** El path no
/// puede ir tal cual: `sun_path` son 108 bytes en Linux, y un workspace real ya
/// son 64 — con `~/.lspd/` y `.sock` queda en ~90, y uno mas profundo lo rompe.
///
/// El hash sale del path **canonico**, asi que dos rutas que apuntan al mismo
/// lugar dan la misma puerta: un symlink no parte el daemon en dos.
///
/// Y el basename no es decoracion: sin el, `~/.lspd/` es un directorio de
/// hashes, y **un directorio de hashes no se puede mirar**.
pub fn nombre(workspace: &std::path::Path) -> String {
    let canonico = workspace.canonicalize().unwrap_or_else(|_| workspace.to_path_buf());
    let base: String = canonico
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "raiz".into())
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    format!("{base}-{:x}", huella(canonico.to_string_lossy().as_bytes()))
}

/// Una huella de 24 bits del path, en hexa.
///
/// **No hace falta que sea criptografica**: lo que decide es que dos workspaces
/// distintos den nombres distintos, y una colision aca no filtra nada — hace
/// que dos proyectos compartan daemon, que es exactamente lo que pasaba antes
/// con todos. Con esto se necesita mala suerte para volver al caso viejo, y no
/// se paga una dependencia por eso.
fn huella(bytes: &[u8]) -> u32 {
    // FNV-1a, truncado. Cabe en seis dígitos hexa y se lee.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    (h & 0xff_ffff) as u32
}

#[cfg(unix)]
pub fn endpoint(workspace: &std::path::Path) -> Endpoint {
    Endpoint::Socket(crate::dir().join(format!("{}.sock", nombre(workspace))))
}

#[cfg(windows)]
pub fn endpoint(workspace: &std::path::Path) -> Endpoint {
    Endpoint::Pipe(format!(r"\\.\pipe\lspd-{}", nombre(workspace)))
}

/// Un stream conectado al daemon.
pub trait Stream: Read + Write + Send {}
impl<T: Read + Write + Send> Stream for T {}

/// Al daemon **de este workspace**.
pub fn connect(workspace: &std::path::Path) -> Result<Box<dyn Stream>> {
    connect_to(&endpoint(workspace))
}

/// A un endpoint dado.
///
/// La derivación de [`endpoint`] es el **default** y no un hardcodeo: que exista
/// esta puerta es lo que permite levantar un daemon de prueba en su propio socket
/// sin tocar el del usuario. No es configuración — nadie la pasa en operación.
#[cfg(unix)]
pub fn connect_to(endpoint: &Endpoint) -> Result<Box<dyn Stream>> {
    use std::os::unix::net::UnixStream;
    let Endpoint::Socket(path) = endpoint else { unreachable!("en unix es un socket") };
    let stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(crate::TIMEOUT))?;
    Ok(Box::new(stream))
}

/// En Windows un named pipe se abre como un archivo.
///
/// **No lleva timeout de lectura**, a diferencia del socket: la API de archivos no
/// lo expone y ponerlo requeriría `SetCommTimeouts` sobre el handle. Un daemon que
/// acepta la conexión y no contesta deja al cliente esperando, y eso es una
/// diferencia real entre los dos transportes — anotada acá y no disimulada.
#[cfg(windows)]
pub fn connect_to(endpoint: &Endpoint) -> Result<Box<dyn Stream>> {
    use std::fs::OpenOptions;
    let Endpoint::Pipe(name) = endpoint else { unreachable!("en windows es un pipe") };
    let file = OpenOptions::new().read(true).write(true).open(name)?;
    Ok(Box::new(file))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// La ruta se deriva y no se configura: la misma regla del lado que escucha y
    /// del que llama, sin nada en el medio.
    #[test]
    fn the_endpoint_is_derived() {
        let w = std::path::Path::new("/tmp");
        let e = endpoint(w);
        assert_eq!(e, endpoint(w), "dos llamadas dan lo mismo");
        assert!(!e.to_string().is_empty());
    }

    /// **Dos workspaces, dos puertas.** Es lo que la puerta unica no permitia,
    /// y de ahi salia que un daemon ajeno contestara una negacion en vez de un
    /// "no se".
    #[test]
    fn two_workspaces_get_two_doors() {
        let a = endpoint(std::path::Path::new("/tmp"));
        let b = endpoint(std::path::Path::new("/usr"));
        assert_ne!(a, b, "dos workspaces comparten puerta");
    }

    /// Y el mismo workspace por dos rutas —un symlink— da **una** puerta: el
    /// nombre sale del path canonico, asi que el daemon no se parte en dos.
    #[test]
    fn the_same_workspace_by_another_route_is_one_door() {
        let dir = std::env::temp_dir();
        let raro = dir.join("..").join(dir.file_name().unwrap());
        assert_eq!(endpoint(&dir), endpoint(&raro), "el path no se canonicalizo");
    }

    /// El nombre se puede leer: `ls ~/.lspd/` tiene que decir de que proyecto es
    /// cada puerta, y un directorio de hashes no se puede mirar.
    #[test]
    fn the_name_carries_the_basename() {
        let n = nombre(std::path::Path::new("/tmp"));
        assert!(n.starts_with("tmp-"), "{n}");
    }

    #[cfg(unix)]
    #[test]
    fn on_unix_it_is_a_socket_under_the_lspd_dir() {
        let Endpoint::Socket(p) =
            endpoint(std::path::Path::new("/tmp")) else { panic!("en unix es un socket") };
        assert!(p.starts_with(crate::dir()), "{}", p.display());
        assert_eq!(p.extension().and_then(|e| e.to_str()), Some("sock"), "{}", p.display());
        // Y entra en `sun_path`, que son 108 bytes en Linux.
        assert!(p.to_string_lossy().len() < 108, "no entra en sun_path: {}", p.display());
        assert!(e_path_is_file_like(&p));
    }

    #[cfg(unix)]
    fn e_path_is_file_like(p: &std::path::Path) -> bool { p.is_absolute() || p.starts_with(".") }

    #[cfg(windows)]
    #[test]
    fn on_windows_it_is_a_named_pipe_and_not_a_file() {
        let Endpoint::Pipe(n) = endpoint() else { panic!("en windows es un pipe") };
        assert_eq!(n, r"\\.\pipe\lspd");
        assert!(endpoint().path().is_none(), "un pipe no deja archivo que borrar");
    }
}
