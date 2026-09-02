#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    TypeScript,
    Python,
    Java,
}

impl Language {
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "rs"                    => Some(Self::Rust),
            "ts" | "tsx" | "js" | "jsx" => Some(Self::TypeScript),
            "py"                    => Some(Self::Python),
            "java"                  => Some(Self::Java),
            _                       => None,
        }
    }

    pub fn find_executable(&self) -> anyhow::Result<String> {
        let candidates: &[&str] = match self {
            Self::Rust       => &["rust-analyzer"],
            Self::TypeScript => &["typescript-language-server"],
            Self::Python     => &["jedi-language-server", "pylsp"],
            Self::Java       => &["jdtls"],
        };
        for &exe in candidates {
            if is_in_path(exe) {
                return Ok(exe.to_string());
            }
        }
        anyhow::bail!(
            "LSP for {:?} not found: install one of {:?}",
            self.name(), candidates
        )
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Rust       => "rust-analyzer",
            Self::TypeScript => "typescript-language-server",
            Self::Python     => "jedi-language-server",
            Self::Java       => "jdtls",
        }
    }

    /// Con cuánta memoria se lanza este servidor. La tercera casilla de la tabla.
    ///
    /// **Un techo que no se fija no es "sin techo": es el que el runtime del servidor
    /// calcule solo.** La JVM de `jdtls` fija su heap máximo en un cuarto de la RAM de
    /// la máquina —7,8 GB en una de 32— y arranca reservando 1 GB, y ninguno de los
    /// dos números los eligió nadie de este lado.
    ///
    /// Lo que hace daño no es el tamaño sino que **escale con la máquina**: lo que en
    /// la del que desarrolla entra justo, en la del que tiene el doble se lleva puesta
    /// la sesión. Y la máquina grande es la que nadie prueba. Un número fijo falla
    /// igual en las dos, que es lo que se le pide a un límite.
    ///
    /// Los otros tres no exponen un techo y el suyo no crece con la máquina, así que
    /// la fila va vacía y eso no es una omisión.
    pub fn spawn_args(&self) -> &'static [&'static str] {
        match self {
            // El launcher de `jdtls` pasa los suyos con `=`, y ya pone `-Xms1G` de
            // piso: 2G deja lugar para un proyecto real sin que el techo dependa de
            // en qué máquina cayó.
            Self::Java => &["--jvm-arg=-Xmx2G"],
            _          => &[],
        }
    }

    /// Lo que este servidor necesita oír en el `initialize` para arrancar de verdad.
    ///
    /// **`initializationOptions` es un campo libre de LSP**, y cada servidor pone ahí
    /// lo suyo. Es la segunda casilla de la tabla: un dato por servidor, no una
    /// decisión, así que agregar un lenguaje sigue siendo agregar una fila.
    ///
    /// `jdtls` es el que lo necesita: **lee sus raíces de acá y no del campo estándar
    /// `workspaceFolders`**. Sin esto no importa el proyecto — cae a su *proyecto
    /// invisible*, se queda sin classpath, y contesta `[]` con el servidor en `READY`.
    /// Los otros tres no piden nada.
    pub fn initialization_options(&self, workspace: &std::path::Path) -> Option<serde_json::Value> {
        match self {
            Self::Java => {
                let uri = async_lsp::lsp_types::Url::from_file_path(workspace).ok()?;
                Some(serde_json::json!({
                    "workspaceFolders": [uri.to_string()],
                    // Sin esto jdtls pregunta por progreso con requests que no
                    // implementamos y espera respuesta; declararlo en falso le dice
                    // que no las mande.
                    "extendedClientCapabilities": {
                        "progressReportProvider": false,
                        "classFileContentsSupport": false,
                    },
                }))
            }
            _ => None,
        }
    }

    /// Si este servidor avisa cuándo terminó de indexar.
    ///
    /// **Es una propiedad del servidor, no de `lspd`.** Los dos que lo hacen lo
    /// hacen con su propia extensión —`experimental/serverStatus` en rust-analyzer,
    /// `language/status` en jdtls—; los otros no mandan nada, y no hay forma de
    /// deducirlo: cronometrar el arranque sería adivinar.
    ///
    /// De acá sale el estado inicial. El que avisa nace `Indexing` y sube cuando lo
    /// dice; el que no avisa nace `Running` y se queda ahí, que es lo que este
    /// daemon podía afirmar de todos antes de que esta distinción existiera.
    pub fn reports_readiness(&self) -> bool {
        match self {
            Self::Rust | Self::Java       => true,
            Self::TypeScript | Self::Python => false,
        }
    }
}

