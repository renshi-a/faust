//! The controller, on its own thread: starts the current version and watches it, and
//! reacts to the app, the dialogs and the jobs. The other parts of the loop live next door:
//! setup, versions, change, talk, update and uninstall each add their methods to [`Controller`].

use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use gix::ObjectId;

use super::child::{self, Child, ChildEvent};
use super::paths::Paths;
use super::shipped::Shipped;
use super::state::{State, Status, now};
use super::{Event, log, toolchain};
use crate::config::Config;
use crate::proc;
use crate::repo::{MAIN, Repo};
use crate::ui::{self, Button};

/// The faust-link protocol this launcher speaks (docs/protocol.md).
const PROTOCOL: &str = "2";

/// One start of the app.
#[derive(Debug)]
pub(super) struct Run {
    pub(super) serial: u64,
    pub(super) started: Instant,
    pub(super) ready: Option<Instant>,
    pub(super) healthy: bool,
    /// The launcher asked it to quit (to switch versions, or to uninstall).
    pub(super) switching: bool,
    pub(super) killed_for_timeout: bool,
}

#[derive(Debug)]
pub(crate) struct Controller {
    pub(super) p: Paths,
    pub(super) shipped: Shipped,
    pub(super) state: State,
    pub(super) rx: Receiver<Event>,
    pub(super) child: Option<Child>,
    pub(super) run: Option<Run>,
    pub(super) serial: u64,
    /// Set while a job runs; storing true asks it to stop.
    pub(super) job: Option<Arc<AtomicBool>>,
    /// The user quit the app while a job ran: quit when it is done.
    pub(super) app_closed: bool,
    /// How many times in a row a good version failed to start.
    pub(super) boot_failures: u32,
    pub(super) update_checked: bool,
    /// The user chose to uninstall: do it once the app has quit.
    pub(super) uninstalling: bool,
    /// The assistant is answering what the user said to the app.
    pub(super) talking: bool,
    /// The conversation with the app's user so far, to continue it.
    pub(super) talk_session: Option<super::assistant::Session>,
}

// Button titles used in more than one dialog.
pub(super) fn quit_title() -> String {
    tr!(en: "Quit", ja: "終了", de: "Beenden", it: "Esci")
}
pub(super) fn cancel_title() -> String {
    tr!(en: "Cancel", ja: "キャンセル", de: "Abbrechen", it: "Annulla")
}
pub(super) fn later_title() -> String {
    tr!(en: "Later", ja: "あとで", de: "Später", it: "Più tardi")
}
pub(super) fn cant_start_title(app: &str) -> String {
    tr!(en: "{app} can't start", ja: "{app} を開始できません", de: "{app} kann nicht starten", it: "Impossibile avviare {app}")
}

/// The first line of `s`, at most `n` characters, with an ellipsis when something was cut.
pub(super) fn short(s: &str, n: usize) -> String {
    let first = s.lines().next().unwrap_or("");
    let mut out: String = first.chars().take(n).collect();
    if first.chars().count() > n || s.lines().count() > 1 {
        out.push('…');
    }
    out
}

/// At most `n` lines of `s`.
pub(super) fn short_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= n { s.to_owned() } else { format!("{}\n…", lines[..n].join("\n")) }
}

/// "2026-10-24 13:05" in local time.
pub(super) fn date(t: i64) -> String {
    #[repr(C)]
    struct Tm {
        sec: i32,
        min: i32,
        hour: i32,
        mday: i32,
        mon: i32,
        year: i32,
        rest: [i32; 3],
        gmtoff: i64,
        zone: *const u8,
    }
    unsafe extern "C" {
        fn localtime_r(t: *const i64, out: *mut Tm) -> *mut Tm;
    }
    let mut tm =
        Tm { sec: 0, min: 0, hour: 0, mday: 0, mon: 0, year: 0, rest: [0; 3], gmtoff: 0, zone: std::ptr::null() };
    // SAFETY: localtime_r writes into the Tm it is given.
    unsafe { localtime_r(&t, &mut tm) };
    format!("{}-{:02}-{:02} {:02}:{:02}", tm.year + 1900, tm.mon + 1, tm.mday, tm.hour, tm.min)
}

