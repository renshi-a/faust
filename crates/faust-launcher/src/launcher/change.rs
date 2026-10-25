//! Changes the user asks for: the first confirmation (before the assistant starts), the
//! job, and the second confirmation (before switching to the new version). A trusted device
//! may answer both confirmations too.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde_json::json;

use super::assistant::{self, Brief};
use super::controller::{Controller, cancel_title, short_lines};
use super::job::{self, Candidate, Outcome, Plan};
use super::state::{Kind, Status, Version, now};
use super::versions::switch_title;
use super::{devices, log};
use crate::proc;
use crate::ui;

impl Controller {
    /// What the assistant is told about the app.
    pub(super) fn brief(&self) -> Brief {
        Brief { app: self.app_name().to_owned(), rules: self.shipped.config.repair.rules.display().to_string() }
    }

    /// From the app's menu (`request` is `None`: ask what to change), or from inside the app
    /// (`request` is what it understood; nothing starts until the user confirms it).
    pub(super) fn ask_change(&mut self, request: Option<String>) {
        if self.busy() {
            return;
        }
        // A version still starting may yet be rolled back, which replaces the work tree.
        if self.run.as_ref().is_some_and(|r| !r.healthy) {
            ui::alert(
                &tr!(en: "The app is still starting", ja: "アプリはまだ起動中です", de: "Die App startet noch", it: "L'app si sta ancora avviando"),
                &tr!(
                    en: "Ask again in a few seconds, once it has started.",
                    ja: "起動が終わってから、数秒後にもう一度頼んでください。",
                    de: "Frag in ein paar Sekunden noch einmal, wenn sie gestartet ist.",
                    it: "Chiedi di nuovo tra qualche secondo, quando si sarà avviata.",
                ),
                &["OK"],
            );
            return;
        }
        let Some(assistant) = assistant::find() else {
            ui::alert(
                &tr!(en: "Claude Code can't be found", ja: "Claude Code が見つかりません", de: "Claude Code wurde nicht gefunden", it: "Impossibile trovare Claude Code"),
                &tr!(
                    en: "Changes are made by Claude Code. Install it, sign in, and then ask again.\nhttps://claude.com/claude-code",
                    ja: "変更は Claude Code が行います。インストールしてサインインしてから、もう一度頼んでください。\nhttps://claude.com/claude-code",
                    de: "Änderungen nimmt Claude Code vor. Installiere es, melde dich an und frag dann noch einmal.\nhttps://claude.com/claude-code",
                    it: "Le modifiche vengono apportate da Claude Code. Installalo, accedi e poi chiedi di nuovo.\nhttps://claude.com/claude-code",
                ),
                &["OK"],
            );
            return;
        };
        if !assistant.restricted {
            ui::alert(
                &tr!(en: "Claude Code needs an update", ja: "Claude Code の更新が必要です", de: "Claude Code muss aktualisiert werden", it: "Claude Code va aggiornato"),
                &assistant::outdated(&assistant.version),
                &["OK"],
            );
            return;
        }
        let app = self.app_name().to_owned();
        let from_app = request.is_some();
        let Some(request) = request
            .or_else(|| {
                ui::ask_text(
                    &tr!(en: "What would you like to change?", ja: "どう変えたいですか？", de: "Was möchtest du ändern?", it: "Cosa vuoi cambiare?"),
                    &tr!(
                        en: "Describe what you'd like {app} to do. Claude Code rewrites {app}'s source and makes a new version.",
                        ja: "{app} にしてほしいことを書いてください。Claude Code が {app} のソースを書き換えて、新しいバージョンを作ります。",
                        de: "Beschreib, was {app} tun soll. Claude Code schreibt den Quellcode von {app} um und erstellt eine neue Version.",
                        it: "Descrivi cosa vuoi che faccia {app}. Claude Code riscrive il codice sorgente di {app} e crea una nuova versione.",
                    ),
                    &tr!(en: "Next", ja: "次へ", de: "Weiter", it: "Avanti"),
                    &cancel_title(),
                )
            })
            .filter(|r| !r.is_empty())
        else {
            return;
        };
        if from_app {
            log(&format!("the app asks for a change: {}", super::controller::short(&request, 80)));
        }
        let hand = self.hand_edit_count();
        let (who, repo) = (assistant.title(), super::paths::Paths::tilde(&self.p.repo));
        let hand_note = if hand > 0 {
            tr!(
                en: "{hand} files were changed by hand. They go into the next version too.\n",
                ja: "手で変えたファイルが {hand} 個あります。それも次のバージョンに入ります。\n",
                de: "{hand} Dateien wurden von Hand geändert. Sie kommen ebenfalls in die nächste Version.\n",
                it: "{hand} file sono stati modificati a mano. Entreranno anche nella prossima versione.\n",
            )
        } else {
            String::new()
        };
        let text = tr!(
            en: "Request:\n{request}\n\nAssistant: {who}\nWhat it may change: {repo} (only inside it)\nWhat it can do: read, write and search files. It can't run commands or reach the internet.\n{hand_note}\nAfterwards the new version is built and tested, and you're asked again whether to switch to it.",
            ja: "頼むこと:\n{request}\n\nアシスタント: {who}\n変更してよい場所: {repo}（この中だけ）\nできること: ファイルを読む、書く、探す。コマンドの実行とインターネットへのアクセスはできません。\n{hand_note}\nそのあと新しいバージョンをビルドしてテストし、切り替えるかをもう一度聞きます。",
            de: "Anfrage:\n{request}\n\nAssistent: {who}\nWas geändert werden darf: {repo} (nur darin)\nWas er tun kann: Dateien lesen, schreiben und durchsuchen. Befehle ausführen oder ins Internet gehen kann er nicht.\n{hand_note}\nDanach wird die neue Version gebaut und getestet, und du wirst noch einmal gefragt, ob du wechseln möchtest.",
            it: "Richiesta:\n{request}\n\nAssistente: {who}\nCosa può modificare: {repo} (solo al suo interno)\nCosa può fare: leggere, scrivere e cercare file. Non può eseguire comandi né accedere a internet.\n{hand_note}\nPoi la nuova versione viene compilata e testata, e ti verrà chiesto di nuovo se passare a questa versione.",
        );
        let title = if from_app {
            tr!(
                en: "Ask Claude Code for the change {app} suggested?",
                ja: "{app} から頼まれた変更を Claude Code に頼みますか？",
                de: "Claude Code um die von {app} vorgeschlagene Änderung bitten?",
                it: "Chiedere a Claude Code la modifica suggerita da {app}?",
            )
        } else {
            tr!(en: "Ask Claude Code to make the change?", ja: "Claude Code に変更を頼みますか？", de: "Claude Code um die Änderung bitten?", it: "Chiedere a Claude Code di fare la modifica?")
        };
        let ask = tr!(en: "Ask", ja: "頼む", de: "Bitten", it: "Chiedi");
        let cancel = cancel_title();
        let id = self.send_confirmation("change", &title, &text, &[("ask", &ask), ("cancel", &cancel)]);
        let answer = ui::alert_remote(&id, &title, &text, &[&ask, &cancel], &[Some("ask"), Some("cancel")]);
        self.confirmation_closed(&id);
        if answer != 0 {
            return;
        }
        if hand > 0
            && let Ok(repo) = self.repo()
        {
            self.keep_hand_edits(&repo);
        }
        let working =
            tr!(en: "Changing {app}", ja: "{app} を変更しています", de: "{app} wird geändert", it: "Modifica di {app}");
        self.start_job(Plan::Change { request, assistant }, &working);
    }

