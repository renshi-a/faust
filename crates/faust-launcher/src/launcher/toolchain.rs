//! The Rust toolchain in `faust/toolchain`: rustup with `RUSTUP_HOME` and `CARGO_HOME`
//! pointing there. The user's `~/.rustup`, `~/.cargo` and shell `PATH` are left alone.

use std::ffi::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use objc2::msg_send;
use objc2::runtime::AnyObject;

use super::paths::Paths;
use crate::build::Toolchain;
use crate::objc::nsstring;
use crate::proc::{self, Limits, Output};

pub(crate) const RUSTUP_VERSION: &str = "1.29.2";
const RUSTUP_URL: &str = "https://static.rust-lang.org/rustup/archive/1.29.2/aarch64-apple-darwin/rustup-init";
const RUSTUP_SHA256: &str = "f7c7e11eb2e07f006e532a8aaac77b32f4527d305ac386c0c633ca6133d35ca9";
const RUSTUP_SIZE: u64 = 11_321_792;
/// rustup and a minimal Rust release, as measured for 1.96.1.
pub(crate) const TOOLCHAIN_SIZE: u64 = 528 << 20;
/// Free space needed before starting: the toolchain, crates, a few versions and target/.
const NEED_FREE: u64 = 3 << 30;
const HOST: &str = "aarch64-apple-darwin";

/// The toolchain as the build code sees it.
pub(crate) fn toolchain(p: &Paths, rust: &str) -> Toolchain {
    Toolchain { cargo_home: p.cargo_home.clone(), rustup_home: p.rustup_home.clone(), rust: rust.to_owned() }
}

pub(crate) fn installed(p: &Paths, rust: &str) -> bool {
    p.cargo_home.join("bin/cargo").is_file()
        && p.rustup_home.join(format!("toolchains/{rust}-{HOST}/bin/rustc")).is_file()
}

unsafe extern "C" {
    fn sysctlbyname(
        name: *const c_char,
        old: *mut c_void,
        oldlen: *mut usize,
        new: *const c_void,
        newlen: usize,
    ) -> c_int;
}

fn macos_version() -> Option<(u32, u32)> {
    let mut buf = [0u8; 32];
    let mut len = buf.len();
    // SAFETY: the name is NUL-terminated and `buf`/`len` describe a writable buffer.
    let r = unsafe {
        sysctlbyname(c"kern.osproductversion".as_ptr(), buf.as_mut_ptr().cast(), &mut len, std::ptr::null(), 0)
    };
    if r != 0 {
        return None;
    }
    let s = String::from_utf8_lossy(&buf[..len.saturating_sub(1)]).into_owned();
    let mut it = s.split('.').map(|n| n.parse().unwrap_or(0));
    Some((it.next()?, it.next().unwrap_or(0)))
}

fn free_space(dir: &Path) -> Option<u64> {
    let mut d = dir;
    while !d.exists() {
        d = d.parent()?;
    }
    let path = nsstring(&d.to_string_lossy());
    // SAFETY: NSFileManager's documented messages, with an NSString path; nil results are checked.
    unsafe {
        let fm: *mut AnyObject = msg_send![objc2::class!(NSFileManager), defaultManager];
        let attrs: *mut AnyObject =
            msg_send![fm, attributesOfFileSystemForPath: &*path, error: std::ptr::null_mut::<*mut AnyObject>()];
        if attrs.is_null() {
            return None;
        }
        let n: *mut AnyObject = msg_send![attrs, objectForKey: &*nsstring("NSFileSystemFreeSize")];
        if n.is_null() {
            return None;
        }
        Some(msg_send![n, unsignedLongLongValue])
    }
}

