//! Official updates: a newer packaged app carries newer source. It is put on
//! `refs/heads/upstream` and combined with what the user changed: as it is when they changed
//! nothing, by a three-way merge when that is clean, and with the assistant's help when the
//! merge conflicts: it combines the two, or makes the user's requests again on the update.
//! Whatever happens, the user's changes are never thrown away silently.

use anyhow::Result;
use gix::ObjectId;

use super::assistant::Assistant;
use super::controller::{Controller, cancel_title, later_title, short};
use super::job::{self, Plan};
use super::{log, toolchain};
use crate::repo::{MAIN, MergeLabels, Repo, UPSTREAM};
use crate::ui;

impl Controller {
    /// Imports a newer shipped source, and if main does not have upstream yet, offers it.
    pub(super) fn offer_update(&mut self) {
        let result = self.import_shipped().and_then(|_| self.offer_update_now());
        if let Err(e) = result {
            log(&format!("official update: {e:#}"));
        }
    }

    fn offer_update_now(&mut self) -> Result<()> {
        let repo = self.repo()?;
        let (Some(main), Some(upstream)) = (repo.ref_id(MAIN), repo.ref_id(UPSTREAM)) else { return Ok(()) };
        if repo.contains(main, upstream) || self.job.is_some() {
            return Ok(());
        }
        let release = self.shipped.release.clone();
        let v = &release.version;
        let title = tr!(en: "Official update {v}", ja: "公式アップデート {v}", de: "Offizielles Update {v}", it: "Aggiornamento ufficiale {v}");
        let notes = if release.notes.is_empty() { String::new() } else { format!("\n\n{}", release.notes) };
        let building = tr!(en: "Building {title}", ja: "{title} をビルドしています", de: "{title} wird gebaut", it: "Compilazione di {title}");
        let available = tr!(en: "{title} is available", ja: "{title} があります", de: "{title} ist verfügbar", it: "È disponibile {title}");
        let explain = tr!(
            en: "It's built and tested first; then you're asked whether to switch.{notes}",
            ja: "まずビルドとテストをしてから、切り替えるかを聞きます。{notes}",
            de: "Es wird zuerst gebaut und getestet, dann wirst du gefragt, ob du wechseln möchtest.{notes}",
            it: "Prima viene compilato e testato, poi ti verrà chiesto se passare a questa versione.{notes}",
        );
        let build = tr!(en: "Build", ja: "ビルドする", de: "Bauen", it: "Compila");
        if !super::dev_mode() && ui::alert(&available, &explain, &[&build, &later_title()]) != 0 {
            return Ok(());
        }
        if !self.approve_rust(&release.rust) {
            return Ok(());
        }

        // The user's changes: main differs from where it left upstream.
        let untouched = match repo.merge_base(main, upstream) {
            Some(base) => repo.tree_of(base)? == repo.tree_of(main)?,
            None => false,
        };
        if untouched {
            self.start_job(Plan::Build { commit: upstream, title, summary: release.notes.clone() }, &building);
            return Ok(());
        }
        let labels = MergeLabels { base: "previous official", ours: "yours", theirs: "official" };
        let merge = repo.merge(main, upstream, labels)?;
        if merge.conflicts.is_empty() {
            let c = repo.commit(merge.tree, &[main, upstream], &format!("Merge {title}\n"), job::signature_now())?;
            let summary = tr!(
                en: "Your changes and the official update were combined.{notes}",
                ja: "あなたの変更と公式アップデートを合わせました。{notes}",
                de: "Deine Änderungen und das offizielle Update wurden zusammengeführt.{notes}",
                it: "Le tue modifiche e l'aggiornamento ufficiale sono stati uniti.{notes}",
            );
            self.start_job(Plan::Build { commit: c, title, summary }, &building);
            return Ok(());
        }
        self.resolve_conflicts(&repo, main, upstream, merge, title)
    }

