//! Running long commands (curl, rustup-init, cargo, the assistant): their output goes to a
//! log file, each line is handed to a callback, and they can be stopped from another thread.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

pub(crate) const SIGTERM: i32 = 15;
pub(crate) const SIGKILL: i32 = 9;

unsafe extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

/// Sends a signal; a process that is already gone is not an error.
pub(crate) fn signal(pid: i32, sig: i32) {
    // SAFETY: kill(2) takes any pid and signal number and reports failure through its result.
    unsafe { kill(pid, sig) };
}

/// When to give up on a command.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits<'a> {
    /// Set from another thread to stop the command.
    pub(crate) cancel: &'a AtomicBool,
    pub(crate) timeout: Option<Duration>,
}

impl Limits<'_> {
    /// No timeout, and a cancel flag nobody sets.
    pub(crate) fn none() -> Limits<'static> {
        static NEVER: AtomicBool = AtomicBool::new(false);
        Limits { cancel: &NEVER, timeout: None }
    }
}

/// What a line callback receives.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Output<'a> {
    /// A line of stdout or stderr.
    Line(&'a str),
    /// About four times a second, for progress that is measured some other way.
    Tick,
}

/// Runs `cmd` to the end, writing everything to `log` and handing each line to `on`.
/// Returns the command's stdout (for the assistant's JSON events).
///
/// # Errors
///
/// When the command can't start, exits unsuccessfully, is cancelled or times out.
pub(crate) fn run(
    mut cmd: Command,
    stdin: Option<&str>,
    log: &Path,
    limits: Limits<'_>,
    on: &mut dyn FnMut(Output<'_>),
) -> Result<String> {
    let mut log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .with_context(|| format!("opening {}", log.display()))?;
    let _ = writeln!(log_file, "\n$ {cmd:?}");
    // Its own process group, so stopping it also stops whatever it started.
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let program = cmd.get_program().to_string_lossy().into_owned();
    let mut child = cmd.spawn().map_err(|e| {
        anyhow!(tr!(
            en: "Can't start {program}: {e}",
            ja: "{program} を起動できません: {e}",
            de: "{program} kann nicht gestartet werden: {e}",
            it: "Impossibile avviare {program}: {e}",
        ))
    })?;
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let text = text.to_owned();
        std::thread::spawn(move || {
            let _ = pipe.write_all(text.as_bytes());
        });
    }

    let (tx, rx) = channel::<(bool, Vec<u8>)>();
    let mut readers = Vec::new();
    let pipes: [(bool, Option<Box<dyn Read + Send>>); 2] =
        [(true, child.stdout.take().map(|p| Box::new(p) as _)), (false, child.stderr.take().map(|p| Box::new(p) as _))];
    for (is_stdout, pipe) in pipes {
        let Some(mut pipe) = pipe else { continue };
        let tx = tx.clone();
        readers.push(std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = pipe.read(&mut buf) {
                if n == 0 || tx.send((is_stdout, buf[..n].to_vec())).is_err() {
                    break;
                }
            }
        }));
    }
    drop(tx);

    let started = Instant::now();
    let mut lines = LineSplitter::default();
    let mut stopped: Option<String> = None;
    let status: ExitStatus = loop {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok((is_stdout, bytes)) => {
                let _ = log_file.write_all(&bytes);
                lines.push(is_stdout, &bytes, on);
                continue;
            }
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {}
        }
        on(Output::Tick);
        if stopped.is_none() {
            if limits.cancel.load(Ordering::Relaxed) {
                stopped = Some(tr!(en: "Cancelled", ja: "中止しました", de: "Abgebrochen", it: "Annullato"));
            } else if limits.timeout.is_some_and(|t| started.elapsed() > t) {
                stopped = Some(tr!(
                    en: "Stopped because it took too long",
                    ja: "時間がかかりすぎたので止めました",
                    de: "Gestoppt, weil es zu lange gedauert hat",
                    it: "Interrotto perché richiedeva troppo tempo",
                ));
            }
            if stopped.is_some() {
                let group = -(child.id() as i32);
                signal(group, SIGTERM);
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_secs(5));
                    signal(group, SIGKILL);
                });
            }
        }
        if let Some(status) = child.try_wait()? {
            break status;
        }
    };
    // Whatever it left running would keep its pipes open: stop it, and read what is left for
    // a moment only.
    signal(-(child.id() as i32), SIGTERM);
    let deadline = Instant::now() + Duration::from_secs(2);
    while let Ok((is_stdout, bytes)) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        let _ = log_file.write_all(&bytes);
        lines.push(is_stdout, &bytes, on);
    }
    drop(readers);
    let _ = writeln!(log_file, "\n[exit: {status}]");
    if let Some(why) = stopped {
        bail!(why);
    }
    if !status.success() {
        bail!(tr!(
            en: "{program} failed ({status})",
            ja: "{program} が失敗しました（{status}）",
            de: "{program} ist fehlgeschlagen ({status})",
            it: "{program} non è riuscito ({status})",
        ));
    }
    Ok(String::from_utf8_lossy(&lines.stdout).into_owned())
}

/// Turns the two pipes' chunks into lines, and keeps stdout whole.
#[derive(Default)]
struct LineSplitter {
    stdout: Vec<u8>,
    /// The unfinished line of stderr (0) and stdout (1).
    partial: [Vec<u8>; 2],
}

impl LineSplitter {
    fn push(&mut self, is_stdout: bool, bytes: &[u8], on: &mut dyn FnMut(Output<'_>)) {
        if is_stdout {
            self.stdout.extend_from_slice(bytes);
        }
        let partial = &mut self.partial[usize::from(is_stdout)];
        for &b in bytes {
            if b == b'\n' || b == b'\r' {
                if !partial.is_empty() {
                    on(Output::Line(&String::from_utf8_lossy(partial)));
                    partial.clear();
                }
            } else {
                partial.push(b);
            }
        }
    }
}

/// The last `n` non-empty lines of a log file.
pub(crate) fn tail(log: &Path, n: usize) -> String {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// Total size of the files under `dir`.
pub(crate) fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    entries
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map_or(0, |m| m.len()),
            _ => 0,
        })
        .sum()
}

/// Random bytes from the system (ids for confirmations).
pub(crate) fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .expect("/dev/urandom is readable on macOS");
    b
}

/// The SHA-256 of a file, as lowercase hex.
pub(crate) fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    use sha2::Digest as _;
    let mut h = sha2::Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

/// Lowercase hex.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_logs_and_cancels() {
        let log = std::env::temp_dir().join(format!("faust-proc-{}.log", std::process::id()));
        let mut lines = Vec::new();
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "cat; echo out; echo err >&2"]);
        let out = run(cmd, Some("in\n"), &log, Limits::none(), &mut |o| {
            if let Output::Line(l) = o {
                lines.push(l.to_owned());
            }
        })
        .unwrap();
        assert_eq!(out, "in\nout\n");
        lines.sort();
        assert_eq!(lines, ["err", "in", "out"]);
        assert!(tail(&log, 3).contains("[exit:"));

        let never = AtomicBool::new(false);
        let limits = Limits { cancel: &never, timeout: Some(Duration::from_millis(300)) };
        let mut cmd = Command::new("/bin/sleep");
        cmd.arg("10");
        let t = Instant::now();
        assert!(run(cmd, None, &log, limits, &mut |_| {}).is_err());
        assert!(t.elapsed() < Duration::from_secs(5));
        let _ = std::fs::remove_file(log);
    }

    #[test]
    fn hex_is_lowercase() {
        assert_eq!(hex(&[0x0f, 0xa0]), "0fa0");
    }
}
