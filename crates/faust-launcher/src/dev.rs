//! `faust dev`: packages the working tree and opens it the way a user would, with its own
//! support folder under `target/faust/dev`. By default it uses your rustup instead of
//! installing Rust again, and takes each new working tree as an official update without
//! asking, so the loop stays quick. `--fresh` starts over and installs everything, as a
//! user's first start does.

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

use crate::cli;
use crate::config::Config;
use crate::proc;

pub(crate) fn run(path: &Path, fresh: bool) -> Result<()> {
    let root = path.canonicalize().with_context(|| format!("{} not found", path.display()))?;
    let config = Config::load(&root)?;
    let dev = root.join("target/faust/dev");
    let support = dev.join("support");
    stop_running(&support.join("faust"));
    if fresh {
        let _ = std::fs::remove_dir_all(&support);
    }
    std::fs::create_dir_all(&support)?;

    // A new app has no Cargo.lock yet, and packaging needs one; the first build writes it.
    if !root.join("Cargo.lock").is_file() {
        eprintln!("== build once to write Cargo.lock");
        cli::build_at(&root)?;
    }

    eprintln!("== package the working tree");
    let bundle = cli::package_worktree(&root, &dev)?;
    let exe = bundle.join("Contents/MacOS").join(config.binary(&root)?);
    let log = dev.join("launcher.log");
    let mut cmd = Command::new(&exe);
    cmd.env("FAUST_SUPPORT_DIR", &support)
        .env("FAUST_DEV", "1")
        .stdin(Stdio::null())
        .stdout(std::fs::File::create(&log)?)
        .stderr(std::fs::File::create(dev.join("launcher.err"))?);
    if !fresh {
        cmd.env("FAUST_SYSTEM_RUST", "1");
    }
    let child = cmd.spawn().with_context(|| format!("starting {}", exe.display()))?;
    eprintln!("== opened {} (pid {})", bundle.display(), child.id());
    eprintln!("   launcher log: {}", support.join("faust/logs/launcher.log").display());
    eprintln!("   app log:      {}", support.join("faust/logs/app.log").display());
    Ok(())
}

/// Stops a dev launcher left running from an earlier `faust dev`, and its app. A pid file
/// can outlive its process, so only a process whose executable lies under `dev/` is stopped.
fn stop_running(faust_dir: &Path) {
    let Some(dev) = faust_dir.parent().and_then(Path::parent) else { return };
    let pid =
        |name: &str| std::fs::read_to_string(faust_dir.join(name)).ok().and_then(|s| s.trim().parse::<i32>().ok());
    for (file, signal) in [("app.pid", proc::SIGKILL), ("launcher.pid", proc::SIGTERM)] {
        if let Some(pid) = pid(file)
            && executable_of(pid).is_some_and(|exe| exe.starts_with(dev))
        {
            proc::signal(pid, signal);
        }
    }
    // Give the lock a moment to go.
    std::thread::sleep(std::time::Duration::from_millis(300));
}

unsafe extern "C" {
    fn proc_pidpath(pid: i32, buffer: *mut u8, size: u32) -> i32;
}

/// The executable a process runs, from libproc.
fn executable_of(pid: i32) -> Option<std::path::PathBuf> {
    let mut buf = [0u8; 4096];
    // SAFETY: proc_pidpath writes at most `size` bytes into `buf` and returns the length.
    let n = unsafe { proc_pidpath(pid, buf.as_mut_ptr(), buf.len() as u32) };
    let n = usize::try_from(n).ok().filter(|n| *n > 0)?;
    Some(std::path::PathBuf::from(String::from_utf8_lossy(&buf[..n]).into_owned()))
}