/// Apple silicon, macOS 11 or later, and 3 GB free.
pub(crate) fn preflight(p: &Paths) -> Result<()> {
    if std::env::consts::ARCH != "aarch64" {
        bail!(tr!(
            en: "A Mac with Apple silicon is needed",
            ja: "Apple シリコンの Mac が必要です",
            de: "Ein Mac mit Apple Silicon ist erforderlich",
            it: "È necessario un Mac con Apple silicon",
        ));
    }
    if let Some((major, minor)) = macos_version()
        && major < 11
    {
        bail!(tr!(
            en: "macOS 11 or later is needed (this Mac has {major}.{minor})",
            ja: "macOS 11 以降が必要です（この Mac は {major}.{minor}）",
            de: "macOS 11 oder neuer ist erforderlich (dieser Mac hat {major}.{minor})",
            it: "È necessario macOS 11 o successivo (questo Mac ha {major}.{minor})",
        ));
    }
    if let Some(free) = free_space(&p.faust)
        && free < NEED_FREE
    {
        let mb = free >> 20;
        bail!(tr!(
            en: "Not enough free space ({mb} MB free; at least 3 GB is needed)",
            ja: "空き容量が足りません（{mb} MB の空きがあります。3 GB 以上必要です）",
            de: "Nicht genug freier Speicher ({mb} MB frei, mindestens 3 GB nötig)",
            it: "Spazio libero insufficiente ({mb} MB liberi, ne servono almeno 3 GB)",
        ));
    }
    Ok(())
}