/// `FAUST_TEST_MESSAGES="change,versions"`: as if the app sent these, one per start.
fn test_message() -> Option<String> {
    static MESSAGES: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();
    let queue = MESSAGES.get_or_init(|| {
        Mutex::new(
            super::overrides::get("FAUST_TEST_MESSAGES")
                .unwrap_or_default()
                .split(',')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
        )
    });
    queue.lock().expect("test messages poisoned").pop_front()
}

impl Controller {
    pub(crate) fn new(p: Paths, shipped: Shipped, rx: Receiver<Event>) -> Controller {
        let state = State::load(&p.state);
        Controller {
            p,
            shipped,
            state,
            rx,
            child: None,
            run: None,
            serial: 0,
            job: None,
            app_closed: false,
            boot_failures: 0,
            update_checked: false,
            uninstalling: false,
            talking: false,
            talk_session: None,
        }
    }

    pub(super) fn app_name(&self) -> &str {
        &self.shipped.config.app.name
    }

    pub(super) fn save(&self) {
        if let Err(e) = self.state.save(&self.p.state) {
            log(&format!("can't write state.json: {e:#}"));
        }
    }

    pub(super) fn repo(&self) -> Result<Repo> {
        Repo::open_or_init(&self.p.repo)
    }

    /// The Rust release in use.
    pub(super) fn rust(&self) -> String {
        if self.state.rust.is_empty() { self.shipped_rust() } else { self.state.rust.clone() }
    }

    /// The release the shipped source asks for. With the developer's own rustup (`faust dev`),
    /// it is the name rustup knows that release by, such as `stable`.
    pub(super) fn shipped_rust(&self) -> String {
        let rust = self.shipped.release.rust.clone();
        if !self.p.system_rust {
            return rust;
        }
        toolchain::toolchain(&self.p, &rust).resolve().map_or(rust, |t| t.rust)
    }

    /// The executable of version `id`, as its own `faust.toml` names it.
    pub(super) fn executable(&self, id: u32) -> Option<PathBuf> {
        let dir = self.p.version_dir(id);
        let exe = Config::load(&dir).ok()?.executable(&dir).ok()?;
        exe.is_file().then_some(exe)
    }

    pub(crate) fn run(mut self) {
        let ready = self.state.current().is_some_and(|v| self.executable(v.id).is_some())
            && (self.p.system_rust || toolchain::installed(&self.p, &self.rust()))
            && self.p.repo.join(".git").is_dir();
        if !ready {
            if !self.setup() {
                return ui::quit();
            }
        } else if ui::option_key_down() {
            self.pick_version();
        }
        self.launch();
        self.event_loop();
    }

    /// Waits for one of `accept` (or a test answer).
    pub(super) fn wait_button(&self, accept: &[Button]) -> Button {
        if let Some(a) = ui::test_answer() {
            return match a.as_str() {
                "install" => Button::Install,
                "retry" => Button::Retry,
                "start" => Button::Start,
                _ => Button::Quit,
            };
        }
        loop {
            match self.rx.recv() {
                Ok(Event::Button(b)) if accept.contains(&b) => return b,
                Ok(_) => {}
                Err(_) => return Button::Quit,
            }
        }
    }

    // -----------------------------------------------------------------------
    // Running the app

