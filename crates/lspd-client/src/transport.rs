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

#[cfg(unix)]
pub fn endpoint() -> Endpoint {
    Endpoint::Socket(crate::dir().join("daemon.sock"))
}

#[cfg(windows)]
pub fn endpoint() -> Endpoint {
    Endpoint::Pipe(r"\\.\pipe\lspd".to_string())
}

/// Un stream conectado al daemon.
pub trait Stream: Read + Write + Send {}
impl<T: Read + Write + Send> Stream for T {}

/// Al daemon de este sistema.
pub fn connect() -> Result<Box<dyn Stream>> {
    connect_to(&endpoint())
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
        let e = endpoint();
        assert_eq!(e, endpoint(), "dos llamadas dan lo mismo");
        assert!(!e.to_string().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn on_unix_it_is_a_socket_under_the_lspd_dir() {
        let Endpoint::Socket(p) = endpoint() else { panic!("en unix es un socket") };
        assert!(p.ends_with(".lspd/daemon.sock"), "{}", p.display());
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