    /// Tells the app about a confirmation, so a trusted device can answer it too.
    /// Returns its id, new for every confirmation.
    pub(super) fn send_confirmation(&self, kind: &str, title: &str, text: &str, choices: &[(&str, &str)]) -> String {
        let id = format!("confirm-{}", proc::hex(&proc::random_bytes::<12>()));
        devices::asked(&id, kind, title, text);
        if let Some(c) = &self.child {
            let choices: Vec<_> = choices.iter().map(|(id, label)| json!({"id": id, "label": label})).collect();
            c.send(&json!({"type": "confirmation", "id": id, "kind": kind, "title": title, "text": text, "choices": choices}));
        }
        id
    }

    /// The confirmation was answered (here or on a device).
    pub(super) fn confirmation_closed(&self, id: &str) {
        devices::closed(id);
        if let Some(c) = &self.child {
            c.send(&json!({"type": "confirmation.closed", "id": id}));
        }
    }

    /// A device was paired in the app. Its answers count only if the user says so here.
    pub(super) fn trust_device(&mut self, name: &str, key: &[u8; 32]) {
        let fp = devices::fingerprint(key);
        let name = if name.is_empty() {
            tr!(en: "the device", ja: "このデバイス", de: "das Gerät", it: "il dispositivo")
        } else {
            name.to_owned()
        };
        let app = self.app_name().to_owned();
        let title = tr!(
            en: "Let “{name}” answer {app}'s confirmations?",
            ja: "「{name}」から {app} の確認に答えられるようにしますか？",
            de: "Darf „{name}“ die Rückfragen von {app} beantworten?",
            it: "Consentire a “{name}” di rispondere alle conferme di {app}?",
        );
        let text = tr!(
            en: "You'll be able to choose on that device whether to ask Claude Code for a change and whether to switch to a new version.\n\nCheck that this fingerprint matches the one the device shows:\n{fp}",
            ja: "そのデバイスでも、Claude Code に変更を頼むかと、新しいバージョンに切り替えるかを選べるようになります。\n\nデバイスに表示されている指紋と一致するか確認してください:\n{fp}",
            de: "Du kannst dann auch auf diesem Gerät entscheiden, ob Claude Code um eine Änderung gebeten und ob zu einer neuen Version gewechselt wird.\n\nPrüfe, ob dieser Fingerabdruck mit dem auf dem Gerät übereinstimmt:\n{fp}",
            it: "Potrai scegliere anche da quel dispositivo se chiedere una modifica a Claude Code e se passare a una nuova versione.\n\nVerifica che questa impronta corrisponda a quella mostrata dal dispositivo:\n{fp}",
        );
        let allow = tr!(en: "Allow", ja: "許可", de: "Erlauben", it: "Consenti");
        let deny = tr!(en: "Don't Allow", ja: "許可しない", de: "Nicht erlauben", it: "Non consentire");
        if ui::alert(&title, &text, &[&allow, &deny]) == 0 {
            log(&format!("trusted device \"{name}\" ({fp})"));
            devices::add(&self.p.devices, &name, key);
        } else {
            devices::remove(&self.p.devices, key);
        }
    }

