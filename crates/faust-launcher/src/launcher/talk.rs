//! What the user says to the app: the assistant answers as the app, reading its source and
//! nothing more, and when the app can't do what was asked yet, it suggests a change. The
//! suggestion then goes through the same confirmation as any other change. Before the first
//! words go to an assistant, the user is asked once whether it may read them.

use serde_json::json;

use super::assistant::{self, Assistant};
use super::controller::{Controller, short};
use super::{Event, log, send};
use crate::ui;

impl Controller {
    /// The app passed on what the user said; `id` goes back with the reply.
    pub(super) fn say(&mut self, id: String, text: &str) {
        if self.talking {
            let busy = tr!(
                en: "I'm still thinking about what you said before.",
                ja: "前に言われたことを、まだ考えています。",
                de: "Ich denke noch über das nach, was du vorhin gesagt hast.",
                it: "Sto ancora pensando a quello che hai detto prima.",
            );
            return self.reply(&id, &busy, None);
        }
        let Some(assistant) = self.assistant().or_else(|| self.no_assistant()) else {
            return self.reply(&id, "", None);
        };
        if assistant.too_old() {
            ui::alert(
                &tr!(en: "Claude Code needs an update", ja: "Claude Code の更新が必要です", de: "Claude Code muss aktualisiert werden", it: "Claude Code va aggiornato"),
                &assistant::outdated(&assistant.version),
                &["OK"],
            );
            return self.reply(&id, "", None);
        }
        let identity = assistant.identity();
        if !self.state.talk_allowed.contains(&identity) {
            if !self.allow_talk(&assistant) {
                log("the user didn't let the assistant answer");
                return self.reply(&id, "", None);
            }
            self.state.talk_allowed.push(identity);
            self.save();
        }
        log(&format!("the user says to the app: {}", short(text, 80)));
        let prompt = self.brief().talk(text, self.talk_session.is_none());
        let (cwd, log_file, serial) = (self.p.repo.clone(), self.p.log("talk.log"), self.serial);
        let resume = self.talk_session.take();
        self.talking = true;
        std::thread::spawn(move || {
            let answer = assistant::talk(&assistant, &cwd, &prompt, resume, &log_file);
            send(Event::Talked(serial, id, answer));
        });
    }

    /// The assistant answered: the reply goes to the app, and a suggested change to the user.
    pub(super) fn talked(&mut self, serial: u64, id: &str, answer: anyhow::Result<assistant::Outcome>) {
        self.talking = false;
        if serial != self.serial {
            // The app it was for has gone.
            return;
        }
        match answer {
            Ok(out) => {
                self.talk_session = out.session;
                let (text, change) = assistant::take_change(&out.summary);
                self.reply(id, &text, change.as_deref());
                if let Some(change) = change {
                    log("the assistant suggests a change");
                    self.ask_change(Some(change));
                }
            }
            Err(e) => {
                log(&format!("the assistant couldn't answer: {e:#}"));
                let why = short(&format!("{e:#}"), 160);
                let text = tr!(
                    en: "I couldn't answer: {why}",
                    ja: "答えられませんでした: {why}",
                    de: "Ich konnte nicht antworten: {why}",
                    it: "Non sono riuscito a rispondere: {why}",
                );
                self.reply(id, &text, None);
            }
        }
    }

    fn reply(&self, id: &str, text: &str, change: Option<&str>) {
        if let Some(c) = &self.child {
            let mut message = json!({"type": "reply", "id": id, "text": text});
            if let Some(change) = change {
                message["change"] = change.into();
            }
            c.send(&message);
        }
    }

    /// Asks whether `a` may read what the user writes in the app, and answer as the app.
    fn allow_talk(&self, a: &Assistant) -> bool {
        let (app, name, who) = (self.app_name().to_owned(), a.name().to_owned(), a.title());
        let repo = super::paths::Paths::tilde(&self.p.repo);
        let sent = match &a.backend {
            assistant::Backend::Model(e) => {
                let host = e.host();
                tr!(
                    en: "What you write and what it reads are sent to {host}.",
                    ja: "書いたことと読んだ内容は {host} に送られます。",
                    de: "Was du schreibst und was er liest, wird an {host} gesendet.",
                    it: "Ciò che scrivi e ciò che legge vengono inviati a {host}.",
                )
            }
            assistant::Backend::ClaudeCode { .. } => tr!(
                en: "What you write and what it reads go to the model Claude Code uses, as when it makes a change.",
                ja: "書いたことと読んだ内容は、変更を頼むときと同じく Claude Code が使うモデルに送られます。",
                de: "Was du schreibst und was er liest, geht an das Modell, das Claude Code verwendet, wie bei einer Änderung.",
                it: "Ciò che scrivi e ciò che legge va al modello usato da Claude Code, come quando fa una modifica.",
            ),
        };
        let title = tr!(
            en: "Let {name} answer what you write in {app}?",
            ja: "{app} に書いたことに {name} が答えてよいですか？",
            de: "Soll {name} auf das antworten, was du in {app} schreibst?",
            it: "Lasciare che {name} risponda a ciò che scrivi in {app}?",
        );
        let text = tr!(
            en: "{name} answers as {app}. When {app} can't do something yet, it suggests a change, and you're asked before anything changes.\n\nAssistant: {who}\nWhat it reads: {repo} (only inside it)\nWhat it can do: read and search files. It can't change them, run commands or reach the internet.\n{sent}",
            ja: "{name} が {app} として答えます。{app} にまだできないことなら変更を提案し、何かを変える前にあなたに確かめます。\n\nアシスタント: {who}\n読む場所: {repo}（この中だけ）\nできること: ファイルを読む、探す。書き換え、コマンドの実行、インターネットへのアクセスはできません。\n{sent}",
            de: "{name} antwortet als {app}. Wenn {app} etwas noch nicht kann, schlägt er eine Änderung vor, und du wirst gefragt, bevor sich etwas ändert.\n\nAssistent: {who}\nWas er liest: {repo} (nur darin)\nWas er tun kann: Dateien lesen und durchsuchen. Ändern, Befehle ausführen oder ins Internet gehen kann er nicht.\n{sent}",
            it: "{name} risponde come {app}. Quando {app} non sa ancora fare qualcosa, propone una modifica, e ti viene chiesto prima che cambi qualcosa.\n\nAssistente: {who}\nCosa legge: {repo} (solo al suo interno)\nCosa può fare: leggere e cercare file. Non può modificarli, eseguire comandi né accedere a internet.\n{sent}",
        );
        let allow = tr!(en: "Allow", ja: "許可する", de: "Erlauben", it: "Consenti");
        let later = tr!(en: "Not Now", ja: "今はしない", de: "Jetzt nicht", it: "Non ora");
        ui::alert(&title, &text, &[&allow, &later]) == 0
    }
}