    /// Installs the Rust release an update asks for, after asking. False when the user declined.
    /// Asks before an update installs the newer Rust it needs (the job installs it). False when
    /// the user declined.
    fn approve_rust(&self, rust: &str) -> bool {
        if self.p.system_rust || toolchain::installed(&self.p, rust) {
            return true;
        }
        let home = super::paths::Paths::tilde(&self.p.rustup_home);
        let install = tr!(en: "Install", ja: "インストール", de: "Installieren", it: "Installa");
        ui::alert(
            &tr!(en: "Rust needs to be added", ja: "Rust を追加する必要があります", de: "Rust muss hinzugefügt werden", it: "È necessario aggiungere Rust"),
            &tr!(
                en: "This update needs Rust {rust} (about 540 MB, installed in {home}).",
                ja: "このアップデートには Rust {rust} が必要です（約 540 MB、{home} にインストールします）。",
                de: "Dieses Update braucht Rust {rust} (etwa 540 MB, wird in {home} installiert).",
                it: "Questo aggiornamento richiede Rust {rust} (circa 540 MB, installato in {home}).",
            ),
            &[&install, &later_title()],
        ) == 0
    }

    fn resolve_conflicts(
        &mut self,
        repo: &Repo,
        main: ObjectId,
        upstream: ObjectId,
        merge: crate::repo::Merge,
        title: String,
    ) -> Result<()> {
        let list = merge.conflicts.iter().map(|c| format!("• {c}")).collect::<Vec<_>>().join("\n");
        let found = self.assistant().filter(|a| !a.too_old());
        let name = found.as_ref().map_or("", |a| a.name()).to_owned();
        let requests = requests_since(repo, main, upstream);
        let n = requests.len();
        let resolve = tr!(en: "Let {name} Combine Them", ja: "{name} に合わせてもらう", de: "Von {name} zusammenführen lassen", it: "Falle unire da {name}");
        let replay = tr!(en: "Redo Your Requests", ja: "頼んだことをやり直す", de: "Deine Wünsche neu umsetzen", it: "Rifai le tue richieste");
        let official = tr!(en: "Use the Official Version", ja: "公式バージョンにする", de: "Offizielle Version verwenden", it: "Usa la versione ufficiale");
        let later = later_title();
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum Choice {
            Resolve,
            Replay,
            Official,
            Later,
        }
        let mut choices: Vec<(&str, Choice)> = Vec::new();
        let mut explain = String::new();
        if found.is_some() {
            choices.push((&resolve, Choice::Resolve));
            explain += &tr!(
                en: "“{resolve}”: {name} fixes them so that both are kept.\n",
                ja: "「{resolve}」: {name} が両方を残すように直します。\n",
                de: "„{resolve}“: {name} korrigiert sie so, dass beides erhalten bleibt.\n",
                it: "“{resolve}”: {name} li corregge in modo da tenere entrambe le parti.\n",
            );
            if n > 0 {
                choices.push((&replay, Choice::Replay));
                explain += &tr!(
                    en: "“{replay}”: {name} is asked again for the {n} changes you asked for, oldest first, on the official version.\n",
                    ja: "「{replay}」: これまで頼んだ {n} 件を、公式バージョンの上で古い順に {name} にもう一度頼みます。\n",
                    de: "„{replay}“: {name} wird auf der offiziellen Version noch einmal um deine {n} Änderungen gebeten, die älteste zuerst.\n",
                    it: "“{replay}”: a {name} vengono richieste di nuovo le tue {n} modifiche sulla versione ufficiale, dalla più vecchia.\n",
                );
            }
        }
        choices.extend([(official.as_str(), Choice::Official), (later.as_str(), Choice::Later)]);
        let buttons: Vec<&str> = choices.iter().map(|c| c.0).collect();
        let release = self.shipped.release.release;
        // Choosing it starts the assistant, so it is described as in the change dialog.
        let scope = found.as_ref().map_or_else(String::new, |a| format!("\n\n{}", self.assistant_scope(a).trim_end()));
        let choice = ui::alert(
            &tr!(en: "{title} conflicts with your changes", ja: "{title} とあなたの変更が衝突しました", de: "{title} steht im Konflikt mit deinen Änderungen", it: "{title} è in conflitto con le tue modifiche"),
            &tr!(
                en: "Conflicting files:\n{list}\n\n{explain}“{official}”: your changes are set aside; you can get them back from refs/heads/before/{release} and from the versions kept.\n“{later}”: keep using the current version. You'll be asked again next time.{scope}",
                ja: "衝突したファイル:\n{list}\n\n{explain}「{official}」: あなたの変更を外します。外した変更は refs/heads/before/{release} と残っているバージョンから戻せます。\n「{later}」: いまのバージョンを使い続けます。次回また聞きます。{scope}",
                de: "Dateien mit Konflikten:\n{list}\n\n{explain}„{official}“: Deine Änderungen werden beiseitegelegt; du kannst sie aus refs/heads/before/{release} und den aufbewahrten Versionen zurückholen.\n„{later}“: Die aktuelle Version wird weiter verwendet. Beim nächsten Mal wirst du wieder gefragt.{scope}",
                it: "File in conflitto:\n{list}\n\n{explain}“{official}”: le tue modifiche vengono messe da parte; puoi recuperarle da refs/heads/before/{release} e dalle versioni conservate.\n“{later}”: continui a usare la versione attuale. Ti verrà chiesto di nuovo la prossima volta.{scope}",
            ),
            &buttons,
        );
        let chosen = choices.get(choice).map_or(Choice::Later, |c| c.1);
        if chosen == Choice::Replay {
            let assistant = found.expect("the button is shown only when there is an assistant");
            return self.replay_requests(repo, main, upstream, requests, title, assistant);
        }
        if chosen == Choice::Resolve {
            let requests = format_requests(&requests);
            let notes = self.shipped.release.notes.clone();
            let combining = tr!(en: "Combining with {title}", ja: "{title} と合わせています", de: "Zusammenführen mit {title}", it: "Unione con {title}");
            let assistant = found.expect("the button is shown only when there is an assistant");
            // The job writes the merge into the work tree.
            self.keep_hand_edits(repo);
            self.start_job(
                Plan::Resolve {
                    merged: merge.tree,
                    conflicts: merge.conflicts,
                    ours: main,
                    upstream,
                    requests,
                    notes,
                    title,
                    assistant,
                },
                &combining,
            );
        } else if chosen == Choice::Official {
            repo.set_ref(&format!("refs/heads/before/{release}"), main, "set aside for an official version")?;
            let notes = if self.shipped.release.notes.is_empty() {
                String::new()
            } else {
                format!("\n\n{}", self.shipped.release.notes)
            };
            let summary = tr!(
                en: "Your changes were set aside; this is the official version.{notes}",
                ja: "あなたの変更を外して、公式バージョンにしました。{notes}",
                de: "Deine Änderungen wurden beiseitegelegt, das ist die offizielle Version.{notes}",
                it: "Le tue modifiche sono state messe da parte: questa è la versione ufficiale.{notes}",
            );
            let building = tr!(en: "Building {title}", ja: "{title} をビルドしています", de: "{title} wird gebaut", it: "Compilazione di {title}");
            self.start_job(Plan::Build { commit: upstream, title, summary }, &building);
        }
        Ok(())
    }

