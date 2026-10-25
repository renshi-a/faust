//! The app as the launcher's child process: one end of a socket on fd 3 for the
//! `faust-link` messages (one JSON object per line), and its stdout and stderr in a log.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use super::{Event, devices, log, send};
use crate::proc;

/// The fd the app finds the socket on.
const LINK_FD: i32 = 3;
/// What the user says to the app is cut to this many characters.
const MAX_SAY: usize = 4000;
/// How long a write to the app may block before it is given up.
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub(crate) enum ChildEvent {
    /// It showed its first window.
    Ready,
    /// It asks for a change: with the user's words, or `None` to ask them.
    Change(Option<String>),
    /// The user said this to the app, for the assistant to answer as the app.
    Say {
        id: String,
        text: String,
    },
    /// It opens the list of versions.
    Versions,
    /// It opens the window for choosing who makes the changes.
    ChooseAssistant,
    Uninstall,
    /// A device was paired in the app; the user decides here whether it may answer.
    TrustDevice {
        name: String,
        key: [u8; 32],
    },
    ForgetDevice([u8; 32]),
    Exited {
        code: Option<i32>,
        signal: Option<i32>,
    },
}

/// The running app.
#[derive(Debug)]
pub(crate) struct Child {
    pub(crate) pid: i32,
    /// Tells this child's events from an earlier one's.
    pub(crate) serial: u64,
    writer: Arc<Mutex<UnixStream>>,
    /// False once it exited, so a late signal can't hit a reused pid.
    alive: Arc<AtomicBool>,
}

unsafe extern "C" {
    fn dup2(from: i32, to: i32) -> i32;
    fn fcntl(fd: i32, cmd: i32, ...) -> i32;
}
const F_SETFD: i32 = 2;