/// Qué dice un language server de sí mismo.
///
/// Tres valores y no dos: el tercero es *"no me lo puede decir"*, y esconderlo atrás
/// de un `Ready` optimista es volver al problema con otro nombre. Ver
/// `concepts/language-servers.md` § "Un servidor que no informa su estado no se
/// puede esperar".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    /// El servidor dijo que todavía no. Una pregunta acá se contesta `-32001`.
    Indexing,
    /// El servidor dijo que sí.
    Ready,
    /// Está arriba, y este servidor no informa readiness.
    Running,
}

impl Readiness {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Indexing => "INDEXING",
            Self::Ready    => "READY",
            Self::Running  => "RUNNING",
        }
    }

    /// El estado con el que nace un servidor de este lenguaje.
    pub fn initial(lang: Language) -> Self {
        if lang.reports_readiness() { Self::Indexing } else { Self::Running }
    }

    fn code(&self) -> u8 {
        match self { Self::Indexing => 0, Self::Ready => 1, Self::Running => 2 }
    }

    fn from_code(c: u8) -> Self {
        match c { 0 => Self::Indexing, 1 => Self::Ready, _ => Self::Running }
    }
}

/// La readiness compartida entre el mainloop —que recibe las notificaciones— y el
/// cliente, que se construye después.
///
/// **Se crea antes que los dos** y se les pasa a ambos: el router del mainloop se
/// arma dentro de un closure que no puede ver un `LspClient` que todavía no existe.
#[derive(Debug)]
pub struct ReadinessCell(std::sync::atomic::AtomicU8);

impl ReadinessCell {
    pub fn new(lang: Language) -> Self {
        Self(std::sync::atomic::AtomicU8::new(Readiness::initial(lang).code()))
    }

    pub fn get(&self) -> Readiness {
        Readiness::from_code(self.0.load(std::sync::atomic::Ordering::Relaxed))
    }

    pub fn set(&self, r: Readiness) {
        self.0.store(r.code(), std::sync::atomic::Ordering::Relaxed);
    }
}

fn is_in_path(name: &str) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&path_var).any(|dir| {
        let full = dir.join(name);
        full.is_file()
    })
}

#[cfg(test)]
mod spawn_args_tests {
    use super::*;

    /// **El techo tiene que ser un número y no una fracción de la máquina.** Es la
    /// diferencia entera: una fracción hace que el mismo daemon aguante en una máquina
    /// y se lleve puesta la sesión en otra más grande, que es justo la que nadie
    /// prueba.
    #[test]
    fn java_se_lanza_con_un_techo_fijo() {
        let args = Language::Java.spawn_args();
        assert!(args.iter().any(|a| a.contains("-Xmx")),
                "jdtls sin -Xmx hereda un cuarto de la RAM de la máquina: {args:?}");
    }

    /// Los otros no exponen un techo, y la fila vacía es el dato — no una fila que
    /// falta.
    #[test]
    fn los_que_no_exponen_techo_se_lanzan_pelados() {
        for lang in [Language::Rust, Language::TypeScript, Language::Python] {
            assert!(lang.spawn_args().is_empty(), "{lang:?} no debería llevar args");
        }
    }
}