    /// Says how many requests will be made again and that it takes a while, and on yes, sets
    /// the user's changes aside on `before/<release>` and starts making them again on upstream.
    fn replay_requests(
        &mut self,
        repo: &Repo,
        main: ObjectId,
        upstream: ObjectId,
        requests: Vec<String>,
        title: String,
        assistant: Assistant,
    ) -> Result<()> {
        const LISTED: usize = 10;
        let (n, name, release) = (requests.len(), assistant.name().to_owned(), self.shipped.release.release);
        let mut list: Vec<String> =
            requests.iter().take(LISTED).enumerate().map(|(i, r)| format!("{}. {}", i + 1, short(r, 60))).collect();
        if n > LISTED {
            let more = n - LISTED;
            list.push(tr!(en: "and {more} more", ja: "ほか {more} 件", de: "und {more} weitere", it: "e altre {more}"));
        }
        let list = list.join("\n");
        let scope = self.assistant_scope(&assistant);
        let again = tr!(en: "Ask Again", ja: "もう一度頼む", de: "Erneut bitten", it: "Chiedi di nuovo");
        let answer = ui::alert(
            &tr!(
                en: "Ask {name} again for {n} changes?",
                ja: "{n} 件を {name} にもう一度頼みますか？",
                de: "{name} noch einmal um {n} Änderungen bitten?",
                it: "Chiedere di nuovo a {name} {n} modifiche?",
            ),
            &tr!(
                en: "On {title}, oldest first:\n{list}\n\nEach one is built and tested before the next starts, so this takes about as long, and uses the assistant about as much, as {n} changes. If one fails, it stops there: you can switch to what was done before it, and your changes as they were stay in refs/heads/before/{release}.\n\n{scope}",
                ja: "{title} の上で、古い順に頼みます:\n{list}\n\n1 件ずつビルドとテストを通してから次に進むので、変更を {n} 回頼むのと同じくらいの時間とアシスタントの利用がかかります。途中で失敗したらそこで止め、それまでにできた分に切り替えられます。元の変更は refs/heads/before/{release} に残ります。\n\n{scope}",
                de: "Auf {title}, die älteste zuerst:\n{list}\n\nJede wird gebaut und getestet, bevor die nächste beginnt. Das dauert etwa so lange und braucht den Assistenten etwa so viel wie {n} Änderungen. Schlägt eine fehl, wird dort angehalten: Du kannst zu dem wechseln, was davor fertig wurde, und deine Änderungen, wie sie waren, bleiben in refs/heads/before/{release}.\n\n{scope}",
                it: "Su {title}, dalla più vecchia:\n{list}\n\nOgnuna viene compilata e testata prima di passare alla successiva, quindi ci vuole circa il tempo, e l'uso dell'assistente, di {n} modifiche. Se una non riesce, ci si ferma lì: puoi passare a quanto fatto prima, e le tue modifiche com'erano restano in refs/heads/before/{release}.\n\n{scope}",
            ),
            &[&again, &cancel_title()],
        );
        if answer != 0 {
            return Ok(());
        }
        repo.set_ref(&format!("refs/heads/before/{release}"), main, "set aside to make the requests again")?;
        // The job writes the official version into the work tree.
        self.keep_hand_edits(repo);
        let notes = self.shipped.release.notes.clone();
        let redoing = tr!(en: "Redoing your requests on {title}", ja: "{title} の上で頼んだことをやり直しています", de: "Deine Wünsche werden auf {title} neu umgesetzt", it: "Le tue richieste vengono rifatte su {title}");
        self.start_job(Plan::Replay { upstream, requests, release, notes, title, assistant }, &redoing);
        Ok(())
    }
}

/// What the user asked for on main since it left upstream, oldest first, from the commit
/// messages.
fn requests_since(repo: &Repo, main: ObjectId, upstream: ObjectId) -> Vec<String> {
    let mut commits = repo.commits_since(main, upstream, 200);
    commits.reverse();
    // The walk lists a merge's parents side by side; the times put them in the order they were made.
    commits.sort_by_key(|c| repo.commit_time(*c).unwrap_or(0));
    commits.into_iter().filter_map(|c| job::requests_in(&repo.message_of(c)).map(str::to_owned)).collect()
}

/// The requests as a list, for the assistant.
fn format_requests(requests: &[String]) -> String {
    if requests.is_empty() {
        return "(none recorded)".into();
    }
    requests.iter().map(|r| format!("- {}", r.replace('\n', "\n  "))).collect::<Vec<_>>().join("\n")
}
