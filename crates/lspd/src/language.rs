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
}

fn is_in_path(name: &str) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&path_var).any(|dir| {
        let full = dir.join(name);
        full.is_file()
    })
}