/// Downloads rustup-init and checks it against the hash built into the launcher.
pub(crate) fn download_rustup(p: &Paths, limits: Limits<'_>, progress: &mut dyn FnMut(f64, &str)) -> Result<PathBuf> {
    let file = download(
        p,
        RUSTUP_URL,
        &format!("rustup-init-{RUSTUP_VERSION}"),
        RUSTUP_SHA256,
        RUSTUP_SIZE,
        limits,
        progress,
    )?;
    std::fs::set_permissions(&file, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
    Ok(file)
}

/// Downloads `url` into `toolchain/downloads/<name>` with `/usr/bin/curl` (part of macOS,
/// no Command Line Tools needed) unless it is already there, and checks its SHA-256.
fn download(
    p: &Paths,
    url: &str,
    name: &str,
    sha: &str,
    size: u64,
    limits: Limits<'_>,
    progress: &mut dyn FnMut(f64, &str),
) -> Result<PathBuf> {
    std::fs::create_dir_all(&p.downloads)?;
    let file = p.downloads.join(name);
    if proc::sha256_file(&file).ok().as_deref() == Some(sha) {
        return Ok(file);
    }
    let partial = p.downloads.join(format!("{name}.part"));
    let mut cmd = Command::new("/usr/bin/curl");
    // `-q` first: no ~/.curlrc. The download is checked by its hash either way.
    cmd.args(["-q", "--fail", "--location", "--silent", "--show-error", "--retry", "2", "--output"])
        .arg(&partial)
        .arg(url);
    proc::run(cmd, None, &p.log("setup.log"), limits, &mut |o| match o {
        Output::Line(l) => progress(0.0, l),
        Output::Tick => {
            let n = std::fs::metadata(&partial).map_or(0, |m| m.len());
            progress(n as f64 / size as f64, &format!("{} / {} MB", n >> 20, size >> 20));
        }
    })?;
    let got = proc::sha256_file(&partial)?;
    if got != sha {
        let _ = std::fs::remove_file(&partial);
        bail!(tr!(
            en: "The download of {name} doesn't match its checksum ({got})",
            ja: "{name} のダウンロードがチェックサムと一致しません（{got}）",
            de: "Der Download von {name} stimmt nicht mit der Prüfsumme überein ({got})",
            it: "Il download di {name} non corrisponde al checksum ({got})",
        ));
    }
    std::fs::rename(&partial, &file)?;
    Ok(file)
}

/// `rustup-init -y --no-modify-path --profile minimal --default-toolchain <rust>`.
pub(crate) fn install(
    p: &Paths,
    rustup_init: &Path,
    rust: &str,
    limits: Limits<'_>,
    progress: &mut dyn FnMut(f64, &str),
) -> Result<()> {
    let mut cmd = Command::new(rustup_init);
    cmd.args([
        "-y",
        "--no-modify-path",
        "--no-update-default-toolchain",
        "--profile",
        "minimal",
        "--default-toolchain",
        rust,
    ])
    .env_clear()
    .env("HOME", std::env::var_os("HOME").unwrap_or_default())
    .env("PATH", "/usr/bin:/bin")
    .env("RUSTUP_HOME", &p.rustup_home)
    .env("CARGO_HOME", &p.cargo_home)
    .env("RUSTUP_INIT_SKIP_PATH_CHECK", "yes");
    let mut last = String::new();
    let mut ticks = 0u32;
    proc::run(cmd, None, &p.log("setup.log"), limits, &mut |o| match o {
        Output::Line(l) => l.clone_into(&mut last),
        Output::Tick => {
            ticks += 1;
            if ticks.is_multiple_of(4) {
                progress(proc::dir_size(&p.rustup_home) as f64 / TOOLCHAIN_SIZE as f64, &last);
            }
        }
    })?;
    if !installed(p, rust) {
        bail!(not_installed(&format!("Rust {rust}")));
    }
    Ok(())
}

pub(crate) fn not_installed(what: &str) -> String {
    tr!(
        en: "{what} couldn't be installed",
        ja: "{what} をインストールできませんでした",
        de: "{what} konnte nicht installiert werden",
        it: "Impossibile installare {what}",
    )
}

/// Adds another Rust release (an official update asked for a newer one).
pub(crate) fn add_release(p: &Paths, rust: &str, limits: Limits<'_>, progress: &mut dyn FnMut(&str)) -> Result<()> {
    let mut cmd = Command::new(p.cargo_home.join("bin/rustup"));
    cmd.args(["toolchain", "install", "--profile", "minimal", rust])
        .env("RUSTUP_HOME", &p.rustup_home)
        .env("CARGO_HOME", &p.cargo_home)
        .env("PATH", "/usr/bin:/bin");
    proc::run(cmd, None, &p.log("setup.log"), limits, &mut |o| {
        if let Output::Line(l) = o {
            progress(l);
        }
    })?;
    if !installed(p, rust) {
        bail!(not_installed(&format!("Rust {rust}")));
    }
    Ok(())
}

/// The packages in a `Cargo.lock`, to estimate how many lines a build prints.
pub(crate) fn package_count(dir: &Path) -> usize {
    std::fs::read_to_string(dir.join("Cargo.lock")).map_or(40, |t| t.matches("[[package]]").count().max(1))
}

/// `cargo fetch --locked`: the crates the build needs, into `CARGO_HOME`.
pub(crate) fn fetch(
    p: &Paths,
    rust: &str,
    dir: &Path,
    limits: Limits<'_>,
    progress: &mut dyn FnMut(f64, &str),
) -> Result<()> {
    let total = package_count(dir) as f64;
    // The source's build scripts don't run here, but cargo reads the source's configuration.
    let writable = super::sandbox::Writable {
        dirs: Vec::new(),
        cargo_home: p.cargo_home.clone(),
        secret: vec![p.assistant.clone()],
    };
    let mut cmd = super::sandbox::command(&writable, &p.cargo_home.join("bin/cargo"));
    cmd.args(["fetch", "--locked"])
        .current_dir(dir)
        .env_clear()
        .env("HOME", std::env::var_os("HOME").unwrap_or_default())
        .env("PATH", format!("{}:/usr/bin:/bin", p.cargo_home.join("bin").display()))
        .env("CARGO_HOME", &p.cargo_home)
        .env("RUSTUP_HOME", &p.rustup_home)
        .env("RUSTUP_TOOLCHAIN", rust);
    let mut n = 0.0;
    proc::run(cmd, None, &p.log("setup.log"), limits, &mut |o| {
        if let Output::Line(l) = o {
            if l.trim_start().starts_with("Downloaded") {
                n += 1.0;
            }
            progress(n / total, l);
        }
    })
    .context("fetching the crates")?;
    Ok(())
}