    pub(super) fn launch(&mut self) {
        let app = self.app_name().to_owned();
        let Some(v) = self.state.current().cloned() else {
            let text = tr!(en: "There's no version to use.", ja: "使えるバージョンがありません。", de: "Es gibt keine Version, die verwendet werden kann.", it: "Non c'è nessuna versione da usare.");
            ui::alert_critical(&cant_start_title(&app), &text, &[&quit_title()]);
            return ui::quit();
        };
        let Some(exe) = self.executable(v.id) else {
            log(&format!("version #{} has no executable", v.id));
            if let Some(prev) = self.state.previous_good(v.id).filter(|id| self.executable(*id).is_some()) {
                self.fall_back_to(prev);
                return self.launch();
            }
            let id = v.id;
            let text = tr!(en: "Version #{id} can't be found.", ja: "バージョン #{id} が見つかりません。", de: "Version #{id} wurde nicht gefunden.", it: "Impossibile trovare la versione #{id}.");
            ui::alert_critical(&cant_start_title(&app), &text, &[&quit_title()]);
            return ui::quit();
        };
        let env: Vec<(&str, OsString)> = vec![
            ("FAUST_PROTOCOL", PROTOCOL.into()),
            ("FAUST_VERSION", v.id.to_string().into()),
            ("FAUST_VERSION_TITLE", v.title.clone().into()),
            ("FAUST_SOURCE_DIR", self.p.repo.clone().into()),
            ("FAUST_LANG", crate::i18n::current().code().into()),
        ];
        self.serial += 1;
        // A new start may be a new version, which can do what the last one couldn't.
        self.talk_session = None;
        match child::spawn(&exe, &env, &self.p.log("app.log"), self.p.devices.clone(), self.serial) {
            Ok(c) => {
                log(&format!("started version #{} (pid {})", v.id, c.pid));
                let _ = std::fs::write(self.p.faust.join("app.pid"), c.pid.to_string());
                self.run = Some(Run {
                    serial: c.serial,
                    started: Instant::now(),
                    ready: None,
                    healthy: false,
                    switching: false,
                    killed_for_timeout: false,
                });
                self.child = Some(c);
                if let Some(v) = self.state.get_mut(v.id) {
                    v.used = now();
                }
                self.save();
            }
            Err(e) => {
                log(&format!("{e:#}"));
                self.run = None;
                self.boot_failed(&format!("{e:#}"));
            }
        }
    }

