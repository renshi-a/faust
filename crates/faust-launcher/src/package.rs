//! `faust package`: the `.app` users download. It holds the launcher (this very executable,
//! copied in) and the app's source; the app itself is built on the user's Mac.
//!
//! ```text
//! Hello.app/Contents/
//!   MacOS/hello                     the launcher
//!   Resources/AppIcon.icns
//!   Resources/faust/release.json    which official release this is
//!   Resources/faust/source/         the app's source, faust.toml included
//! ```

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::build::bundle::{self, PlistValue};
use crate::config::Config;
use crate::repo::{Ignore, Repo, walk};

/// The launcher's bundle identifier is the app's with this suffix, so the bundle the
/// launcher builds keeps the app's own identifier (and its preferences).
pub(crate) const LAUNCHER_ID_SUFFIX: &str = ".faust";

/// Where path dependencies from outside the repository are copied, inside the source.
const VENDOR: &str = ".faust/vendor";

/// `Resources/faust/release.json`: the official release a packaged app carries.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Release {
    /// Orders releases: a launcher imports a shipped source only when this is larger than
    /// the last one it imported. Seconds since the epoch: the commit's time, or the moment
    /// a working tree was packaged.
    pub(crate) release: u64,
    /// For people: `1.2.0+3f2a9c1`.
    pub(crate) version: String,
    pub(crate) commit: String,
    pub(crate) rust: String,
    /// What is new, shown when an update is offered.
    pub(crate) notes: String,
}

/// Which source goes in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    /// The committed tree at HEAD.
    Head,
    /// The working tree as it is, committed or not (ignored files left out).
    WorkingTree,
}

#[derive(Debug)]
pub(crate) struct Options {
    pub(crate) root: PathBuf,
    pub(crate) source: Source,
    pub(crate) out_dir: PathBuf,
    pub(crate) dmg: bool,
    /// A bundle for `faust dev`: its launcher honors the developer settings in
    /// `launcher::overrides`. Never for a release.
    pub(crate) development: bool,
}

/// Makes `<out>/<name>.app` (and a `.dmg` when asked). Returns the bundle.
///
/// # Errors
///
/// When the repository can't be read or the bundle can't be written.
pub(crate) fn package(opts: &Options) -> Result<PathBuf> {
    let config = Config::load(&opts.root)?;
    let bundle = opts.out_dir.join(format!("{}.app", config.app.name));
    let _ = std::fs::remove_dir_all(&bundle);
    let contents = bundle.join("Contents");
    let resources = contents.join("Resources");
    let source_dir = resources.join("faust/source");
    std::fs::create_dir_all(contents.join("MacOS"))?;
    std::fs::create_dir_all(&source_dir)?;

    let release = write_source(&opts.root, &config, opts.source, &source_dir)?;
    ensure!(
        source_dir.join("Cargo.lock").is_file(),
        "the source has no Cargo.lock; run `faust build` once and commit Cargo.lock (users' Macs fetch exactly what it pins)"
    );
    vendor_path_dependencies(&opts.root, &source_dir)?;
    std::fs::write(resources.join("faust/release.json"), serde_json::to_string_pretty(&release)? + "\n")?;

    let binary = config.binary(&opts.root)?;
    let me = std::env::current_exe().context("finding the faust executable")?;
    std::fs::copy(&me, contents.join("MacOS").join(&binary)).with_context(|| format!("copying {}", me.display()))?;
    if let Some(icon) = &config.app.icon {
        std::fs::copy(bundle::inside(&opts.root, icon)?, resources.join("AppIcon.icns"))?;
    }
    let mut plist: Vec<(String, PlistValue)> = vec![
        ("CFBundleExecutable".into(), binary.into()),
        ("CFBundleIdentifier".into(), format!("{}{LAUNCHER_ID_SUFFIX}", config.app.id).into()),
        ("CFBundleName".into(), config.app.name.as_str().into()),
        ("CFBundleDisplayName".into(), config.app.name.as_str().into()),
        ("CFBundlePackageType".into(), "APPL".into()),
        ("CFBundleShortVersionString".into(), config.version(&opts.root).into()),
        ("CFBundleVersion".into(), release.release.to_string().into()),
        ("CFBundleDevelopmentRegion".into(), "en".into()),
        ("CFBundleLocalizations".into(), PlistValue::Array(["en", "ja", "de", "it"].map(str::to_owned).to_vec())),
        ("LSMinimumSystemVersion".into(), "11.0".into()),
        ("NSHighResolutionCapable".into(), true.into()),
    ];
    if config.app.icon.is_some() {
        plist.push(("CFBundleIconFile".into(), "AppIcon".into()));
    }
    if opts.development {
        plist.push((crate::launcher::overrides::DEVELOPMENT_KEY.into(), true.into()));
    }
    std::fs::write(contents.join("Info.plist"), bundle::render_plist(&plist))?;

    if opts.dmg {
        make_dmg(&bundle, &opts.out_dir.join(format!("{}.dmg", config.app.name)), &config.app.name)?;
    }
    Ok(bundle)
}

