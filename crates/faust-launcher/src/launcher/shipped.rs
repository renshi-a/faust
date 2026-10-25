//! What the packaged app carries: the official source and which release it is.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::Config;
use crate::package::Release;

/// `Contents/Resources/faust` of the running launcher.
#[derive(Debug)]
pub(crate) struct Shipped {
    /// The source, `faust.toml` included.
    pub(crate) source: PathBuf,
    pub(crate) release: Release,
    /// The shipped `faust.toml`: who the app is. Versions may change their own copy; this
    /// one comes from the signed bundle.
    pub(crate) config: Config,
}

impl Shipped {
    /// Reads the launcher's own bundle (`FAUST_RESOURCES` points elsewhere, for tests).
    pub(crate) fn find() -> Result<Shipped> {
        let dir = resources_dir()?;
        let release_path = dir.join("release.json");
        let text =
            std::fs::read_to_string(&release_path).with_context(|| format!("reading {}", release_path.display()))?;
        let release: Release = serde_json::from_str(&text).with_context(|| format!("in {}", release_path.display()))?;
        let source = dir.join("source");
        let config = Config::load(&source)?;
        Ok(Shipped { source, release, config })
    }
}

/// `<bundle>/Contents/Resources/faust` for the running executable.
pub(crate) fn resources_dir() -> Result<PathBuf> {
    if let Some(dir) = super::overrides::get("FAUST_RESOURCES") {
        return Ok(PathBuf::from(dir));
    }
    let exe = std::env::current_exe()?;
    Ok(exe.parent().and_then(Path::parent).context("the executable is not inside a bundle")?.join("Resources/faust"))
}

/// The `.app` the launcher runs from, if it runs from one.
pub(crate) fn bundle_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let app = exe.parent()?.parent()?.parent()?;
    (app.extension()? == "app" && app.join("Contents/Info.plist").is_file()).then(|| app.to_path_buf())
}