    fn event_loop(&mut self) {
        loop {
            match self.rx.recv_timeout(Duration::from_millis(500)) {
                Ok(e) => self.handle(e),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            self.check_health();
            self.test_quit();
            if self.child.is_none() && self.job.is_none() && self.app_closed {
                return ui::quit();
            }
        }
    }

    /// `FAUST_TEST_QUIT_AFTER=<seconds>`: once the app has run that long with no job going,
    /// ask it to quit, as at the end of a test.
    fn test_quit(&mut self) {
        let Some(secs) = super::overrides::get("FAUST_TEST_QUIT_AFTER").and_then(|s| s.parse().ok()) else { return };
        if self.job.is_some() || self.app_closed {
            return;
        }
        if let (Some(c), Some(r)) = (&self.child, &self.run)
            && r.healthy
            && !r.switching
            && r.started.elapsed() >= Duration::from_secs(secs)
        {
            log("(test: asking the app to quit)");
            c.send(&serde_json::json!({"type": "quit"}));
            self.app_closed = true;
        }
    }

    fn check_health(&mut self) {
        let check = &self.shipped.config.check;
        let (ready_timeout, healthy_after) =
            (Duration::from_secs(check.ready_timeout_secs), Duration::from_secs(check.healthy_after_secs));
        let signals_ready = check.ready_signal;
        let Some(r) = self.run.as_mut() else { return };
        if r.healthy {
            return;
        }
        let good = match r.ready {
            Some(t) => t.elapsed() >= healthy_after,
            // An app that doesn't use faust-link only has to keep running.
            None => !signals_ready && r.started.elapsed() >= healthy_after,
        };
        if good {
            r.healthy = true;
            return self.started_ok();
        }
        match r.ready {
            None if signals_ready && r.started.elapsed() >= ready_timeout && !r.killed_for_timeout => {
                r.killed_for_timeout = true;
                log("no window in time: stopping it");
                if let Some(c) = &self.child {
                    c.kill();
                }
            }
            _ => {}
        }
    }

    fn handle(&mut self, e: Event) {
        match e {
            Event::Child(serial, ev) if self.run.as_ref().is_some_and(|r| r.serial == serial) => match ev {
                ChildEvent::Ready => {
                    if let Some(r) = self.run.as_mut() {
                        r.ready.get_or_insert_with(Instant::now);
                    }
                    self.tell_assistant();
                }
                ChildEvent::Change(request) => self.ask_change(request),
                ChildEvent::Say { id, text } => self.say(id, &text),
                ChildEvent::Versions => self.pick_version(),
                ChildEvent::ChooseAssistant => {
                    self.choose_assistant();
                }
                ChildEvent::Uninstall => self.ask_uninstall(),
                ChildEvent::TrustDevice { name, key } => self.trust_device(&name, &key),
                ChildEvent::ForgetDevice(key) => {
                    log("forgot a device");
                    super::devices::remove(&self.p.devices, &key);
                }
                ChildEvent::Exited { code, signal } => self.exited(code, signal),
            },
            Event::Button(Button::Cancel) => self.cancel_job(),
            Event::Child(..) | Event::Button(_) => {}
            Event::Reopen => {
                if let Some(c) = &self.child {
                    ui::activate_pid(c.pid);
                }
            }
            Event::Job(out) => self.job_done(out),
            Event::Talked(serial, id, answer) => self.talked(serial, &id, answer),
        }
    }

    /// Cancel on the progress panel.
    fn cancel_job(&self) {
        if let Some(c) = &self.job {
            c.store(true, Ordering::Relaxed);
            ui::progress::update(
                None,
                &tr!(en: "Stopping…", ja: "止めています…", de: "Wird gestoppt…", it: "Interruzione…"),
            );
        }
    }

    /// Removes the folders in `versions/` of versions the state doesn't know, such as one whose
    /// job was cut short when the launcher quit.
    fn remove_stray_versions(&self) {
        let Ok(entries) = std::fs::read_dir(&self.p.versions) else { return };
        for e in entries.flatten() {
            let id = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok());
            if let Some(id) = id.filter(|id| self.state.get(*id).is_none()) {
                let _ = std::fs::remove_dir_all(e.path());
                log(&format!("removed the folder of unused version #{id}"));
            }
        }
    }

    /// The current version kept running: it is good, and main follows it.
    fn started_ok(&mut self) {
        self.boot_failures = 0;
        let Some(v) = self.state.current().cloned() else { return };
        log(&format!("version #{} started fine", v.id));
        if self.state.fallback.take().is_some() {
            self.save();
        }
        if v.status != Status::Good {
            if let Some(m) = self.state.get_mut(v.id) {
                m.status = Status::Good;
            }
            if let (Ok(repo), Ok(commit)) = (self.repo(), ObjectId::from_hex(v.commit.as_bytes())) {
                let _ = repo.set_ref(MAIN, commit, "started fine");
                if let Ok(tree) = repo.tree_of(commit) {
                    let _ = repo.write_index(tree);
                }
            }
            for id in self.state.prune() {
                let _ = std::fs::remove_dir_all(self.p.version_dir(id));
                log(&format!("removed version #{id}"));
            }
            if self.job.is_none() {
                self.remove_stray_versions();
            }
            self.save();
        }
        if !self.update_checked {
            self.update_checked = true;
            self.offer_update();
        }
        if let Some(t) = test_message() {
            log(&format!("(test: the app sends {t})"));
            let ev = match t.as_str() {
                said if said.starts_with("say:") => {
                    ChildEvent::Say { id: "test".into(), text: said["say:".len()..].to_owned() }
                }
                "change" => ChildEvent::Change(None),
                "versions" => ChildEvent::Versions,
                "assistant.choose" => ChildEvent::ChooseAssistant,
                "uninstall" => ChildEvent::Uninstall,
                _ => return,
            };
            self.handle(Event::Child(self.serial, ev));
        }
    }