/// Writes the source into `dir` and says which release it is.
fn write_source(root: &Path, config: &Config, source: Source, dir: &Path) -> Result<Release> {
    let excluded: Vec<String> = ["target", "dist", ".git"]
        .iter()
        .map(|s| (*s).to_owned())
        .chain(config.package.exclude.iter().map(|s| s.trim_end_matches('/').to_owned()))
        .collect();
    let version = config.version(root);
    let rust = config.toolchain.rust.clone();
    let git = Repo::discover(root);
    let head = git.as_ref().and_then(|(r, _)| r.ref_id("HEAD"));
    match source {
        Source::Head => {
            let (Some((repo, rel)), Some(head)) = (&git, head) else {
                bail!(
                    "{} is not in a git repository with commits; commit first, or package the working tree with --worktree",
                    root.display()
                );
            };
            let tree = repo.subtree(repo.tree_of(head)?, rel)?;
            repo.export(tree, dir)?;
            for name in &excluded {
                let _ = std::fs::remove_dir_all(dir.join(name));
                let _ = std::fs::remove_file(dir.join(name));
            }
            let message = repo.message_of(head);
            let short: String = head.to_string().chars().take(7).collect();
            Ok(Release {
                release: u64::try_from(repo.commit_time(head)?).context("a commit from before 1970")?,
                version: format!("{version}+{short}"),
                commit: head.to_string(),
                rust,
                notes: message.lines().next().unwrap_or("").to_owned(),
            })
        }
        Source::WorkingTree => {
            let ignore = Ignore::read(root).with_top(excluded);
            for (rel, path) in walk(root, &ignore)? {
                let to = dir.join(&rel);
                std::fs::create_dir_all(to.parent().expect("a file inside the source"))?;
                bundle::copy_tree(&path, &to).with_context(|| rel.clone())?;
            }
            let short: String =
                head.map(|h| h.to_string().chars().take(7).collect()).unwrap_or_else(|| "nohead".into());
            Ok(Release {
                release: now(),
                version: format!("{version}+{short}-worktree"),
                commit: head.map(|h| h.to_string()).unwrap_or_default(),
                rust,
                notes: "Working tree snapshot".into(),
            })
        }
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Copies path dependencies that live outside the repository into the source, and points
/// `Cargo.toml` at the copies, so the source builds on its own.
fn vendor_path_dependencies(root: &Path, source: &Path) -> Result<()> {
    let manifest = source.join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest).with_context(|| format!("reading {}", manifest.display()))?;
    let mut doc: toml_edit::DocumentMut = text.parse().context("parsing Cargo.toml")?;
    let root = root.canonicalize()?;
    let mut changed = false;
    let mut visit = |deps: &mut dyn toml_edit::TableLike| -> Result<()> {
        for (_, dep) in deps.iter_mut() {
            let Some(table) = dep.as_table_like_mut() else { continue };
            let Some(rel) = table.get("path").and_then(|p| p.as_str()).map(str::to_owned) else { continue };
            let from = root.join(&rel).canonicalize().with_context(|| format!("path dependency {rel}"))?;
            if from.starts_with(&root) {
                continue;
            }
            let name = from.file_name().context("a path dependency without a name")?.to_string_lossy().into_owned();
            let to = source.join(VENDOR).join(&name);
            ensure!(!to.exists(), "two path dependencies are both named {name}");
            let ignore = Ignore::read(&from).with_top(["target".to_owned()]);
            for (file, path) in walk(&from, &ignore)? {
                let dest = to.join(&file);
                std::fs::create_dir_all(dest.parent().expect("a file inside the crate"))?;
                std::fs::copy(&path, &dest)?;
            }
            table.insert("path", toml_edit::value(format!("{VENDOR}/{name}")));
            changed = true;
        }
        Ok(())
    };
    for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(t) = doc.get_mut(key).and_then(|t| t.as_table_like_mut()) {
            visit(t)?;
        }
    }
    if let Some(t) =
        doc.get_mut("workspace").and_then(|w| w.get_mut("dependencies")).and_then(|t| t.as_table_like_mut())
    {
        visit(t)?;
    }
    if let Some(patches) = doc.get_mut("patch").and_then(|p| p.as_table_like_mut()) {
        for (_, registry) in patches.iter_mut() {
            if let Some(t) = registry.as_table_like_mut() {
                visit(t)?;
            }
        }
    }
    if changed {
        // Path dependencies inside a workspace's folder become its members; the copies are
        // not the app's to test.
        if let Some(workspace) = doc.get_mut("workspace").and_then(|w| w.as_table_like_mut()) {
            let exclude = workspace.entry("exclude").or_insert(toml_edit::value(toml_edit::Array::new()));
            if let Some(list) = exclude.as_array_mut()
                && !list.iter().any(|v| v.as_str() == Some(".faust"))
            {
                list.push(".faust");
            }
        }
        std::fs::write(&manifest, doc.to_string())?;
    }
    Ok(())
}

