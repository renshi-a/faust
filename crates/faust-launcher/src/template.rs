//! `faust new`: a small repairable app to start from.

use std::path::Path;

use anyhow::{Context, Result, ensure};

use crate::config::{self, Config};

/// The template's files: (path in the new app, text with `{{placeholders}}`).
const FILES: &[(&str, &str)] = &[
    ("Cargo.toml", include_str!("../templates/app/Cargo.toml.tmpl")),
    ("faust.toml", include_str!("../templates/app/faust.toml.tmpl")),
    ("FAUST.md", include_str!("../templates/app/FAUST.md.tmpl")),
    (".gitignore", include_str!("../templates/app/gitignore.tmpl")),
    ("src/main.rs", include_str!("../templates/app/src/main.rs")),
];

/// The `faust-link` requirement new apps get: the release of this repository that matches
/// this `faust`.
const FAUST_LINK: &str =
    concat!("{ git = \"https://github.com/renshi-a/faust\", tag = \"v", env!("CARGO_PKG_VERSION"), "\" }");

/// Fills in a template file.
fn render(text: &str, name: &str, binary: &str, id: &str, faust_link: &str) -> String {
    text.replace("{{name}}", name)
        .replace("{{binary}}", binary)
        .replace("{{id}}", id)
        .replace("{{rust}}", config::DEFAULT_RUST)
        .replace("{{faust_link}}", faust_link)
}

/// `Hello World` -> `hello-world`: a Cargo package name.
fn package_name(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').to_owned();
    if out.is_empty() { "app".into() } else { out }
}

/// Creates a new app in `dir`.
///
/// # Errors
///
/// When `dir` already exists and isn't empty, or a file can't be written.
pub(crate) fn create(dir: &Path, name: Option<&str>, id: Option<&str>) -> Result<()> {
    if dir.exists() {
        ensure!(dir.read_dir()?.next().is_none(), "{} already exists and isn't empty", dir.display());
    }
    let folder = dir.file_name().context("the folder needs a name")?.to_string_lossy().into_owned();
    let name = name.map_or_else(|| folder.clone(), str::to_owned);
    let binary = package_name(&folder);
    let id = id.map_or_else(|| format!("com.example.{}", binary.replace('-', "")), str::to_owned);
    for (path, text) in FILES {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().expect("template files sit in a folder"))?;
        std::fs::write(&file, render(text, &name, &binary, &id, FAUST_LINK))
            .with_context(|| format!("writing {}", file.display()))?;
    }
    Config::load(dir).context("the new faust.toml")?;
    println!("Created {name} in {}.", dir.display());
    println!();
    println!("Next:");
    println!("  cd {}", dir.display());
    println!("  git init && git add -A && git commit -m \"Start {name}\"");
    println!("  faust build        # dist/{name}.app, built without Xcode");
    println!("  faust dev          # open it the way a user would, then ask it for a change");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_packages() {
        assert_eq!(package_name("Hello World!"), "hello-world");
        assert_eq!(package_name("Notes"), "notes");
        assert_eq!(package_name("—"), "app");
    }

    #[test]
    fn creates_an_app_faust_can_read() {
        let dir = std::env::temp_dir().join(format!("faust-new-{}", std::process::id())).join("Happy Day");
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
        create(&dir, None, None).unwrap();
        let c = Config::load(&dir).unwrap();
        assert_eq!((c.app.name.as_str(), c.app.id.as_str()), ("Happy Day", "com.example.happyday"));
        assert_eq!(c.binary(&dir).unwrap(), "happy-day");
        assert!(std::fs::read_to_string(dir.join("src/main.rs")).unwrap().contains("Quit Happy Day"));
        assert!(create(&dir, None, None).is_err(), "refuses a folder that isn't empty");
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    /// `examples/hello` is the template, filled in, with faust-link from this repository.
    #[test]
    fn the_example_is_the_template() {
        let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/hello");
        if !example.is_dir() {
            return;
        }
        for (path, text) in FILES {
            let want = render(
                text,
                "Hello",
                "hello",
                "dev.faust.hello",
                "{ path = \"../../crates/faust-link\", version = \"0.1\" }",
            );
            let got = std::fs::read_to_string(example.join(path)).unwrap();
            assert_eq!(got, want, "examples/hello/{path} differs from the template");
        }
    }
}
