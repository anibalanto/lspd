//! `lspd` — multiplexa language servers.
//!
//! Mantiene un servidor por lenguaje vivo entre invocaciones y contesta por un
//! socket local las preguntas que necesitan un índice. **No es de nadie**: lattice
//! le pide el call graph, bilinker le va a pedir los tipos de una firma.
//!
//! Es lib además de binario para que el servidor se pueda levantar desde un test
//! sobre el transporte real, que es lo único que cambia entre los tres sistemas
//! operativos y por lo tanto lo único que hay que probar en los tres.
//!
//! La spec vive en `subsystems/lspd/`.

pub mod ipc;
pub mod language;
pub mod lsp_client;
pub mod lsp_manager;
pub mod types;
