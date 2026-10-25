//! `faust.toml`: what Faust needs to know about an app. It lives at the root of the app's
//! repository, next to `Cargo.toml`.
//!
//! ```toml
//! [app]
//! name = "Hello"
//! id = "com.example.hello"
//!
//! [toolchain]
//! rust = "1.96.1"
//! ```
//!
//! The copy inside a packaged app decides who the app is (its name, id and toolchain). The
//! copy in a version's source decides how that version is built and checked, so a change the
//! user asks for may change it too. Nothing that protects the user is read from it: those
//! rules live in the signed launcher.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

/// The file name, at the root of the app's repository.
pub(crate) const FILE: &str = "faust.toml";

/// The Rust toolchain apps get when `faust.toml` names none.
pub(crate) const DEFAULT_RUST: &str = "1.96.1";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    pub(crate) app: App,
    #[serde(default)]
    pub(crate) toolchain: Toolchain,
    #[serde(default)]
    pub(crate) check: Check,
    #[serde(default)]
    pub(crate) repair: Repair,
    #[serde(default)]
    pub(crate) package: Package,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct App {
    /// Shown everywhere: the bundle's name, the windows' titles.
    pub(crate) name: String,
    /// The bundle identifier, such as `com.example.hello`. Faust's state lives in
    /// `~/Library/Application Support/<id>/faust`.
    pub(crate) id: String,
    /// The Cargo binary to bundle. Defaults to the package's name.
    pub(crate) binary: Option<String>,
    /// Defaults to the package's version in `Cargo.toml`.
    pub(crate) version: Option<String>,
    /// One or two sentences for the installer's welcome page.
    #[serde(default)]
    pub(crate) description: String,
    /// The same, in other languages: `{ ja = "…", de = "…" }`.
    #[serde(default)]
    pub(crate) descriptions: BTreeMap<String, String>,
    /// An `.icns` file, relative to the repository.
    pub(crate) icon: Option<PathBuf>,
    /// Files and folders copied into `Contents/Resources`.
    #[serde(default)]
    pub(crate) resources: Vec<PathBuf>,
    #[serde(default = "default_minimum_macos")]
    pub(crate) minimum_macos: String,
    /// Extra `Info.plist` keys (strings, booleans and integers).
    #[serde(default)]
    pub(crate) info_plist: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Toolchain {
    /// A pinned Rust release, installed on the user's Mac with rustup.
    pub(crate) rust: String,
}

impl Default for Toolchain {
    fn default() -> Toolchain {
        Toolchain { rust: DEFAULT_RUST.into() }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Check {
    /// Run `cargo test` before a version may be used.
    #[serde(default = "yes")]
    pub(crate) tests: bool,
    /// Arguments that make the app check itself and exit (0 means good), such as
    /// `["--selftest"]`. Without them there is no self-test.
    #[serde(default)]
    pub(crate) selftest: Vec<String>,
    /// The app calls `faust_link`'s `ready()` once its first window is up. When false (an app
    /// that doesn't use faust-link), it only has to keep running for `healthy_after_secs`.
    #[serde(default = "yes")]
    pub(crate) ready_signal: bool,
    /// How long a new version may take to call `ready()` before it is stopped.
    #[serde(default = "default_ready_timeout")]
    pub(crate) ready_timeout_secs: u64,
    /// How long it must keep running after that to count as good.
    #[serde(default = "default_healthy_after")]
    pub(crate) healthy_after_secs: u64,
}

impl Default for Check {
    fn default() -> Check {
        Check {
            tests: true,
            selftest: Vec::new(),
            ready_signal: true,
            ready_timeout_secs: default_ready_timeout(),
            healthy_after_secs: default_healthy_after(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Repair {
    /// The rules the assistant reads before it changes anything.
    #[serde(default = "default_rules")]
    pub(crate) rules: PathBuf,
    /// Paths (files, or folders ending in `/`) the assistant must leave as they are.
    #[serde(default)]
    pub(crate) protect: Vec<String>,
}

impl Default for Repair {
    fn default() -> Repair {
        Repair { rules: default_rules(), protect: Vec::new() }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Package {
    /// Paths left out of the source that ships inside the app (besides ignored files).
    #[serde(default)]
    pub(crate) exclude: Vec<String>,
}

fn yes() -> bool {
    true
}
fn default_minimum_macos() -> String {
    "11.0".into()
}
fn default_ready_timeout() -> u64 {
    60
}
fn default_healthy_after() -> u64 {
    8
}
fn default_rules() -> PathBuf {
    "FAUST.md".into()
}

/// What `Cargo.toml` says about the package that holds the binary.
#[derive(Debug, Deserialize)]
struct CargoManifest {
    package: Option<CargoPackage>,
}

#[derive(Debug, Deserialize)]
struct CargoPackage {
    name: String,
    version: Option<toml::Value>,
}

impl Config {
    /// Reads `faust.toml` from the root of an app's repository.
    pub(crate) fn load(root: &Path) -> Result<Config> {
        let path = root.join(FILE);
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        Config::parse(&text).with_context(|| format!("in {}", path.display()))
    }

    pub(crate) fn parse(text: &str) -> Result<Config> {
        let config: Config = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        let app = &self.app;
        ensure!(!app.name.trim().is_empty(), "app.name is empty");
        ensure!(is_file_name(&app.name), "app.name must not contain '/' or start with '.': {:?}", app.name);
        ensure!(
            !app.id.is_empty()
                && app
                    .id
                    .split('.')
                    .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')),
            "app.id must look like com.example.app: {:?}",
            app.id
        );
        if let Some(binary) = &app.binary {
            ensure!(is_file_name(binary), "app.binary must be a plain name: {binary:?}");
        }
        for (key, value) in &app.info_plist {
            if !matches!(value, toml::Value::String(_) | toml::Value::Boolean(_) | toml::Value::Integer(_)) {
                bail!("app.info_plist.{key}: only strings, booleans and integers are supported");
            }
        }
        ensure!(
            self.toolchain.rust.chars().all(|c| c.is_ascii_alphanumeric() || ".-".contains(c))
                && !self.toolchain.rust.is_empty(),
            "toolchain.rust must be a release such as 1.96.1: {:?}",
            self.toolchain.rust
        );
        Ok(())
    }

    /// The Cargo binary to bundle: `app.binary`, or the package's name.
    pub(crate) fn binary(&self, root: &Path) -> Result<String> {
        if let Some(b) = &self.app.binary {
            return Ok(b.clone());
        }
        let name = cargo_package(root)?.name;
        ensure!(is_file_name(&name), "package name {name:?} can't be a binary name");
        Ok(name)
    }

    /// `app.version`, or the package's version in `Cargo.toml`.
    pub(crate) fn version(&self, root: &Path) -> String {
        if let Some(v) = &self.app.version {
            return v.clone();
        }
        cargo_package(root)
            .ok()
            .and_then(|p| match p.version {
                Some(toml::Value::String(v)) => Some(v),
                _ => None,
            })
            .unwrap_or_else(|| "0.0.0".into())
    }

    /// The installer's welcome text, in the user's language when the app gives one.
    pub(crate) fn description(&self) -> &str {
        let lang = crate::i18n::current().code();
        self.app.descriptions.get(lang).map_or(self.app.description.as_str(), String::as_str)
    }

    /// The built bundle: `dist/<name>.app` in the repository.
    pub(crate) fn bundle(&self, root: &Path) -> PathBuf {
        root.join("dist").join(format!("{}.app", self.app.name))
    }

    /// The executable inside the built bundle.
    pub(crate) fn executable(&self, root: &Path) -> Result<PathBuf> {
        Ok(self.bundle(root).join("Contents/MacOS").join(self.binary(root)?))
    }
}

/// Is `path` (relative, with '/') one of `protect` (files, or folders ending in '/')?
pub(crate) fn is_protected(protect: &[String], path: &str) -> bool {
    protect.iter().any(|p| match p.strip_suffix('/') {
        Some(dir) => path.starts_with(&format!("{dir}/")),
        None => path == p,
    })
}

fn cargo_package(root: &Path) -> Result<CargoPackage> {
    let path = root.join("Cargo.toml");
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let manifest: CargoManifest = toml::from_str(&text).with_context(|| format!("in {}", path.display()))?;
    manifest.package.with_context(|| format!("{} has no [package]; set app.binary in faust.toml", path.display()))
}

/// A single path component that stays inside its folder.
fn is_file_name(s: &str) -> bool {
    !s.is_empty() && !s.contains('/') && !s.contains('\0') && !s.starts_with('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_minimal_file() {
        let c = Config::parse("[app]\nname = \"Hello\"\nid = \"com.example.hello\"\n").unwrap();
        assert_eq!(c.toolchain.rust, DEFAULT_RUST);
        assert!(c.check.tests);
        assert_eq!(c.check.ready_timeout_secs, 60);
        assert_eq!(c.repair.rules, PathBuf::from("FAUST.md"));
        assert_eq!(c.bundle(Path::new("/r")), PathBuf::from("/r/dist/Hello.app"));
    }

    #[test]
    fn rejects_what_would_escape_or_mislead() {
        let bad = [
            "[app]\nname = \"../x\"\nid = \"com.example.x\"\n",
            "[app]\nname = \"X\"\nid = \"com..x\"\n",
            "[app]\nname = \"X\"\nid = \"com.example.x\"\nbinary = \"a/b\"\n",
            "[app]\nname = \"X\"\nid = \"com.example.x\"\n[app.info_plist]\nK = [1]\n",
            "[app]\nname = \"X\"\nid = \"com.example.x\"\nunknown = 1\n",
            "[app]\nname = \"X\"\nid = \"com.example.x\"\n[toolchain]\nrust = \"1.96; rm\"\n",
        ];
        for text in bad {
            assert!(Config::parse(text).is_err(), "accepted: {text}");
        }
    }

    #[test]
    fn protects_files_and_folders() {
        let c = Config::parse(
            "[app]\nname = \"X\"\nid = \"com.example.x\"\n[repair]\nprotect = [\"LICENSE\", \"legal/\"]\n",
        )
        .unwrap();
        let protected = |p: &str| is_protected(&c.repair.protect, p);
        assert!(protected("LICENSE"));
        assert!(protected("legal/terms.md"));
        assert!(!protected("LICENSE.md"));
        assert!(!protected("legalese.md"));
    }
}
