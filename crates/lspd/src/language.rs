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
