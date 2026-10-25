//! The launcher: what runs when a user opens a packaged app. It is signed and never
//! rewritten. It installs Rust into the app's support folder, keeps the app's source there
//! as a git repository, builds versions of it, runs the current one as a child process, and
//! switches versions or rolls them back. `docs/architecture.md` describes the whole loop.

mod assistant;
mod change;
mod checks;
mod child;
mod controller;
mod devices;
mod job;
pub(crate) mod overrides;
mod paths;
mod sandbox;
mod setup;
mod shipped;
mod state;
mod toolchain;
mod uninstall;
mod update;
mod versions;

use std::io::Write;
use std::os::fd::AsRawFd;
use std::process::ExitCode;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Mutex, OnceLock};

use crate::ui::{self, Button};

pub(crate) use child::ChildEvent;
pub(crate) use job::Outcome;

/// What the controller thread reacts to.
#[derive(Debug)]
pub(crate) enum Event {
    Button(Button),
    /// The user opened the app again while it runs.
    Reopen,
    Child(u64, ChildEvent),
    Job(Outcome),
}

static EVENTS: OnceLock<Mutex<Sender<Event>>> = OnceLock::new();

/// Hands an event to the controller.
pub(crate) fn send(e: Event) {
    if let Some(tx) = EVENTS.get() {
        let _ = tx.lock().expect("event sender poisoned").send(e);
    }
}

static LOG: OnceLock<Mutex<Option<std::fs::File>>> = OnceLock::new();

/// One line in `faust/logs/launcher.log` (and on stderr).
pub(crate) fn log(msg: &str) {
    let line = format!("[{}] {msg}", state::now());
    eprintln!("{line}");
    if let Some(file) = LOG.get()
        && let Some(f) = file.lock().expect("log poisoned").as_mut()
    {
        let _ = writeln!(f, "{line}");
    }
}

/// Started by `faust dev`: official updates (each new working tree) are taken without asking.
pub(crate) fn dev_mode() -> bool {
    overrides::flag("FAUST_DEV")
}

/// Is this executable a packaged app's launcher, opened as an app (no arguments)?
pub(crate) fn is_bundled_launch() -> bool {
    // Finder passes -psn_… on old systems; anything else is the developer's command line.
    let args: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with("-psn_")).collect();
    args.is_empty() && shipped::resources_dir().is_ok_and(|d| d.join("release.json").is_file())
}

unsafe extern "C" {
    fn flock(fd: i32, op: i32) -> i32;
}
const LOCK_EX: i32 = 2;
const LOCK_NB: i32 = 4;

pub(crate) fn main() -> ExitCode {
    ui::load_appkit();
    let shipped = match shipped::Shipped::find() {
        Ok(s) => s,
        Err(e) => {
            ui::init("Faust");
            let title = tr!(en: "This app can't start", ja: "このアプリを起動できません", de: "Diese App kann nicht starten", it: "Impossibile avviare l'app");
            let text = tr!(
                en: "Its source is missing or damaged. Please download the app again.\n\n{e:#}",
                ja: "ソースが見つからないか壊れています。アプリをもう一度ダウンロードしてください。\n\n{e:#}",
                de: "Der Quellcode fehlt oder ist beschädigt. Lade die App bitte erneut herunter.\n\n{e:#}",
                it: "Il codice sorgente manca o è danneggiato. Scarica di nuovo l'app.\n\n{e:#}",
            );
            ui::alert_critical(&title, &text, &["OK"]);
            return ExitCode::FAILURE;
        }
    };
    let p = paths::Paths::new(&shipped.config.app.id);
    let _ = std::fs::create_dir_all(&p.faust);

    // One launcher per support folder. A second one brings the running app forward and leaves.
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(p.faust.join("lock"));
    // SAFETY: flock on a descriptor this process owns.
    let locked = lock.as_ref().is_ok_and(|f| unsafe { flock(f.as_raw_fd(), LOCK_EX | LOCK_NB) } == 0);
    if !locked {
        if let Some(pid) = std::fs::read_to_string(p.faust.join("app.pid")).ok().and_then(|s| s.trim().parse().ok()) {
            ui::activate_pid_now(pid);
        }
        return ExitCode::SUCCESS;
    }
    // Held for the life of the process.
    std::mem::forget(lock);
    let _ = std::fs::write(p.faust.join("launcher.pid"), std::process::id().to_string());

    let log_file = std::fs::OpenOptions::new().create(true).append(true).open(p.log("launcher.log")).ok();
    let _ = LOG.set(Mutex::new(log_file));
    let (tx, rx) = channel();
    let _ = EVENTS.set(Mutex::new(tx));
    ui::init(&shipped.config.app.name);
    log(&format!(
        "Faust {} starts {} ({})",
        env!("CARGO_PKG_VERSION"),
        shipped.config.app.name,
        shipped.release.version
    ));
    std::thread::spawn(move || controller::Controller::new(p, shipped, rx).run());
    ui::run();
    ExitCode::SUCCESS
}