    fn exited(&mut self, code: Option<i32>, signal: Option<i32>) {
        self.child = None;
        if self.uninstalling {
            self.run = None;
            return self.uninstall();
        }
        let Some(r) = self.run.take() else { return };
        log(&format!("the app exited ({})", child::describe(code, signal)));
        if r.switching {
            return self.launch();
        }
        // Status 0 after the window was up: the user quit, even if it was soon after the start.
        // A version on trial stays on trial and is judged again next time.
        if code == Some(0) && r.ready.is_some() {
            self.app_closed = true;
            return;
        }
        if !r.healthy {
            let why = if r.killed_for_timeout {
                let secs = self.shipped.config.check.ready_timeout_secs;
                tr!(
                    en: "No window appeared within {secs} seconds",
                    ja: "{secs} 秒たってもウインドウが表示されませんでした",
                    de: "Nach {secs} Sekunden war noch kein Fenster zu sehen",
                    it: "Dopo {secs} secondi non è ancora apparsa nessuna finestra",
                )
            } else {
                child::describe(code, signal)
            };
            return self.boot_failed(&why);
        }
        if code == Some(0) {
            // Quit by the user. With a job running, wait for it.
            self.app_closed = true;
            return;
        }
        let tail = proc::tail(&self.p.log("app.log"), 12);
        let prev = self.state.current.and_then(|id| self.state.previous_good(id)).filter(|_| self.job.is_none());
        let again = tr!(en: "Open Again", ja: "もう一度開く", de: "Erneut öffnen", it: "Riapri");
        let back = tr!(en: "Go Back to the Previous Version", ja: "前のバージョンに戻す", de: "Zur vorherigen Version zurück", it: "Torna alla versione precedente");
        let quit = quit_title();
        let mut buttons = vec![again.as_str()];
        if prev.is_some() {
            buttons.push(back.as_str());
        }
        buttons.push(quit.as_str());
        let app = self.app_name().to_owned();
        let title = tr!(en: "{app} stopped", ja: "{app} が停止しました", de: "{app} wurde beendet", it: "{app} si è interrotto");
        let b = ui::alert_critical(&title, &format!("{}\n\n{tail}", child::describe(code, signal)), &buttons);
        match (b, prev) {
            (0, _) => self.launch(),
            (1, Some(prev)) => {
                self.use_version(prev);
                self.launch();
            }
            _ => self.app_closed = true,
        }
    }

