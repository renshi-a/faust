//! Building and testing an app with rustup's Rust and its `rust-lld` alone: no Xcode, no
//! Command Line Tools, no C compiler. The same code runs on the developer's Mac
//! (`faust build`) and on the user's (the launcher, inside a sandbox).
//!
//! Cargo runs with a scrubbed environment: `PATH` has no `/usr/bin` (where the Xcode shims
//! live) but a folder of tripwires named `cc`, `ld`, `xcrun` and friends that log the call
//! and fail. A build only counts when no tripwire fired.
//!
//! Linking takes two stages (see [`sdk`]). Stage 1 is needed only when the code starts to
//! use a library function it did not use before, so a rebuild first tries stage 2 with the
//! SDK from the last build and falls back to stage 1 when the link misses a symbol.

pub(crate) mod bundle;
pub(crate) mod sdk;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::config::Config;
use crate::proc::{self, Limits, Output};

/// rustc spawns `rust-lld` itself; it ships in the toolchain's sysroot.
const LINK_FLAGS: &str = "-Clinker=rust-lld -Clinker-flavor=ld64.lld";
const DYNAMIC_LOOKUP: &str = "-Clink-arg=-undefined -Clink-arg=dynamic_lookup";

/// Programs that must never run during a build.
const TRIPWIRES: &[&str] = &[
    "cc",
    "c++",
    "clang",
    "clang++",
    "gcc",
    "g++",
    "ld",
    "ld64",
    "xcrun",
    "xcodebuild",
    "xcode-select",
    "libtool",
    "lipo",
    "strip",
    "codesign",
    "make",
    "cmake",
];

/// Where rustup put Rust, and which release to use.
#[derive(Debug, Clone)]
pub(crate) struct Toolchain {
    pub(crate) cargo_home: PathBuf,
    pub(crate) rustup_home: PathBuf,
    pub(crate) rust: String,
}

impl Toolchain {
    /// `CARGO_HOME`, `RUSTUP_HOME` and `RUSTUP_TOOLCHAIN` when set, else rustup's defaults and
    /// the release `faust.toml` pins.
    pub(crate) fn from_env(config: &Config) -> Toolchain {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        Toolchain {
            cargo_home: std::env::var_os("CARGO_HOME").map_or_else(|| home.join(".cargo"), PathBuf::from),
            rustup_home: std::env::var_os("RUSTUP_HOME").map_or_else(|| home.join(".rustup"), PathBuf::from),
            rust: std::env::var("RUSTUP_TOOLCHAIN").unwrap_or_else(|_| config.toolchain.rust.clone()),
        }
    }

    fn cargo(&self) -> PathBuf {
        self.cargo_home.join("bin/cargo")
    }

    /// The same release under the name rustup knows it by: `1.96.1` is often installed as
    /// `stable` on a developer's Mac.
    ///
    /// # Errors
    ///
    /// When rustup or the pinned release is missing, with what to run to fix it.
    pub(crate) fn resolve(mut self) -> Result<Toolchain> {
        if !self.cargo().is_file() {
            bail!("{} not found: install Rust with rustup (https://rustup.rs)", self.cargo().display());
        }
        let toolchains = self.rustup_home.join("toolchains");
        if toolchains.join(format!("{}-{HOST}/bin/rustc", self.rust)).is_file() {
            return Ok(self);
        }
        let wanted = format!("rustc {} ", self.rust);
        let entries = std::fs::read_dir(&toolchains).map(|d| d.flatten().collect::<Vec<_>>()).unwrap_or_default();
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(channel) = name.strip_suffix(&format!("-{HOST}")) else { continue };
            let version = Command::new(entry.path().join("bin/rustc"))
                .arg("--version")
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default();
            if version.starts_with(&wanted) {
                self.rust = channel.to_owned();
                return Ok(self);
            }
        }
        bail!("Rust {} is not installed: run `rustup toolchain install {} --profile minimal`", self.rust, self.rust)
    }

    fn check(&self) -> Result<()> {
        if !self.rustup_home.join("toolchains").join(format!("{}-{HOST}/bin/rustc", self.rust)).is_file() {
            bail!("Rust {} is not installed", self.rust);
        }
        Ok(())
    }
}

const HOST: &str = "aarch64-apple-darwin";

/// An app's repository, ready to build.
#[derive(Debug)]
pub(crate) struct Project<'a> {
    pub(crate) root: &'a Path,
    pub(crate) config: &'a Config,
    pub(crate) toolchain: &'a Toolchain,
    /// Cargo's output; the launcher shares one between versions.
    pub(crate) target_dir: PathBuf,
}

