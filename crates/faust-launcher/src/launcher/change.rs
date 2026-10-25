//! Changes the user asks for: the first confirmation (before the assistant starts), the
//! job, and the second confirmation (before switching to the new version). A trusted device
//! may answer both confirmations too.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde_json::json;

use super::assistant::{self, Assistant, Brief, Choice};
use super::controller::{Controller, cancel_title, short_lines};
use super::job::{self, Candidate, Outcome, Plan, Stopped};
use super::model::{self, Endpoint};
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
        let Some(mut assistant) = self.assistant().or_else(|| self.no_assistant()) else { return };
        if assistant.too_old() {
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
                        en: "Describe what you'd like {app} to do. The assistant rewrites {app}'s source and makes a new version.",
                        ja: "{app} にしてほしいことを書いてください。アシスタントが {app} のソースを書き換えて、新しいバージョンを作ります。",
                        de: "Beschreib, was {app} tun soll. Der Assistent schreibt den Quellcode von {app} um und erstellt eine neue Version.",
                        it: "Descrivi cosa vuoi che faccia {app}. L'assistente riscrive il codice sorgente di {app} e crea una nuova versione.",
                    ),
                    "",
                    &next_title(),
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
        loop {
            let (scope, name) = (self.assistant_scope(&assistant), assistant.name().to_owned());
            let text = tr!(
                en: "Request:\n{request}\n\n{scope}{hand_note}\nAfterwards the new version is built and tested, and you're asked again whether to switch to it.",
                ja: "頼むこと:\n{request}\n\n{scope}{hand_note}\nそのあと新しいバージョンをビルドしてテストし、切り替えるかをもう一度聞きます。",
                de: "Anfrage:\n{request}\n\n{scope}{hand_note}\nDanach wird die neue Version gebaut und getestet, und du wirst noch einmal gefragt, ob du wechseln möchtest.",
                it: "Richiesta:\n{request}\n\n{scope}{hand_note}\nPoi la nuova versione viene compilata e testata, e ti verrà chiesto di nuovo se passare a questa versione.",
            );
            let title = if from_app {
                tr!(
                    en: "Ask {name} for the change {app} suggested?",
                    ja: "{app} から頼まれた変更を {name} に頼みますか？",
                    de: "{name} um die von {app} vorgeschlagene Änderung bitten?",
                    it: "Chiedere a {name} la modifica suggerita da {app}?",
                )
            } else {
                tr!(en: "Ask {name} to make the change?", ja: "{name} に変更を頼みますか？", de: "{name} um die Änderung bitten?", it: "Chiedere a {name} di fare la modifica?")
            };
            let ask = tr!(en: "Ask", ja: "頼む", de: "Bitten", it: "Chiedi");
            let other = tr!(en: "Another Assistant…", ja: "ほかのアシスタント…", de: "Anderer Assistent…", it: "Altro assistente…");
            let cancel = cancel_title();
            let id = self.send_confirmation("change", &title, &text, &[("ask", &ask), ("cancel", &cancel)]);
            let answer =
                ui::alert_remote(&id, &title, &text, &[&ask, &cancel, &other], &[Some("ask"), Some("cancel"), None]);
            self.confirmation_closed(&id);
            match answer {
                0 => break,
                2 => {
                    if let Some(a) = self.choose_assistant() {
                        assistant = a;
                    }
                }
                _ => return,
            }
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

    /// What the user is told about an assistant before it starts: who it is, what it may
    /// change, what it can do, and where what it reads is sent. One line each.
    pub(super) fn assistant_scope(&self, a: &Assistant) -> String {
        let (who, repo) = (a.title(), super::paths::Paths::tilde(&self.p.repo));
        let sent = match &a.backend {
            assistant::Backend::Model(e) => {
                let host = e.host();
                tr!(
                    en: "What it reads is sent to {host}.\n",
                    ja: "読んだ内容は {host} に送られます。\n",
                    de: "Was er liest, wird an {host} gesendet.\n",
                    it: "Ciò che legge viene inviato a {host}.\n",
                )
            }
            assistant::Backend::ClaudeCode { .. } => String::new(),
        };
        tr!(
            en: "Assistant: {who}\nWhat it may change: {repo} (only inside it)\nWhat it can do: read, write and search files. It can't run commands or reach the internet.\n{sent}",
            ja: "アシスタント: {who}\n変更してよい場所: {repo}（この中だけ）\nできること: ファイルを読む、書く、探す。コマンドの実行とインターネットへのアクセスはできません。\n{sent}",
            de: "Assistent: {who}\nWas geändert werden darf: {repo} (nur darin)\nWas er tun kann: Dateien lesen, schreiben und durchsuchen. Befehle ausführen oder ins Internet gehen kann er nicht.\n{sent}",
            it: "Assistente: {who}\nCosa può modificare: {repo} (solo al suo interno)\nCosa può fare: leggere, scrivere e cercare file. Non può eseguire comandi né accedere a internet.\n{sent}",
        )
    }

    /// The assistant the user chose, or else Claude Code if it is installed.
    pub(super) fn assistant(&self) -> Option<Assistant> {
        assistant::chosen(&self.p.assistant)
    }

    /// There is no assistant yet: offers to choose a model.
    pub(super) fn no_assistant(&mut self) -> Option<Assistant> {
        let choose = tr!(en: "Choose a Model…", ja: "モデルを選ぶ…", de: "Modell wählen…", it: "Scegli un modello…");
        let answer = ui::alert(
            &tr!(en: "No assistant yet", ja: "アシスタントがまだいません", de: "Noch kein Assistent", it: "Ancora nessun assistente"),
            &tr!(
                en: "Choose a model:\n• ChatGPT or Gemini (with an API key)\n• A model on this Mac (Ollama, LM Studio, …)\n\nTo use Claude Code, install it, sign in and ask again:\nhttps://claude.com/claude-code",
                ja: "モデルを選んでください。\n・ChatGPT や Gemini（API キーが必要）\n・この Mac で動くモデル（Ollama など）\n\nClaude Code を使う場合は、\nインストールしてサインインし、\nもう一度頼んでください。\nhttps://claude.com/claude-code",
                de: "Wähle ein Modell:\n• ChatGPT oder Gemini (mit API-Schlüssel)\n• Ein Modell auf diesem Mac (Ollama, LM Studio, …)\n\nFür Claude Code: installieren, anmelden und noch einmal fragen:\nhttps://claude.com/claude-code",
                it: "Scegli un modello:\n• ChatGPT o Gemini (con una chiave API)\n• Un modello su questo Mac (Ollama, LM Studio, …)\n\nPer usare Claude Code, installalo, accedi e chiedi di nuovo:\nhttps://claude.com/claude-code",
            ),
            &[&choose, "OK"],
        );
        if answer == 0 { self.choose_assistant() } else { None }
    }

    /// Lets the user choose who makes the changes, and keeps the choice. `None` when they
    /// cancel.
    pub(super) fn choose_assistant(&mut self) -> Option<Assistant> {
        let claude = assistant::find().filter(|a| !a.too_old());
        let mut items = Vec::new();
        if let Some(c) = &claude {
            items.push(format!("Claude Code {}", c.short_version()));
        }
        items.extend(model::SERVICES.iter().map(|s| s.name.to_owned()));
        items.push(tr!(
            en: "Another model (Ollama, LM Studio, …)",
            ja: "ほかのモデル（Ollama、LM Studio など）",
            de: "Ein anderes Modell (Ollama, LM Studio, …)",
            it: "Un altro modello (Ollama, LM Studio, …)",
        ));
        let (button, item) = ui::pick(
            &tr!(en: "Who should make the changes?", ja: "だれに変更してもらいますか？", de: "Wer soll die Änderungen vornehmen?", it: "Chi deve apportare le modifiche?"),
            &tr!(
                en: "It can only read, write and search the app's source.\nYou're asked before any new version is used.",
                ja: "ソースの読み書きと検索だけができます。\n新しいバージョンは使う前に確認します。",
                de: "Er darf nur den Quellcode der App lesen, schreiben und durchsuchen.\nVor jeder neuen Version wirst du gefragt.",
                it: "Può solo leggere, scrivere e cercare nel codice sorgente dell'app.\nTi viene chiesto prima di usare ogni nuova versione.",
            ),
            &items,
            &[&tr!(en: "Choose", ja: "選ぶ", de: "Wählen", it: "Scegli"), &cancel_title()],
        );
        if button != 0 {
            return None;
        }
        let item = match claude {
            Some(c) if item == 0 => {
                self.keep_choice(&Choice::ClaudeCode);
                return Some(c);
            }
            Some(_) => item - 1,
            None => item,
        };
        self.choose_model(model::SERVICES.get(item))
    }

    /// Asks where the model is (unless it is one of the `service`s) and which one to use.
    fn choose_model(&mut self, service: Option<&model::Service>) -> Option<Assistant> {
        let before = match assistant::load(&self.p.assistant) {
            Some(Choice::Model(e)) => Some(e),
            _ => None,
        };
        let url = match service {
            Some(s) => s.url.to_owned(),
            None => self.ask_model_url(before.as_ref())?,
        };
        let kept = tr!(
            en: "The key stays on this Mac.\nOnly you can read it.",
            ja: "キーはこの Mac に保存され、\n読めるのはあなただけです。",
            de: "Der Schlüssel bleibt auf diesem Mac.\nNur du kannst ihn lesen.",
            it: "La chiave resta su questo Mac.\nSolo tu puoi leggerla.",
        );
        let (key_title, key_text) = match service {
            Some(s) => {
                let (name, keys) = (s.name, s.keys);
                (
                    tr!(en: "API key for {name}", ja: "{name} の API キー", de: "API-Schlüssel für {name}", it: "Chiave API per {name}"),
                    tr!(
                        en: "Create one at:\n{keys}\n\nThe API is billed separately from a ChatGPT or Gemini subscription.\n\n{kept}",
                        ja: "キーはここで作れます:\n{keys}\n\nAPI の料金は、ChatGPT や Gemini の\n有料プランとは別にかかります。\n\n{kept}",
                        de: "Erstelle einen unter:\n{keys}\n\nDie API wird getrennt von einem ChatGPT- oder Gemini-Abo abgerechnet.\n\n{kept}",
                        it: "Creane una su:\n{keys}\n\nL'API si paga a parte rispetto a un abbonamento a ChatGPT o Gemini.\n\n{kept}",
                    ),
                )
            }
            None => (
                tr!(en: "API key", ja: "API キー", de: "API-Schlüssel", it: "Chiave API"),
                tr!(
                    en: "Leave it empty for a model on this Mac.\n\n{kept}",
                    ja: "この Mac で動くモデルなら、\n空のままで構いません。\n\n{kept}",
                    de: "Für ein Modell auf diesem Mac leer lassen.\n\n{kept}",
                    it: "Lasciala vuota per un modello su questo Mac.\n\n{kept}",
                ),
            ),
        };
        let key = ui::ask_line(
            &key_title,
            &key_text,
            before.as_ref().filter(|e| e.url == url).map_or("", |e| e.key.as_str()),
            true,
            &next_title(),
            &cancel_title(),
        )?;
        if service.is_some() && key.trim().is_empty() {
            return None;
        }
        let mut e = Endpoint { url, model: String::new(), key: key.trim().to_owned() };
        let which = tr!(en: "Which model?", ja: "どのモデルにしますか？", de: "Welches Modell?", it: "Quale modello?");
        let previous = before.as_ref().map_or("", |b| b.model.as_str());
        e.model = match model::models(&e, &self.p.log("assistant.log")).map(model::chat_models) {
            Ok(ids) if !ids.is_empty() => {
                let host = e.host();
                let (button, item) = ui::pick(
                    &which,
                    &tr!(
                        en: "Models on {host}.\nLarger models made for coding work best.",
                        ja: "{host} のモデルです。\nコード向けの大きめのモデルが\nおすすめです。",
                        de: "Modelle auf {host}.\nGrößere Modelle für Programmierung klappen am besten.",
                        it: "Modelli su {host}.\nFunzionano meglio i modelli più grandi pensati per il codice.",
                    ),
                    &ids,
                    &[&tr!(en: "Choose", ja: "選ぶ", de: "Wählen", it: "Scegli"), &cancel_title()],
                );
                if button != 0 {
                    return None;
                }
                ids.get(item)?.clone()
            }
            listed => {
                let why = match listed {
                    Err(err) => format!("{err:#}"),
                    Ok(_) => tr!(en: "it lists none", ja: "一覧が空です", de: "sie ist leer", it: "è vuoto"),
                };
                ui::ask_line(
                    &which,
                    &tr!(
                        en: "The list of models couldn't be read.\nType the model's name.\n\n{why}",
                        ja: "モデルの一覧を取得できませんでした。\n名前を入力してください。\n\n{why}",
                        de: "Die Liste der Modelle war nicht lesbar.\nGib den Namen des Modells ein.\n\n{why}",
                        it: "Impossibile leggere l'elenco dei modelli.\nScrivi il nome del modello.\n\n{why}",
                    ),
                    previous,
                    false,
                    &next_title(),
                    &cancel_title(),
                )?
                .trim()
                .to_owned()
            }
        };
        if e.model.is_empty() {
            return None;
        }
        log(&format!("the assistant is now {} at {}", e.model, e.url));
        self.keep_choice(&Choice::Model(e.clone()));
        Some(Assistant::model(e))
    }

    /// Asks for the address of an OpenAI-compatible API.
    fn ask_model_url(&self, before: Option<&Endpoint>) -> Option<String> {
        let examples = "Ollama: http://localhost:11434/v1\nLM Studio: http://localhost:1234/v1\nllama.cpp: http://localhost:8080/v1\nOpenRouter: https://openrouter.ai/api/v1";
        let url = ui::ask_line(
            &tr!(en: "Where is the model?", ja: "モデルはどこにありますか？", de: "Wo ist das Modell?", it: "Dov'è il modello?"),
            &tr!(
                en: "The address of an OpenAI-compatible API, for example:\n\n{examples}",
                ja: "OpenAI 互換 API のアドレスです。例:\n\n{examples}",
                de: "Die Adresse einer OpenAI-kompatiblen API, zum Beispiel:\n\n{examples}",
                it: "L'indirizzo di un'API compatibile con OpenAI, per esempio:\n\n{examples}",
            ),
            before.as_ref().map_or("http://localhost:11434/v1", |e| e.url.as_str()),
            false,
            &next_title(),
            &cancel_title(),
        )?;
        let url = url.trim().trim_end_matches('/').to_owned();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            ui::alert(
                &tr!(en: "That isn't an address", ja: "アドレスではありません", de: "Das ist keine Adresse", it: "Non è un indirizzo"),
                &tr!(en: "Enter one that starts with http:// or https://.", ja: "http:// か https:// で始まるアドレスを入力してください。", de: "Gib eine ein, die mit http:// oder https:// beginnt.", it: "Inseriscine uno che inizi con http:// o https://."),
                &["OK"],
            );
            return None;
        }
        Some(url)
    }

    fn keep_choice(&self, choice: &Choice) {
        if let Err(e) = assistant::save(&self.p.assistant, choice) {
            log(&format!("couldn't save the assistant: {e}"));
        }
        self.tell_assistant();
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

    /// Tells the app who makes the next change, for it to show. Only Faust's own window
    /// chooses it.
    pub(super) fn tell_assistant(&self) {
        if let Some(c) = &self.child {
            let name = self.assistant().map(|a| a.name().to_owned()).unwrap_or_default();
            c.send(&json!({"type": "assistant", "name": name}));
        }
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
            en: "You'll be able to choose on that device whether to ask the assistant for a change and whether to switch to a new version.\n\nCheck that this fingerprint matches the one the device shows:\n{fp}",
            ja: "そのデバイスでも、アシスタントに変更を頼むかと、新しいバージョンに切り替えるかを選べるようになります。\n\nデバイスに表示されている指紋と一致するか確認してください:\n{fp}",
            de: "Du kannst dann auch auf diesem Gerät entscheiden, ob der Assistent um eine Änderung gebeten und ob zu einer neuen Version gewechselt wird.\n\nPrüfe, ob dieser Fingerabdruck mit dem auf dem Gerät übereinstimmt:\n{fp}",
            it: "Potrai scegliere anche da quel dispositivo se chiedere una modifica all'assistente e se passare a una nuova versione.\n\nVerifica che questa impronta corrisponda a quella mostrata dal dispositivo:\n{fp}",
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
            Outcome::Candidate(c) => {
                if let Some(s) = &c.stopped {
                    self.tell_stopped(c.id, s);
                }
                self.offer_switch(c);
            }
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

    /// A replay stopped part way: what was made again, where it stopped and why, before the
    /// user is asked whether to switch to what was done.
    fn tell_stopped(&self, id: u32, s: &Stopped) {
        let (done, of, report) = (s.done, s.of, &s.report);
        let title = tr!(
            en: "{done} of {of} requests were made again",
            ja: "{of} 件のうち {done} 件をやり直しました",
            de: "{done} von {of} Wünschen wurden neu umgesetzt",
            it: "Rifatte {done} richieste su {of}",
        );
        let text = tr!(
            en: "{report}\n\nNext, you can switch to what was made again.",
            ja: "{report}\n\n次に、やり直せた分までに切り替えるかを聞きます。",
            de: "{report}\n\nAls Nächstes kannst du zu dem wechseln, was neu umgesetzt wurde.",
            it: "{report}\n\nOra puoi passare a quanto è stato rifatto.",
        );
        if ui::alert(&title, &text, &["OK", &ui::show_log_title()]) == 1 {
            ui::open_text(&self.p.log(&format!("{id:04}-build.log")));
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
        super::checks::keep_target(&self.p, &self.p.version_dir(c.id));
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

fn next_title() -> String {
    tr!(en: "Next", ja: "次へ", de: "Weiter", it: "Avanti")
}