    pub(super) fn start_job(&mut self, plan: Plan, title: &str) {
        let cancel = Arc::new(AtomicBool::new(false));
        let id = self.state.take_id();
        self.save();
        self.job = Some(cancel.clone());
        ui::progress::show(title);
        log(&format!("job #{id} starts: {title}"));
        // An update is built with the Rust its source asks for (the job installs it if needed).
        let rust = if matches!(plan, Plan::Change { .. }) { self.rust() } else { self.shipped_rust() };
        let spec = job::Spec {
            id,
            rust,
            brief: self.brief(),
            protect: self.shipped.config.repair.protect.clone(),
            gate: self.shipped.config.check.clone(),
            plan,
            cancel,
        };
        job::start(self.p.clone(), spec);
    }

    pub(super) fn job_done(&mut self, out: Outcome) {
        self.job = None;
        ui::progress::close();
        match out {
            Outcome::Candidate(c) => self.offer_switch(c),
            Outcome::Failed { title, detail, log: log_file } => {
                log(&format!("the job failed: {title}"));
                self.reset_worktree();
                let detail = short_lines(&detail, 30);
                let text = tr!(
                    en: "{detail}\n\nYou can keep using the current version.",
                    ja: "{detail}\n\nいまのバージョンをそのまま使えます。",
                    de: "{detail}\n\nDu kannst die aktuelle Version weiter verwenden.",
                    it: "{detail}\n\nPuoi continuare a usare la versione attuale.",
                );
                if ui::alert(&title, &text, &["OK", &ui::show_log_title()]) == 1 {
                    ui::open_text(&log_file);
                }
            }
            Outcome::Cancelled => {
                log("the job was cancelled");
                self.reset_worktree();
            }
        }
    }