/// Where a build's output goes.
pub(crate) struct Sink<'a> {
    pub(crate) log: &'a Path,
    pub(crate) limits: Limits<'a>,
    pub(crate) on_line: &'a mut dyn FnMut(&str),
}

impl std::fmt::Debug for Sink<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sink").field("log", &self.log).finish_non_exhaustive()
    }
}

impl Sink<'_> {
    fn say(&mut self, step: &str) {
        (self.on_line)(&format!("== {step}"));
    }
}

impl Project<'_> {
    fn faust_dir(&self) -> PathBuf {
        self.target_dir.join("faust")
    }

    /// A cargo command with nothing from the caller's environment but what the build needs.
    fn cargo(&self, sdk: &Path, rustflags: &str, target: &Path) -> Command {
        let tc = self.toolchain;
        let mut cmd = Command::new(tc.cargo());
        cmd.current_dir(self.root).env_clear();
        let path: OsString =
            std::env::join_paths([self.faust_dir().join("tripwire"), tc.cargo_home.join("bin"), PathBuf::from("/bin")])
                .expect("paths without ':'");
        cmd.env("HOME", std::env::var_os("HOME").unwrap_or_default())
            .env("PATH", path)
            .env("CARGO_HOME", &tc.cargo_home)
            .env("RUSTUP_HOME", &tc.rustup_home)
            .env("RUSTUP_TOOLCHAIN", &tc.rust)
            .env("CARGO_TARGET_DIR", target)
            .env("CARGO_TERM_COLOR", "never")
            .env("TERM", "dumb")
            .env("DEVELOPER_DIR", "/nonexistent-faust-uses-no-xcode")
            .env("MACOSX_DEPLOYMENT_TARGET", &self.config.app.minimum_macos)
            .env("SDKROOT", sdk)
            .env("RUSTFLAGS", rustflags)
            .env("RUSTDOCFLAGS", rustflags);
        for key in
            ["http_proxy", "https_proxy", "no_proxy", "HTTP_PROXY", "HTTPS_PROXY", "NO_PROXY", "CARGO_NET_OFFLINE"]
        {
            if let Some(v) = std::env::var_os(key) {
                cmd.env(key, v);
            }
        }
        cmd
    }

    fn write_tripwires(&self) -> Result<PathBuf> {
        let dir = self.faust_dir().join("tripwire");
        let log = dir.join("calls.log");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(&log, "")?;
        for name in TRIPWIRES {
            let script = format!("#!/bin/sh\necho \"{name} $*\" >> '{}'\nexit 127\n", log.display());
            let path = dir.join(name);
            std::fs::write(&path, script)?;
            std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
        }
        Ok(log)
    }

    fn check_tripwires(log: &Path) -> Result<()> {
        let calls = std::fs::read_to_string(log).unwrap_or_default();
        if !calls.trim().is_empty() {
            bail!(
                "the build tried to run Apple or C tools, which a Mac without Xcode does not have:\n{}\n\
                 A crate with a C part (its build script calls cc) can't be used; look for a pure-Rust \
                 alternative or a feature that turns the C part off.",
                calls.trim()
            );
        }
        Ok(())
    }

    /// Builds the app and puts it in `dist/<name>.app`. Returns the bundle.
    ///
    /// # Errors
    ///
    /// When cargo fails, a tripwire fired, or the bundle can't be made.
    pub(crate) fn build(&self, build_number: u64, sink: &mut Sink<'_>) -> Result<PathBuf> {
        self.toolchain.check()?;
        let binary = self.config.binary(self.root)?;
        let faust = self.faust_dir();
        let stubs = faust.join("stub-sdk");
        let mini = faust.join("mini-sdk");
        let mini_ok = faust.join("mini-sdk.ok");
        let stage1 = faust.join("stage1");
        let stage2 = faust.join("stage2");
        let tripwire_log = self.write_tripwires()?;
        sdk::write_stubs(&stubs)?;

        let args = ["build", "--release", "--workspace", "--bin", binary.as_str()];
        let run_stage1 = |sink: &mut Sink<'_>| -> Result<()> {
            sink.say("stage 1: link against empty stubs with -undefined dynamic_lookup");
            let mut cmd = self.cargo(&stubs, &format!("{LINK_FLAGS} {DYNAMIC_LOOKUP}"), &stage1);
            cmd.args(args);
            proc::run(cmd, None, sink.log, sink.limits, &mut |o| forward(o, sink.on_line))?;
            sink.say("mini SDK: the symbols the binaries import");
            let _ = std::fs::remove_file(&mini_ok);
            let made = sdk::write_mini_sdk(&mini, &stage1_binaries(&stage1.join("release"), &binary))?;
            let extra: Vec<String> = made.extra_libraries.iter().map(|p| p.display().to_string()).collect();
            std::fs::write(&mini_ok, format!("{}\n{}", env!("CARGO_PKG_VERSION"), extra.join("\n")))?;
            Ok(())
        };
        let run_stage2 = |sink: &mut Sink<'_>| -> Result<std::result::Result<(), Missing>> {
            sink.say("stage 2: two-level namespace link against the mini SDK");
            let marker = std::fs::read_to_string(&mini_ok).unwrap_or_default();
            let mut flags = LINK_FLAGS.to_owned();
            for lib in marker.lines().skip(1).filter(|l| !l.is_empty()) {
                flags.push_str(&format!(" -Clink-arg={lib}"));
            }
            let mut cmd = self.cargo(&mini, &flags, &stage2);
            cmd.args(args);
            let mut missing = false;
            let result = proc::run(cmd, None, sink.log, sink.limits, &mut |o| {
                if let Output::Line(l) = o
                    && l.contains("undefined symbol")
                {
                    missing = true;
                }
                forward(o, sink.on_line);
            });
            match result {
                Ok(_) => Ok(Ok(())),
                Err(_) if missing && !sink.limits.cancel.load(std::sync::atomic::Ordering::Relaxed) => Ok(Err(Missing)),
                Err(e) => Err(e),
            }
        };

        let reusable =
            std::fs::read_to_string(&mini_ok).is_ok_and(|m| m.lines().next() == Some(env!("CARGO_PKG_VERSION")));
        if reusable {
            sink.say("stage 1 skipped: trying the mini SDK of the last build");
            if run_stage2(sink)?.is_err() {
                sink.say("the mini SDK lacks a symbol the code now uses: making it again");
                run_stage1(sink)?;
                run_stage2(sink)?.map_err(|Missing| anyhow::anyhow!("the link still misses symbols"))?;
            }
        } else {
            run_stage1(sink)?;
            run_stage2(sink)?.map_err(|Missing| anyhow::anyhow!("the link misses symbols the mini SDK should have"))?;
        }
        Project::check_tripwires(&tripwire_log)?;

        sink.say("bundle");
        let bundle = bundle::assemble(self.config, self.root, &stage2.join("release").join(&binary), build_number)?;
        (sink.on_line)(&format!("built {}", bundle.display()));
        Ok(bundle)
    }

    /// Runs the workspace's tests (`cargo test --workspace`), linked like stage 1.
    ///
    /// # Errors
    ///
    /// When a test fails or a tripwire fired.
    pub(crate) fn test(&self, sink: &mut Sink<'_>) -> Result<()> {
        self.toolchain.check()?;
        let faust = self.faust_dir();
        let stubs = faust.join("stub-sdk");
        let tripwire_log = self.write_tripwires()?;
        sdk::write_stubs(&stubs)?;
        sink.say("tests");
        let mut cmd = self.cargo(&stubs, &format!("{LINK_FLAGS} {DYNAMIC_LOOKUP}"), &faust.join("test"));
        cmd.args(["test", "--workspace"]);
        proc::run(cmd, None, sink.log, sink.limits, &mut |o| forward(o, sink.on_line)).context("the tests failed")?;
        Project::check_tripwires(&tripwire_log)
    }
}

/// The stage-2 link missed a symbol the mini SDK does not list.
#[derive(Debug)]
struct Missing;

fn forward(o: Output<'_>, on_line: &mut dyn FnMut(&str)) {
    if let Output::Line(l) = o {
        on_line(l);
    }
}

/// Everything stage 1 linked: the app, build scripts and proc macros (rustc loads those).
fn stage1_binaries(release: &Path, binary: &str) -> Vec<PathBuf> {
    let mut out = vec![release.join(binary)];
    let read = |dir: &Path| {
        std::fs::read_dir(dir).map(|d| d.flatten().map(|e| e.path()).collect::<Vec<_>>()).unwrap_or_default()
    };
    for dir in read(&release.join("build")) {
        out.extend(read(&dir).into_iter().filter(|p| {
            p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("build-script-")) && is_executable(p)
        }));
    }
    out.extend(read(&release.join("deps")).into_iter().filter(|p| p.extension().is_some_and(|e| e == "dylib")));
    out
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}
