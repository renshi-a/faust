//! Where the launcher keeps everything: `~/Library/Application Support/<app id>/faust`.

use std::path::{Path, PathBuf};

/// The folders and files under the app's support folder.
#[derive(Debug, Clone)]
pub(crate) struct Paths {
    /// `~/Library/Application Support/<app id>` (`FAUST_SUPPORT_DIR` overrides it). The app
    /// may keep its own data here too; Faust uses only `faust/` inside it.
    pub(crate) support: PathBuf,
    pub(crate) faust: PathBuf,
    pub(crate) rustup_home: PathBuf,
    pub(crate) cargo_home: PathBuf,
    pub(crate) downloads: PathBuf,
    pub(crate) repo: PathBuf,
    pub(crate) versions: PathBuf,
    /// Cargo's output, shared by every version so a rebuild is quick.
    pub(crate) target: PathBuf,
    pub(crate) logs: PathBuf,
    pub(crate) state: PathBuf,
    pub(crate) devices: PathBuf,
    /// The assistant the user chose (it may hold an API key).
    pub(crate) assistant: PathBuf,
    /// Uses the developer's own rustup (`faust dev`) instead of installing one.
    pub(crate) system_rust: bool,
}

impl Paths {
    pub(crate) fn new(app_id: &str) -> Paths {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        let support = super::overrides::get("FAUST_SUPPORT_DIR")
            .map_or_else(|| home.join("Library/Application Support").join(app_id), PathBuf::from);
        let faust = support.join("faust");
        let system_rust = super::overrides::flag("FAUST_SYSTEM_RUST");
        let (rustup_home, cargo_home) = if system_rust {
            (home.join(".rustup"), home.join(".cargo"))
        } else {
            (faust.join("toolchain/rustup"), faust.join("toolchain/cargo"))
        };
        Paths {
            rustup_home,
            cargo_home,
            downloads: faust.join("toolchain/downloads"),
            repo: faust.join("repo"),
            versions: faust.join("versions"),
            target: faust.join("target"),
            logs: faust.join("logs"),
            state: faust.join("state.json"),
            devices: faust.join("devices.json"),
            assistant: faust.join("assistant.json"),
            faust,
            support,
            system_rust,
        }
    }

    pub(crate) fn version_dir(&self, id: u32) -> PathBuf {
        self.versions.join(format!("{id:04}"))
    }

    /// A log file under `logs/`, creating the folder.
    pub(crate) fn log(&self, name: &str) -> PathBuf {
        let _ = std::fs::create_dir_all(&self.logs);
        self.logs.join(name)
    }

    /// The support folder, unless it looks like something that must never be deleted.
    pub(crate) fn deletable_support(&self) -> Option<&Path> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let deep_enough = self.support.components().count() >= 4;
        (deep_enough && Some(&self.support) != home.as_ref() && self.state.is_file()).then_some(self.support.as_path())
    }

    /// `path` with the home folder written as `~`, for showing.
    pub(crate) fn tilde(path: &Path) -> String {
        let home = std::env::var("HOME").unwrap_or_default();
        let s = path.to_string_lossy();
        match s.strip_prefix(&home) {
            Some(rest) if !home.is_empty() => format!("~{rest}"),
            _ => s.into_owned(),
        }
    }
}