/// Starts `exe` with the socket on fd 3 and `env` added.
pub(crate) fn spawn(
    exe: &Path,
    env: &[(&str, OsString)],
    log_file: &Path,
    devices_file: PathBuf,
    serial: u64,
) -> Result<Child> {
    let (ours, theirs) = UnixStream::pair()?;
    let fd = theirs.as_raw_fd();
    let mut cmd = Command::new(exe);
    cmd.env("FAUST_LINK_FD", LINK_FD.to_string()).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    // SAFETY: only async-signal-safe calls (dup2, fcntl) run between fork and exec.
    unsafe {
        cmd.pre_exec(move || {
            if fd == LINK_FD {
                // Already in place: just let it survive exec.
                if fcntl(LINK_FD, F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            } else if dup2(fd, LINK_FD) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().map_err(|e| {
        let exe = exe.display();
        anyhow!(tr!(
            en: "Can't start {exe}: {e}",
            ja: "{exe} を起動できません: {e}",
            de: "{exe} kann nicht gestartet werden: {e}",
            it: "Impossibile avviare {exe}: {e}",
        ))
    })?;
    drop(theirs);
    let pid = child.id() as i32;

    let log_out = Arc::new(Mutex::new(std::fs::File::create(log_file)?));
    let pipes: [Option<Box<dyn Read + Send>>; 2] =
        [child.stdout.take().map(|p| Box::new(p) as _), child.stderr.take().map(|p| Box::new(p) as _)];
    for pipe in pipes.into_iter().flatten() {
        let log_out = log_out.clone();
        std::thread::spawn(move || {
            for line in lines(pipe) {
                let _ = writeln!(log_out.lock().expect("app log poisoned"), "{line}");
            }
        });
    }

    // An app that stops reading must not stall the controller, which writes from its thread.
    ours.set_write_timeout(Some(WRITE_TIMEOUT))?;
    let reader = ours.try_clone()?;
    std::thread::spawn(move || {
        for line in lines(reader) {
            if let Ok(message) = serde_json::from_str::<Value>(&line) {
                handle_message(&message, serial, &devices_file);
            }
        }
    });

    let alive = Arc::new(AtomicBool::new(true));
    let still = alive.clone();
    std::thread::spawn(move || {
        let status = child.wait();
        still.store(false, Ordering::SeqCst);
        let (code, signal) = status.map_or((None, None), |s| (s.code(), s.signal()));
        send(Event::Child(serial, ChildEvent::Exited { code, signal }));
    });
    Ok(Child { pid, serial, writer: Arc::new(Mutex::new(ours)), alive })
}

/// The lines `r` gives until it ends. Bytes that aren't UTF-8 are replaced rather than ending
/// the reading: an app whose output nobody drains blocks once the pipe is full.
fn lines(r: impl Read) -> impl Iterator<Item = String> {
    let mut r = BufReader::new(r);
    let mut buf = Vec::new();
    std::iter::from_fn(move || {
        buf.clear();
        match r.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => None,
            Ok(_) => {
                let line = buf.strip_suffix(b"\n").unwrap_or(&buf);
                Some(String::from_utf8_lossy(line.strip_suffix(b"\r").unwrap_or(line)).into_owned())
            }
        }
    })
}

/// One message from the app.
fn handle_message(m: &Value, serial: u64, devices_file: &Path) {
    let text = |k: &str| m.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
    let event = match m.get("type").and_then(Value::as_str) {
        Some("ready") => ChildEvent::Ready,
        Some("change") => ChildEvent::Change(
            m.get("request").and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty()).map(str::to_owned),
        ),
        Some("say") => {
            let said = text("text");
            let said = said.trim();
            if said.is_empty() {
                return;
            }
            ChildEvent::Say { id: text("id").chars().take(64).collect(), text: said.chars().take(MAX_SAY).collect() }
        }
        Some("versions") => ChildEvent::Versions,
        Some("assistant.choose") => ChildEvent::ChooseAssistant,
        Some("uninstall") => ChildEvent::Uninstall,
        Some("device.trust") => {
            let Some(key) = devices::decode_key(&text("public_key")) else { return };
            ChildEvent::TrustDevice { name: text("name").chars().take(60).collect(), key }
        }
        Some("device.forget") => {
            let Some(key) = devices::decode_key(&text("public_key")) else { return };
            ChildEvent::ForgetDevice(key)
        }
        // A device's answer, relayed by the app. Only one signed by a device the user
        // trusts closes the dialog; the app itself can't make one.
        Some("confirmation.answer") => {
            let (id, choice) = (text("id"), text("choice"));
            if devices::verify(devices_file, &id, &choice, &text("public_key"), &text("signature")) {
                log(&format!("confirmation {id}: answered on a device ({choice})"));
                crate::ui::remote_answer(&id, &choice);
            } else {
                log(&format!("confirmation {id}: ignored an answer that no trusted device signed"));
            }
            return;
        }
        _ => return,
    };
    send(Event::Child(serial, event));
}

impl Child {
    /// Writes one message. An app that went away is not an error.
    pub(crate) fn send(&self, message: &Value) {
        let line = format!("{message}\n");
        let _ = self.writer.lock().expect("link writer poisoned").write_all(line.as_bytes());
    }

    /// Asks it to quit; stops it if it is still there after `grace`.
    pub(crate) fn stop(&self, grace: Duration) {
        let (pid, alive) = (self.pid, self.alive.clone());
        std::thread::spawn(move || {
            std::thread::sleep(grace);
            if alive.load(Ordering::SeqCst) {
                proc::signal(pid, proc::SIGTERM);
            }
            std::thread::sleep(Duration::from_secs(3));
            if alive.load(Ordering::SeqCst) {
                proc::signal(pid, proc::SIGKILL);
            }
        });
        // After the timer: an app that stopped reading can't hold this up for long either.
        self.send(&json!({"type": "quit"}));
    }

    pub(crate) fn kill(&self) {
        if self.alive.load(Ordering::SeqCst) {
            proc::signal(self.pid, proc::SIGKILL);
        }
    }
}

/// "exit code 1", "signal 6".
pub(crate) fn describe(code: Option<i32>, signal: Option<i32>) -> String {
    match (code, signal) {
        (Some(c), _) => tr!(en: "exit code {c}", ja: "終了コード {c}", de: "Exit-Code {c}", it: "codice di uscita {c}"),
        (_, Some(s)) => tr!(en: "signal {s}", ja: "シグナル {s}", de: "Signal {s}", it: "segnale {s}"),
        _ => tr!(en: "unknown", ja: "不明", de: "unbekannt", it: "sconosciuto"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_on_past_bytes_that_are_not_utf8() {
        let input: &[u8] = b"one\r\nt\xffo\n{\"type\":\"ready\"}";
        assert_eq!(lines(input).collect::<Vec<_>>(), ["one", "t\u{fffd}o", r#"{"type":"ready"}"#]);
    }
}