    pub(super) fn boot_failed(&mut self, why: &str) {
        let Some(v) = self.state.current().cloned() else { return ui::quit() };
        let tail = proc::tail(&self.p.log("app.log"), 12);
        log(&format!("version #{} can't start: {why}", v.id));
        // Switching versions replaces the work tree, which a running job is editing.
        let fallback = self
            .state
            .fallback
            .or_else(|| self.state.previous_good(v.id))
            .filter(|id| *id != v.id && self.executable(*id).is_some() && self.job.is_none());
        let app = self.app_name().to_owned();
        if v.status == Status::Trial {
            if let Some(prev) = fallback {
                if let Some(m) = self.state.get_mut(v.id) {
                    m.status = Status::Bad;
                }
                let _ = std::fs::remove_dir_all(self.p.version_dir(v.id));
                self.state.current = Some(prev);
                self.state.fallback = None;
                self.save();
                self.reset_worktree();
                self.launch();
                let (id, title) = (v.id, short(&v.title, 40));
                let b = ui::alert(
                    &tr!(
                        en: "The new version couldn't start, so the previous one is back",
                        ja: "新しいバージョンが起動できなかったので、前のバージョンに戻しました",
                        de: "Die neue Version konnte nicht starten, daher läuft wieder die vorherige",
                        it: "La nuova versione non si è avviata, quindi è tornata quella precedente",
                    ),
                    &tr!(
                        en: "Version #{id} ({title}) couldn't start ({why}). Version #{prev} is running now.\n\n{tail}",
                        ja: "バージョン #{id}（{title}）は起動できませんでした（{why}）。いまはバージョン #{prev} で動いています。\n\n{tail}",
                        de: "Version #{id} ({title}) konnte nicht starten ({why}). Jetzt läuft Version #{prev}.\n\n{tail}",
                        it: "La versione #{id} ({title}) non si è avviata ({why}). Ora è in uso la versione #{prev}.\n\n{tail}",
                    ),
                    &["OK", &ui::show_log_title()],
                );
                if b == 1 {
                    ui::open_text(&self.p.log("app.log"));
                }
                return;
            }
        } else {
            self.boot_failures += 1;
            if self.boot_failures < 2 {
                return self.launch();
            }
            if let Some(prev) = fallback {
                self.fall_back_to(prev);
                self.boot_failures = 0;
                self.launch();
                let id = v.id;
                ui::alert(
                    &tr!(en: "Went back to the previous version", ja: "前のバージョンに戻しました", de: "Zur vorherigen Version zurückgekehrt", it: "Ripristinata la versione precedente"),
                    &tr!(
                        en: "Version #{id} couldn't start twice in a row, so version #{prev} is back ({why}).\n\n{tail}",
                        ja: "バージョン #{id} が 2 回続けて起動できなかったので、バージョン #{prev} に戻しました（{why}）。\n\n{tail}",
                        de: "Version #{id} konnte zweimal hintereinander nicht starten, daher läuft wieder Version #{prev} ({why}).\n\n{tail}",
                        it: "La versione #{id} non si è avviata per due volte di seguito, quindi è tornata la versione #{prev} ({why}).\n\n{tail}",
                    ),
                    &["OK"],
                );
                return;
            }
        }
        let title = tr!(en: "{app} couldn't start", ja: "{app} を起動できませんでした", de: "{app} konnte nicht starten", it: "Impossibile avviare {app}");
        let again = tr!(en: "Try Again", ja: "もう一度試す", de: "Erneut versuchen", it: "Riprova");
        match ui::alert_critical(&title, &format!("{why}\n\n{tail}"), &[&again, &ui::show_log_title(), &quit_title()]) {
            0 => self.launch(),
            1 => {
                ui::open_text(&self.p.log("app.log"));
                self.boot_failed(why);
            }
            _ => ui::quit(),
        }
    }

    /// Makes `prev` current after the current version failed, with main and the work tree on
    /// it, so the next change starts from a version that works.
    fn fall_back_to(&mut self, prev: u32) {
        self.use_version(prev);
        self.state.fallback = None;
        self.save();
    }

    /// True (after telling the user) when a job is already running.
    pub(super) fn busy(&self) -> bool {
        if self.job.is_none() {
            return false;
        }
        ui::alert(
            &tr!(en: "Something else is in progress", ja: "ほかの作業をしています", de: "Gerade läuft schon etwas anderes", it: "È già in corso un'altra operazione"),
            &tr!(
                en: "Ask again once the change or build you asked for earlier is done.",
                ja: "前に頼んだ変更かビルドが終わってから、もう一度頼んでください。",
                de: "Frag noch einmal, wenn die Änderung oder der Build von vorhin fertig ist.",
                it: "Chiedi di nuovo quando la modifica o la compilazione richiesta prima sarà finita.",
            ),
            &["OK"],
        );
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortens_for_dialogs() {
        assert_eq!(short("one line", 20), "one line");
        assert_eq!(short("a long first line", 6), "a long…");
        assert_eq!(short("first\nsecond", 20), "first…");
        assert_eq!(short_lines("a\nb\nc", 2), "a\nb\n…");
        assert_eq!(date(0).len(), 16);
    }
}
