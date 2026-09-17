//! Los documentos abiertos en un servidor, y qué hay que mandarle para que estén al
//! día con el disco.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::Path;

use async_lsp::lsp_types::Url;

/// Qué hay que mandarle al servidor para que un documento esté como en disco.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocSync {
    /// Nunca se abrió: `didOpen`, con esta versión.
    Open { version: i32 },
    /// Se abrió y cambió: `didChange` con el texto entero, con esta versión.
    Change { version: i32 },
    /// Lo que tiene el servidor es lo que hay en disco.
    Current,
}

/// Lo que se le mandó al servidor de cada documento: su versión y el hash de su texto.
///
/// **Se guarda el hash y no el texto**: alcanza para saber si cambió, y lo abierto
/// no crece con el tamaño de los archivos.
#[derive(Debug, Default)]
pub struct OpenDocuments(HashMap<Url, (i32, u64)>);

impl OpenDocuments {
    /// Anota que el servidor va a tener `text` en `uri`, y dice qué mandarle.
    pub fn sync(&mut self, uri: &Url, text: &str) -> DocSync {
        let hash = {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            text.hash(&mut h);
            h.finish()
        };
        match self.0.get_mut(uri) {
            None => {
                self.0.insert(uri.clone(), (1, hash));
                DocSync::Open { version: 1 }
            }
            Some((_, sent)) if *sent == hash => DocSync::Current,
            Some((version, sent)) => {
                *version += 1;
                *sent = hash;
                DocSync::Change { version: *version }
            }
        }
    }
}

impl OpenDocuments {
    /// Los documentos abiertos.
    pub fn uris(&self) -> Vec<Url> {
        self.0.keys().cloned().collect()
    }

    /// Olvida un documento que se cerró: si se vuelve a pedir, se abre de cero.
    pub fn forget(&mut self, uri: &Url) {
        self.0.remove(uri);
    }
}

/// El `languageId` de LSP de un archivo, por su extensión.
pub fn language_id(file: &Path) -> &'static str {
    match file.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "tsx" => "typescriptreact",
        "js"  => "javascript",
        "jsx" => "javascriptreact",
        "rs"  => "rust",
        "py"  => "python",
        "java" => "java",
        _     => "typescript",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_lsp::lsp_types::Url;

    fn uri(p: &str) -> Url { Url::from_file_path(p).unwrap() }

    /// **La primera vez se abre**, con la versión 1.
    #[test]
    fn la_primera_vez_se_abre() {
        let mut d = OpenDocuments::default();
        assert_eq!(d.sync(&uri("/ws/a.ts"), "x"), DocSync::Open { version: 1 });
    }

    /// Un documento que no cambió no se vuelve a mandar.
    #[test]
    fn sin_cambios_no_se_manda_nada() {
        let mut d = OpenDocuments::default();
        d.sync(&uri("/ws/a.ts"), "x");
        assert_eq!(d.sync(&uri("/ws/a.ts"), "x"), DocSync::Current);
    }

    /// **Lo que cambió en disco se manda, con la versión siguiente**: la respuesta no
    /// puede salir de un texto viejo.
    #[test]
    fn lo_que_cambio_se_manda_con_la_version_siguiente() {
        let mut d = OpenDocuments::default();
        d.sync(&uri("/ws/a.ts"), "x");
        assert_eq!(d.sync(&uri("/ws/a.ts"), "y"), DocSync::Change { version: 2 });
        assert_eq!(d.sync(&uri("/ws/a.ts"), "y"), DocSync::Current);
        assert_eq!(d.sync(&uri("/ws/a.ts"), "x"), DocSync::Change { version: 3 },
                   "volver al texto de antes también es un cambio");
    }

    /// Cada documento lleva su versión.
    #[test]
    fn cada_documento_es_independiente() {
        let mut d = OpenDocuments::default();
        d.sync(&uri("/ws/a.ts"), "x");
        d.sync(&uri("/ws/a.ts"), "y");
        assert_eq!(d.sync(&uri("/ws/b.ts"), "y"), DocSync::Open { version: 1 });
    }

    /// Los abiertos se pueden recorrer, y uno olvidado se vuelve a abrir desde cero.
    #[test]
    fn un_documento_olvidado_se_vuelve_a_abrir() {
        let mut d = OpenDocuments::default();
        d.sync(&uri("/ws/a.ts"), "x");
        d.sync(&uri("/ws/a.ts"), "y");
        d.sync(&uri("/ws/b.ts"), "x");
        let mut abiertos = d.uris();
        abiertos.sort();
        assert_eq!(abiertos, vec![uri("/ws/a.ts"), uri("/ws/b.ts")]);

        d.forget(&uri("/ws/a.ts"));
        assert_eq!(d.uris(), vec![uri("/ws/b.ts")]);
        assert_eq!(d.sync(&uri("/ws/a.ts"), "y"), DocSync::Open { version: 1 });
    }

    #[test]
    fn el_language_id_sale_de_la_extension() {
        assert_eq!(language_id(std::path::Path::new("/ws/a.ts")),  "typescript");
        assert_eq!(language_id(std::path::Path::new("/ws/a.tsx")), "typescriptreact");
        assert_eq!(language_id(std::path::Path::new("/ws/a.js")),  "javascript");
        assert_eq!(language_id(std::path::Path::new("/ws/a.jsx")), "javascriptreact");
    }
}
