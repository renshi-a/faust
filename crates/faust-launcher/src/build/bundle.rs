//! Putting a built binary into a `.app` bundle, and writing `Info.plist` files.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};

use crate::config::Config;

/// A value in a property list.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PlistValue {
    String(String),
    Bool(bool),
    Integer(i64),
    Array(Vec<String>),
}

impl From<&str> for PlistValue {
    fn from(s: &str) -> PlistValue {
        PlistValue::String(s.to_owned())
    }
}

impl From<String> for PlistValue {
    fn from(s: String) -> PlistValue {
        PlistValue::String(s)
    }
}

impl From<bool> for PlistValue {
    fn from(b: bool) -> PlistValue {
        PlistValue::Bool(b)
    }
}

/// An `Info.plist` with the keys in the order they were given.
pub(crate) fn render_plist(entries: &[(String, PlistValue)]) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n",
    );
    for (key, value) in entries {
        out.push_str(&format!("  <key>{}</key>", escape(key)));
        match value {
            PlistValue::String(s) => out.push_str(&format!("<string>{}</string>", escape(s))),
            PlistValue::Bool(b) => out.push_str(if *b { "<true/>" } else { "<false/>" }),
            PlistValue::Integer(n) => out.push_str(&format!("<integer>{n}</integer>")),
            PlistValue::Array(items) => {
                out.push_str("<array>");
                for item in items {
                    out.push_str(&format!("<string>{}</string>", escape(item)));
                }
                out.push_str("</array>");
            }
        }
        out.push('\n');
    }
    out.push_str("</dict>\n</plist>\n");
    out
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The keys every bundle Faust makes has, plus the app's own extras.
pub(crate) fn app_plist(config: &Config, root: &Path, build_number: u64) -> Result<Vec<(String, PlistValue)>> {
    let app = &config.app;
    let mut entries: Vec<(String, PlistValue)> = vec![
        ("CFBundleExecutable".into(), config.binary(root)?.into()),
        ("CFBundleIdentifier".into(), app.id.as_str().into()),
        ("CFBundleName".into(), app.name.as_str().into()),
        ("CFBundleDisplayName".into(), app.name.as_str().into()),
        ("CFBundlePackageType".into(), "APPL".into()),
        ("CFBundleShortVersionString".into(), config.version(root).into()),
        ("CFBundleVersion".into(), build_number.to_string().into()),
        ("CFBundleDevelopmentRegion".into(), "en".into()),
        ("LSMinimumSystemVersion".into(), app.minimum_macos.as_str().into()),
        ("NSHighResolutionCapable".into(), true.into()),
    ];
    if app.icon.is_some() {
        entries.push(("CFBundleIconFile".into(), "AppIcon".into()));
    }
    for (key, value) in &app.info_plist {
        let value = match value {
            toml::Value::String(s) => PlistValue::String(s.clone()),
            toml::Value::Boolean(b) => PlistValue::Bool(*b),
            toml::Value::Integer(n) => PlistValue::Integer(*n),
            other => anyhow::bail!("app.info_plist.{key}: unsupported value {other}"),
        };
        entries.retain(|(k, _)| k != key);
        entries.push((key.clone(), value));
    }
    Ok(entries)
}

/// Makes `dist/<name>.app` from the built `binary`: the executable, the icon, the resources
/// and `Info.plist`. Returns the bundle's path.
pub(crate) fn assemble(config: &Config, root: &Path, binary: &Path, build_number: u64) -> Result<PathBuf> {
    let bundle = config.bundle(root);
    let _ = std::fs::remove_dir_all(&bundle);
    let contents = bundle.join("Contents");
    let macos = contents.join("MacOS");
    let resources = contents.join("Resources");
    std::fs::create_dir_all(&macos)?;
    std::fs::create_dir_all(&resources)?;
    std::fs::copy(binary, macos.join(config.binary(root)?)).with_context(|| format!("copying {}", binary.display()))?;
    if let Some(icon) = &config.app.icon {
        let from = inside(root, icon)?;
        std::fs::copy(&from, resources.join("AppIcon.icns")).with_context(|| format!("copying {}", from.display()))?;
    }
    for item in &config.app.resources {
        let from = inside(root, item)?;
        let name = from.file_name().context("a resource path has no file name")?;
        copy_tree(&from, &resources.join(name)).with_context(|| format!("copying {}", from.display()))?;
    }
    std::fs::write(contents.join("Info.plist"), render_plist(&app_plist(config, root, build_number)?))?;
    Ok(bundle)
}

/// `rel` under `root`, refusing paths that climb out of it.
pub(crate) fn inside(root: &Path, rel: &Path) -> Result<PathBuf> {
    ensure!(
        rel.is_relative() && !rel.components().any(|c| matches!(c, std::path::Component::ParentDir)),
        "{} must be a path inside the repository",
        rel.display()
    );
    Ok(root.join(rel))
}

/// Copies a file, or a folder with everything in it (symbolic links stay links).
pub(crate) fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(from)?;
    if meta.file_type().is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(from)?, to)?;
    } else if meta.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
    } else {
        std::fs::copy(from, to)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_escapes_and_orders() {
        let text = render_plist(&[
            ("A".into(), "x < y & z".into()),
            ("B".into(), true.into()),
            ("C".into(), PlistValue::Array(vec!["en".into(), "ja".into()])),
        ]);
        assert!(text.contains("<key>A</key><string>x &lt; y &amp; z</string>\n  <key>B</key><true/>\n"), "{text}");
        assert!(text.contains("<array><string>en</string><string>ja</string></array>"), "{text}");
    }

    #[test]
    fn app_keys_take_extras() {
        let c = Config::parse("[app]\nname = \"Hi\"\nid = \"com.example.hi\"\nbinary = \"hi\"\nversion = \"1.2.0\"\n[app.info_plist]\nLSUIElement = true\nCFBundleDevelopmentRegion = \"ja\"\n").unwrap();
        let entries = app_plist(&c, Path::new("/nowhere"), 7).unwrap();
        let get = |k: &str| entries.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
        assert_eq!(get("CFBundleExecutable"), Some("hi".into()));
        assert_eq!(get("CFBundleShortVersionString"), Some("1.2.0".into()));
        assert_eq!(get("CFBundleVersion"), Some("7".into()));
        assert_eq!(get("LSUIElement"), Some(true.into()));
        assert_eq!(get("CFBundleDevelopmentRegion"), Some("ja".into()));
        assert!(inside(Path::new("/r"), Path::new("../x")).is_err());
    }
}
