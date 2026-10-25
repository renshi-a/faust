//! Official updates: a newer packaged app carries newer source. It is put on
//! `refs/heads/upstream` and combined with what the user changed: as it is when they changed
//! nothing, by a three-way merge when that is clean, and with the assistant's help when the
//! merge conflicts. Whatever happens, the user's changes are never thrown away silently.

use anyhow::Result;
use gix::ObjectId;

use super::controller::{Controller, later_title};
use super::job::{self, Plan};
use super::{assistant, log, toolchain};
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
        let found = assistant::find();
        let resolve = tr!(en: "Let Claude Code Combine Them", ja: "Claude Code に合わせてもらう", de: "Von Claude Code zusammenführen lassen", it: "Falle unire da Claude Code");
        let official = tr!(en: "Use the Official Version", ja: "公式バージョンにする", de: "Offizielle Version verwenden", it: "Usa la versione ufficiale");
        let later = later_title();
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum Choice {
            Resolve,
            Official,
            Later,
        }
        let mut choices: Vec<(&str, Choice)> = Vec::new();
        if found.is_some() {
            choices.push((&resolve, Choice::Resolve));
        }
        choices.extend([(official.as_str(), Choice::Official), (later.as_str(), Choice::Later)]);
        let buttons: Vec<&str> = choices.iter().map(|c| c.0).collect();
        let release = self.shipped.release.release;
        let choice = ui::alert(
            &tr!(en: "{title} conflicts with your changes", ja: "{title} とあなたの変更が衝突しました", de: "{title} steht im Konflikt mit deinen Änderungen", it: "{title} è in conflitto con le tue modifiche"),
            &tr!(
                en: "Conflicting files:\n{list}\n\n“{resolve}”: Claude Code fixes them so that both are kept.\n“{official}”: your changes are set aside; you can get them back from refs/heads/before/{release} and from the versions kept.\n“{later}”: keep using the current version. You'll be asked again next time.",
                ja: "衝突したファイル:\n{list}\n\n「{resolve}」: Claude Code が両方を残すように直します。\n「{official}」: あなたの変更を外します。外した変更は refs/heads/before/{release} と残っているバージョンから戻せます。\n「{later}」: いまのバージョンを使い続けます。次回また聞きます。",
                de: "Dateien mit Konflikten:\n{list}\n\n„{resolve}“: Claude Code korrigiert sie so, dass beides erhalten bleibt.\n„{official}“: Deine Änderungen werden beiseitegelegt; du kannst sie aus refs/heads/before/{release} und den aufbewahrten Versionen zurückholen.\n„{later}“: Die aktuelle Version wird weiter verwendet. Beim nächsten Mal wirst du wieder gefragt.",
                it: "File in conflitto:\n{list}\n\n“{resolve}”: Claude Code li corregge in modo da tenere entrambe le parti.\n“{official}”: le tue modifiche vengono messe da parte; puoi recuperarle da refs/heads/before/{release} e dalle versioni conservate.\n“{later}”: continui a usare la versione attuale. Ti verrà chiesto di nuovo la prossima volta.",
            ),
            &buttons,
        );
        let chosen = choices.get(choice).map_or(Choice::Later, |c| c.1);
        if chosen == Choice::Resolve {
            let requests = self.requests_since(repo, main, upstream);
            let notes = self.shipped.release.notes.clone();
            let combining = tr!(en: "Combining with {title}", ja: "{title} と合わせています", de: "Zusammenführen mit {title}", it: "Unione con {title}");
            let assistant = found.expect("the button is shown only when Claude Code was found");
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

    /// What the user asked for on main since it left upstream, from the commit messages.
    fn requests_since(&self, repo: &Repo, main: ObjectId, upstream: ObjectId) -> String {
        let mut out: Vec<String> = repo
            .commits_since(main, upstream, 200)
            .into_iter()
            .filter_map(|c| job::requests_in(&repo.message_of(c)).map(|r| format!("- {}", r.replace('\n', "\n  "))))
            .collect();
        out.reverse();
        if out.is_empty() { "(none recorded)".into() } else { out.join("\n") }
    }
}
