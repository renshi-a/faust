//! The sandbox around code from the app's source. Building runs it (build scripts, proc
//! macros), and so do the tests and the self-test, before the user has agreed to use the
//! version, so it runs under `sandbox-exec`:
//!
//! - it may write only to the version's own folder, cargo's output, cargo's download cache
//!   (not its `bin`), and the temporary folders. The repository, `state.json`, the trusted
//!   devices, other versions and the toolchain stay read-only;
//! - it may not read the assistant's settings, which may hold an API key;
//! - it may not start the tools that hand work to processes outside the sandbox (`open`,
//!   `osascript`, `launchctl`, …), nor reach LaunchServices or Apple Events.
//!
//! Reading and the network are allowed: see docs/security.md for what that leaves open.

use std::path::{Path, PathBuf};
use std::process::Command;

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Programs that would run work outside the sandbox on its behalf.
const NO_EXEC: &[&str] = &[
    "/usr/bin/open",
    "/usr/bin/osascript",
    "/bin/launchctl",
    "/usr/bin/automator",
    "/usr/bin/shortcuts",
    "/usr/bin/sandbox-exec",
];

/// Services that start or script other apps.
const NO_LOOKUP: &[&str] = &[
    "com.apple.coreservices.launchservicesd",
    "com.apple.coreservices.appleevents",
    "com.apple.lsd.mapdb",
    "com.apple.lsd.modifydb",
];

/// Where sandboxed code may write.
#[derive(Debug, Clone)]
pub(crate) struct Writable {
    /// Folders it may write anywhere in.
    pub(crate) dirs: Vec<PathBuf>,
    /// Cargo's home: writable for downloads, except its `bin`.
    pub(crate) cargo_home: PathBuf,
    /// Files it may not even read.
    pub(crate) secret: Vec<PathBuf>,
}

fn quoted(p: &Path) -> String {
    let p = p.canonicalize().unwrap_or_else(|_| p.to_owned());
    let s = p.to_string_lossy();
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The SBPL profile for `writable`.
pub(crate) fn profile(writable: &Writable) -> String {
    let dirs: Vec<String> = writable.dirs.iter().map(|d| format!("(subpath {})", quoted(d))).collect();
    let cargo = quoted(&writable.cargo_home);
    let cargo_bin = quoted(&writable.cargo_home.join("bin"));
    let exec: Vec<String> = NO_EXEC.iter().map(|p| format!("(literal \"{p}\")")).collect();
    let lookup: Vec<String> = NO_LOOKUP.iter().map(|n| format!("(global-name \"{n}\")")).collect();
    let secret: String =
        writable.secret.iter().map(|f| format!("(deny file-read* (literal {}))\n", quoted(f))).collect();
    format!(
        "(version 1)\n(allow default)\n\
         (deny file-write* (require-not (require-any {} (require-all (subpath {cargo}) (require-not (subpath {cargo_bin}))) \
         (subpath \"/private/tmp\") (subpath \"/private/var/folders\") (subpath \"/dev\"))))\n\
         (deny process-exec {})\n(deny mach-lookup {})\n{secret}",
        dirs.join(" "),
        exec.join(" "),
        lookup.join(" "),
    )
}

/// `program`, run in the sandbox (or as it is where `sandbox-exec` is missing).
pub(crate) fn command(writable: &Writable, program: &Path) -> Command {
    if Path::new(SANDBOX_EXEC).is_file() {
        let mut c = Command::new(SANDBOX_EXEC);
        c.arg("-p").arg(profile(writable)).arg(program);
        c
    } else {
        Command::new(program)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_only_where_allowed() {
        if !Path::new(SANDBOX_EXEC).is_file() {
            return;
        }
        // Not under the temporary folders, which the sandbox always lets code write to.
        let base =
            Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../target/sandbox-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (inside, cargo) = (base.join("version"), base.join("cargo"));
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::create_dir_all(cargo.join("bin")).unwrap();
        std::fs::create_dir_all(cargo.join("registry")).unwrap();
        let secret = base.join("assistant.json");
        std::fs::write(&secret, "key").unwrap();
        let writable = Writable { dirs: vec![inside.clone()], cargo_home: cargo.clone(), secret: vec![secret.clone()] };
        let home = std::env::var("HOME").unwrap();
        let outside = format!("{home}/.faust-sandbox-test-{}", std::process::id());
        let script = format!(
            "echo x > '{}/a' && echo x > '{}/registry/b' && ! echo x > '{}/bin/cargo' 2>/dev/null && ! echo x > '{outside}' 2>/dev/null && ! /usr/bin/open -h >/dev/null 2>&1 && ! /bin/cat '{}' >/dev/null 2>&1",
            inside.display(),
            cargo.display(),
            cargo.display(),
            secret.display()
        );
        let ok = command(&writable, Path::new("/bin/sh")).args(["-c", &script]).status().unwrap();
        assert!(ok.success(), "{}", profile(&writable));
        assert!(inside.join("a").is_file() && cargo.join("registry/b").is_file());
        assert!(!cargo.join("bin/cargo").exists());
        assert!(!Path::new(&outside).exists());
        let _ = std::fs::remove_dir_all(base);
    }
}
