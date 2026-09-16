#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    TypeScript,
    Python,
    Java,
}

impl Language {
    /// Todos, en el orden en que se listan: el de la tabla de marcadores.
    pub const ALL: [Self; 4] = [Self::Rust, Self::Java, Self::TypeScript, Self::Python];

    /// El nombre del lenguaje, como lo escribe quien pide calentarlo.
    ///
    /// **No es el del servidor**: `rust` y no `rust-analyzer`. Quien sabe qué lenguaje
    /// tiene su workspace no tiene por qué saber qué ejecutable lo atiende.
    pub fn id(&self) -> &'static str {
        match self {
            Self::Rust       => "rust",
            Self::Java       => "java",
            Self::TypeScript => "typescript",
            Self::Python     => "python",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.id() == name)
    }

    /// Los archivos que, en la raíz de un workspace, dicen que ahí hay un proyecto de
    /// este lenguaje.
    fn markers(&self) -> &'static [&'static str] {
        match self {
            Self::Rust       => &["Cargo.toml"],
            Self::Java       => &["pom.xml", "build.gradle", "build.gradle.kts"],
            Self::TypeScript => &["package.json", "tsconfig.json"],
            Self::Python     => &["pyproject.toml", "setup.py", "requirements.txt"],
        }
    }

    /// Qué lenguajes tiene un workspace, según los marcadores de su raíz.
    ///
    /// **Sólo la raíz, y sólo archivos.** Recorrer el árbol sería lento en un repo
    /// grande, obligaría a respetar `.gitignore`, y un `.py` suelto en `scripts/`
    /// levantaría un servidor que nadie va a consultar.
    pub fn from_markers(workspace: &std::path::Path) -> Vec<Self> {
        Self::ALL
            .into_iter()
            .filter(|l| l.markers().iter().any(|m| workspace.join(m).is_file()))
            .collect()
    }

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

    /// Con qué argumentos se lanza este servidor. La tercera casilla de la tabla.
    ///
    /// **`typescript-language-server` no habla por stdio si no se lo piden**: sin
    /// `--stdio` termina apenas arranca. Los otros lo hacen solos.
    ///
    /// Y **con cuánta memoria**, que es lo que lleva `jdtls`.
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
    /// de memoria no llevan nada, y eso no es una omisión.
    pub fn spawn_args(&self) -> &'static [&'static str] {
        match self {
            // El launcher de `jdtls` pasa los suyos con `=`, y ya pone `-Xms1G` de
            // piso: 2G deja lugar para un proyecto real sin que el techo dependa de
            // en qué máquina cayó.
            Self::Java       => &["--jvm-arg=-Xmx2G"],
            Self::TypeScript => &["--stdio"],
            _                => &[],
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

    /// Si este servidor necesita el documento abierto para contestar sobre él. La
    /// cuarta casilla de la tabla.
    ///
    /// **`tsserver` arma el proyecto a partir de lo abierto**: sin `didOpen`,
    /// `prepareCallHierarchy` vuelve vacío. `jdtls` importa el proyecto entero y
    /// `rust-analyzer` carga el workspace de cargo, así que leen del disco solos.
    pub fn needs_open(&self) -> bool {
        matches!(self, Self::TypeScript)
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

/// Cuándo llegó la última señal de avance de un servidor.
///
/// **Arranca en el momento en que se crea**: un servidor que todavía no reportó nada
/// cuenta desde que arrancó, y así un arranque que nunca avanza también se ve.
/// Compartido como [`ReadinessCell`], y por la misma razón.
#[derive(Debug)]
pub struct ProgressClock {
    origin:  std::time::Instant,
    /// Milisegundos desde `origin` hasta la última señal.
    last_ms: std::sync::atomic::AtomicU64,
}

impl ProgressClock {
    pub fn new() -> Self {
        Self { origin: std::time::Instant::now(), last_ms: std::sync::atomic::AtomicU64::new(0) }
    }

    /// Llegó una señal de avance.
    pub fn touch(&self) {
        let now = self.origin.elapsed().as_millis() as u64;
        self.last_ms.store(now, std::sync::atomic::Ordering::Relaxed);
    }

    /// Hace cuánto llegó la última señal, o el arranque si no llegó ninguna.
    pub fn since(&self) -> std::time::Duration {
        let last = std::time::Duration::from_millis(
            self.last_ms.load(std::sync::atomic::Ordering::Relaxed));
        self.origin.elapsed().saturating_sub(last)
    }
}

impl Default for ProgressClock {
    fn default() -> Self { Self::new() }
}

fn is_in_path(name: &str) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&path_var).any(|dir| {
        let full = dir.join(name);
        full.is_file()
    })
}

#[cfg(test)]
mod marker_tests {
    use super::*;

    fn workspace_with(files: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for f in files {
            let path = dir.path().join(f);
            if let Some(parent) = path.parent() { std::fs::create_dir_all(parent).unwrap(); }
            std::fs::write(path, "").unwrap();
        }
        dir
    }

    #[test]
    fn cada_marcador_dice_su_lenguaje() {
        let casos: &[(&str, Language)] = &[
            ("Cargo.toml",        Language::Rust),
            ("pom.xml",           Language::Java),
            ("build.gradle",      Language::Java),
            ("build.gradle.kts",  Language::Java),
            ("package.json",      Language::TypeScript),
            ("tsconfig.json",     Language::TypeScript),
            ("pyproject.toml",    Language::Python),
            ("setup.py",          Language::Python),
            ("requirements.txt",  Language::Python),
        ];
        for (marker, lang) in casos {
            let ws = workspace_with(&[marker]);
            assert_eq!(Language::from_markers(ws.path()), vec![*lang], "{marker}");
        }
    }

    /// Varios marcadores del mismo lenguaje son **un** lenguaje: un servidor por
    /// lenguaje empieza por no pedirle dos.
    #[test]
    fn varios_lenguajes_salen_una_vez_y_en_el_orden_de_la_tabla() {
        let ws = workspace_with(&["requirements.txt", "setup.py", "package.json",
                                  "pom.xml", "build.gradle", "Cargo.toml"]);
        assert_eq!(Language::from_markers(ws.path()),
                   vec![Language::Rust, Language::Java, Language::TypeScript, Language::Python]);
    }

    #[test]
    fn sin_marcadores_no_hay_lenguajes() {
        let ws = workspace_with(&["README.md", "main.rs"]);
        assert!(Language::from_markers(ws.path()).is_empty());
    }

    /// **Sólo la raíz.** Un `.py` o un `package.json` en un subdirectorio no levanta
    /// un servidor que nadie va a consultar.
    #[test]
    fn un_marcador_en_un_subdirectorio_no_cuenta() {
        let ws = workspace_with(&["scripts/requirements.txt", "web/package.json"]);
        assert!(Language::from_markers(ws.path()).is_empty());
    }

    /// Un directorio que se llama como un marcador no es un marcador.
    #[test]
    fn un_directorio_con_nombre_de_marcador_no_cuenta() {
        let ws = tempfile::tempdir().unwrap();
        std::fs::create_dir(ws.path().join("Cargo.toml")).unwrap();
        assert!(Language::from_markers(ws.path()).is_empty());
    }

    #[test]
    fn el_nombre_de_un_lenguaje_ida_y_vuelta() {
        for lang in [Language::Rust, Language::Java, Language::TypeScript, Language::Python] {
            assert_eq!(Language::from_name(lang.id()), Some(lang));
        }
        assert_eq!(Language::from_name("cobol"), None);
        assert_eq!(Language::from_name("rust-analyzer"), None,
                   "el nombre es el del lenguaje, no el del servidor");
    }
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

    /// **`typescript-language-server` no habla por stdio si no se lo piden**: sin
    /// `--stdio` termina apenas arranca, con `required option '--stdio' not specified`.
    #[test]
    fn typescript_se_lanza_con_stdio() {
        assert_eq!(Language::TypeScript.spawn_args(), &["--stdio"]);
    }

    /// Los otros hablan por stdio sin que se lo pidan y no exponen un techo, y la fila
    /// vacía es el dato — no una fila que falta.
    #[test]
    fn los_que_no_piden_nada_se_lanzan_pelados() {
        for lang in [Language::Rust, Language::Python] {
            assert!(lang.spawn_args().is_empty(), "{lang:?} no debería llevar args");
        }
    }

    /// **`typescript-language-server` sólo conoce lo abierto**: sin `didOpen`, el
    /// `prepareCallHierarchy` vuelve vacío. Los otros leen el proyecto del disco.
    #[test]
    fn solo_typescript_necesita_el_documento_abierto() {
        assert!(Language::TypeScript.needs_open());
        for lang in [Language::Rust, Language::Java, Language::Python] {
            assert!(!lang.needs_open(), "{lang:?}");
        }
    }
}