/// A disk image with the app and a link to /Applications. hdiutil ships with macOS.
fn make_dmg(bundle: &Path, dmg: &Path, name: &str) -> Result<()> {
    let stage = bundle.with_extension("dmg-stage");
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage)?;
    let status = std::process::Command::new("/bin/cp").arg("-R").arg(bundle).arg(&stage).status()?;
    ensure!(status.success(), "copying the app for the disk image failed");
    std::os::unix::fs::symlink("/Applications", stage.join("Applications"))?;
    let _ = std::fs::remove_file(dmg);
    let status = std::process::Command::new("/usr/bin/hdiutil")
        .args(["create", "-quiet", "-volname", name, "-format", "UDZO", "-ov", "-srcfolder"])
        .arg(&stage)
        .arg(dmg)
        .status()?;
    let _ = std::fs::remove_dir_all(&stage);
    ensure!(status.success(), "hdiutil could not make {}", dmg.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendors_outside_path_dependencies() {
        let base = std::env::temp_dir().join(format!("faust-vendor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (app, lib, out) = (base.join("app"), base.join("lib"), base.join("out"));
        for d in [&app, &lib.join("src"), &out] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(lib.join("Cargo.toml"), "[package]\nname = \"lib\"\n").unwrap();
        std::fs::write(lib.join("src/lib.rs"), "").unwrap();
        let manifest = "[package]\nname = \"app\"\n\n[dependencies]\n# kept\nlib = { path = \"../lib\", version = \"0.1\" }\nserde = \"1\"\n";
        std::fs::write(out.join("Cargo.toml"), manifest).unwrap();
        vendor_path_dependencies(&app, &out).unwrap();
        let text = std::fs::read_to_string(out.join("Cargo.toml")).unwrap();
        assert!(text.contains("lib = { path = \".faust/vendor/lib\", version = \"0.1\" }"), "{text}");
        assert!(text.contains("# kept") && text.contains("serde = \"1\""), "{text}");
        assert!(out.join(".faust/vendor/lib/src/lib.rs").is_file());
        let _ = std::fs::remove_dir_all(base);
    }
}
