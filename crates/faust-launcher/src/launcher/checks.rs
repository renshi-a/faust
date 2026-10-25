//! The gate every version passes before it may be used: build, tests and the app's
//! self-test. Any failure means the version is not used. Whether tests and the self-test run
//! comes from the shipped `faust.toml`, not from the version's own, which may have been changed.
//!
//! The source may have been rewritten, and building runs code from it (build scripts, proc
//! macros, the tests), so each step runs in the sandbox (see `sandbox.rs`).

use std::path::Path;
use std::process::Command;

use anyhow::{Result, anyhow, bail};

use super::paths::Paths;
use super::sandbox::{self, Writable};
use super::toolchain;
use crate::config::{Check, Config};
use crate::proc::{self, Limits, Output};

/// One step of the checks, for progress.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Step {
    Build,
    Tests,
    SelfTest,
}

impl Step {
    /// Generous limits, so a hung build or test can't hold the user up forever.
    fn timeout(self) -> std::time::Duration {
        std::time::Duration::from_secs(match self {
            Step::Build => 45 * 60,
            Step::Tests => 20 * 60,
            Step::SelfTest => 2 * 60,
        })
    }

    pub(crate) fn name(self) -> String {
        match self {
            Step::Build => tr!(en: "Build", ja: "ビルド", de: "Build", it: "Compilazione"),
            Step::Tests => tr!(en: "Tests", ja: "テスト", de: "Tests", it: "Test"),
            Step::SelfTest => tr!(en: "Self-test", ja: "セルフテスト", de: "Selbsttest", it: "Autotest"),
        }
    }
}

/// What every version is checked with.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Gate<'a> {
    pub(crate) paths: &'a Paths,
    /// The Rust release to build with.
    pub(crate) rust: &'a str,
    /// Whether tests and the self-test run (the shipped `[check]`).
    pub(crate) check: &'a Check,
}

/// Builds and checks the source in `dir` as version `id`. `progress` gets the whole check's
/// fraction (0 to 1), the step, and the last line of output.
pub(crate) fn run(
    gate: Gate<'_>,
    dir: &Path,
    id: u32,
    log: &Path,
    limits: Limits<'_>,
    progress: &mut dyn FnMut(f64, Step, &str),
) -> Result<()> {
    let Gate { paths: p, rust, check: gate } = gate;
    let config = Config::load(dir)?;
    let me = std::env::current_exe()?;
    let packages = toolchain::package_count(dir) as f64;
    let tc = toolchain::toolchain(p, rust);
    let env = [
        ("CARGO_HOME", tc.cargo_home.as_os_str().to_owned()),
        ("RUSTUP_HOME", tc.rustup_home.as_os_str().to_owned()),
        ("RUSTUP_TOOLCHAIN", rust.into()),
        ("FAUST_LANG", crate::i18n::current().code().into()),
    ];
    let project_args = |cmd: &mut Command| {
        cmd.arg("--path").arg(dir).arg("--target-dir").arg(&p.target);
    };

    let mut steps: Vec<(Step, f64, Command, f64)> = Vec::new();
    let writable = Writable {
        dirs: vec![dir.to_owned(), p.target.clone()],
        cargo_home: p.cargo_home.clone(),
        secret: vec![p.assistant.clone()],
    };
    let mut build = sandbox::command(&writable, &me);
    build.arg("build").arg("--build-number").arg(id.to_string());
    project_args(&mut build);
    // A build compiles every package up to twice (stage 1 and 2).
    steps.push((Step::Build, 0.7, build, packages * 2.0));
    if gate.tests {
        let mut test = sandbox::command(&writable, &me);
        test.arg("test");
        project_args(&mut test);
        steps.push((Step::Tests, 0.25, test, packages));
    }
    if !gate.selftest.is_empty() {
        let mut selftest = sandbox::command(&writable, &config.executable(dir)?);
        selftest.args(&gate.selftest);
        steps.push((Step::SelfTest, 0.05, selftest, 1.0));
    }
    let total: f64 = steps.iter().map(|s| s.1).sum();

    let mut done = 0.0;
    for (step, weight, mut cmd, mut expected) in steps {
        cmd.current_dir(dir);
        for (k, v) in &env {
            cmd.env(k, v);
        }
        let mut compiled = 0.0;
        let step_limits = Limits { cancel: limits.cancel, timeout: Some(step.timeout()) };
        let result = proc::run(cmd, None, log, step_limits, &mut |o| {
            let Output::Line(l) = o else { return };
            if l.contains("stage 1 skipped") {
                expected = packages;
            }
            if l.trim_start().starts_with("Compiling") {
                compiled += 1.0;
            }
            progress((done + weight * (compiled / expected).min(0.95)) / total, step, l);
        });
        if let Err(e) = result {
            let (name, tail) = (step.name(), proc::tail(log, 40));
            return Err(anyhow!(tr!(
                en: "{name} failed: {e}\n\n{tail}",
                ja: "{name}に失敗しました: {e}\n\n{tail}",
                de: "{name} fehlgeschlagen: {e}\n\n{tail}",
                it: "{name} non riuscito: {e}\n\n{tail}",
            )));
        }
        done += weight;
        progress(done / total, step, "");
    }
    if !config.executable(dir)?.is_file() {
        bail!("the build did not make {}", config.executable(dir)?.display());
    }
    Ok(())
}