    fn offer_switch(&mut self, c: Candidate) {
        let (name, id, app) = (&c.title, c.id, self.app_name().to_owned());
        let title = if c.kind == Kind::Update {
            tr!(en: "Switch to {name}?", ja: "{name} に切り替えますか？", de: "Zu {name} wechseln?", it: "Passare a {name}?")
        } else {
            tr!(en: "Switch to the new version (#{id})?", ja: "新しいバージョン（#{id}）に切り替えますか？", de: "Zur neuen Version (#{id}) wechseln?", it: "Passare alla nuova versione (#{id})?")
        };
        // Short, like macOS's own prompts; what changed is behind "Show Changes".
        let text = tr!(
            en: "{app} will quit and open again with the new version.",
            ja: "{app} はいったん終了し、新しいバージョンで開き直します。",
            de: "{app} wird beendet und mit der neuen Version wieder geöffnet.",
            it: "{app} verrà chiuso e riaperto con la nuova versione.",
        );
        let show = tr!(en: "Show Changes", ja: "変更を表示", de: "Änderungen anzeigen", it: "Mostra modifiche");
        let (switch, cancel) = (switch_title(), cancel_title());
        let id = self.send_confirmation(
            "switch",
            &title,
            &device_switch_text(&c, &self.shipped.config.check),
            &[("switch", &switch), ("cancel", &cancel)],
        );
        // `faust dev` takes each new working tree without asking.
        if !(super::dev_mode() && c.kind == Kind::Update) {
            loop {
                match ui::alert_remote(
                    &id,
                    &title,
                    &text,
                    &[&switch, &show, &cancel],
                    &[Some("switch"), None, Some("cancel")],
                ) {
                    1 => ui::open_text(&c.diff),
                    0 => break,
                    _ => {
                        self.confirmation_closed(&id);
                        log(&format!("candidate #{} was not used", c.id));
                        let _ = std::fs::remove_dir_all(self.p.version_dir(c.id));
                        self.reset_worktree();
                        return;
                    }
                }
            }
        }
        self.confirmation_closed(&id);
        self.state.rust.clone_from(&c.rust);
        self.state.versions.push(Version {
            id: c.id,
            commit: c.commit.to_string(),
            status: Status::Trial,
            kind: c.kind,
            title: c.title.clone(),
            made: now(),
            used: now(),
        });
        self.state.fallback = self.state.current;
        self.state.current = Some(c.id);
        self.save();
        // The work tree has the candidate already (changes) or gets it now (updates), after
        // saving anything the user edited there by hand.
        if let Ok(repo) = self.repo()
            && let Ok(tree) = repo.tree_of(c.commit)
        {
            if c.kind == Kind::Update {
                self.keep_hand_edits(&repo);
            }
            let _ = repo.checkout(tree, &self.p.repo);
        }
        log(&format!("trying version #{}", c.id));
        if self.child.is_some() {
            self.restart_into_current();
        } else {
            self.app_closed = false;
            self.launch();
        }
    }
}

/// The switch confirmation for a device: the assistant's summary, the changed files with
/// their line counts, and the checks it passed.
fn device_switch_text(c: &Candidate, gate: &crate::config::Check) -> String {
    let mut lines = Vec::new();
    if !c.summary.trim().is_empty() {
        lines.push(short_lines(c.summary.trim(), 12));
        lines.push(String::new());
    }
    lines.push(tr!(en: "Changed files:", ja: "変更したファイル:", de: "Geänderte Dateien:", it: "File modificati:"));
    for ch in c.changes.iter().take(12) {
        lines.push(format!("{} {}  +{} −{}", ch.kind.mark(), ch.path, ch.added, ch.removed));
    }
    if c.changes.len() > 12 {
        let n = c.changes.len() - 12;
        lines.push(tr!(en: "and {n} more", ja: "ほか {n} 個", de: "und {n} weitere", it: "e altri {n}"));
    }
    lines.push(String::new());
    lines.push(match (gate.tests, !gate.selftest.is_empty()) {
        (true, true) => tr!(
            en: "The build, the tests and the self-test passed.",
            ja: "ビルド、テスト、セルフテストは通りました。",
            de: "Build, Tests und Selbsttest waren erfolgreich.",
            it: "Compilazione, test e autotest superati.",
        ),
        (true, false) => {
            tr!(en: "The build and the tests passed.", ja: "ビルドとテストは通りました。", de: "Build und Tests waren erfolgreich.", it: "Compilazione e test superati.")
        }
        (false, true) => tr!(
            en: "The build and the self-test passed.",
            ja: "ビルドとセルフテストは通りました。",
            de: "Build und Selbsttest waren erfolgreich.",
            it: "Compilazione e autotest superati.",
        ),
        (false, false) => tr!(en: "The build passed.", ja: "ビルドは通りました。", de: "Der Build war erfolgreich.", it: "Compilazione riuscita."),
    });
    lines.push(tr!(
        en: "The full diff is under “Show Changes” on the Mac.",
        ja: "差分の全文は Mac の「変更を表示」で見られます。",
        de: "Alle Änderungen zeigt „Änderungen anzeigen“ auf dem Mac.",
        it: "Le modifiche complete sono in “Mostra modifiche” sul Mac.",
    ));
    lines.join("\n")
}
