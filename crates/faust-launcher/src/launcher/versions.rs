//! Choosing, switching and restoring versions, and keeping the work tree in step.

use std::time::Duration;

use gix::ObjectId;

use super::controller::{Controller, cancel_title, date, short};
use super::job;
use super::log;
use super::state::{Status, Version, now};
use crate::repo::{MAIN, Repo};
use crate::ui;

/// How long the app gets to quit by itself before it is stopped.
const QUIT_GRACE: Duration = Duration::from_secs(10);

pub(super) fn switch_title() -> String {
    tr!(en: "Switch", ja: "切り替える", de: "Wechseln", it: "Passa")
}

impl Controller {
    /// Makes `id` (a good version) the current one, with main and the work tree on its commit.
    pub(super) fn use_version(&mut self, id: u32) {
        let Some(v) = self.state.get(id).cloned() else { return };
        self.state.fallback = self.state.current.filter(|c| *c != id);
        self.state.current = Some(id);
        self.save();
        if let (Ok(repo), Ok(commit)) = (self.repo(), ObjectId::from_hex(v.commit.as_bytes())) {
            self.keep_hand_edits(&repo);
            let _ = repo.set_ref(MAIN, commit, "chose a version");
            if let Ok(tree) = repo.tree_of(commit) {
                let _ = repo.checkout(tree, &self.p.repo);
                let _ = repo.write_index(tree);
            }
        }
        log(&format!("switching to version #{id}"));
    }

    /// The list of versions to go back to.
    pub(super) fn pick_version(&mut self) {
        if self.busy() {
            return;
        }
        let title =
            tr!(en: "Choose a Version", ja: "バージョンを選ぶ", de: "Version auswählen", it: "Scegli una versione");
        let mut versions: Vec<Version> = self
            .state
            .versions
            .iter()
            .filter(|v| v.status == Status::Good && self.executable(v.id).is_some())
            .cloned()
            .collect();
        versions.sort_by_key(|v| std::cmp::Reverse(v.id));
        if versions.is_empty() {
            let text = tr!(en: "There are no versions to choose from yet.", ja: "まだ選べるバージョンがありません。", de: "Es gibt noch keine Versionen zur Auswahl.", it: "Non ci sono ancora versioni da scegliere.");
            ui::alert(&title, &text, &["OK"]);
            return;
        }
        let current = tr!(en: " (current)", ja: "（使用中）", de: " (aktuell)", it: " (attuale)");
        let items: Vec<String> = versions
            .iter()
            .map(|v| {
                let mark = if Some(v.id) == self.state.current { current.as_str() } else { "" };
                format!("#{}  {}  {}{mark}", v.id, date(v.made), short(&v.title, 36))
            })
            .collect();
        let text = tr!(
            en: "Choose the version to use. It switches right away, without building.",
            ja: "使うバージョンを選んでください。ビルドせずにすぐ切り替えます。",
            de: "Wähle die Version, die du verwenden möchtest. Es wird sofort gewechselt, ohne neuen Build.",
            it: "Scegli la versione da usare. Il passaggio è immediato, senza compilare.",
        );
        let (b, i) = ui::pick(&title, &text, &items, &[&switch_title(), &cancel_title()]);
        if b != 0 || Some(versions[i].id) == self.state.current {
            return;
        }
        self.use_version(versions[i].id);
        self.restart_into_current();
    }

    /// Asks the running app to quit; [`Controller::exited`] then starts the current version.
    pub(super) fn restart_into_current(&mut self) {
        // Not started yet (chosen at launch) or already gone: launch() comes next anyway.
        if let (Some(c), Some(r)) = (&self.child, self.run.as_mut()) {
            r.switching = true;
            c.stop(QUIT_GRACE);
        }
    }

    /// Asks the running app to quit for good (to uninstall).
    pub(super) fn stop_app(&mut self) -> bool {
        match (&self.child, self.run.as_mut()) {
            (Some(c), Some(r)) => {
                r.switching = true;
                c.stop(QUIT_GRACE);
                true
            }
            _ => false,
        }
    }

    /// Saves files edited by hand in the work tree to `refs/heads/wip/<time>` before they
    /// are replaced.
    pub(super) fn keep_hand_edits(&self, repo: &Repo) {
        let (Some(main), Ok(tree)) = (repo.ref_id(MAIN), repo.tree_from_dir(&self.p.repo)) else { return };
        if repo.tree_of(main).ok() == Some(tree) {
            return;
        }
        if let Ok(c) =
            repo.commit(tree, &[main], "Files edited by hand, kept before switching versions\n", job::signature_now())
        {
            let name = format!("refs/heads/wip/{}", now());
            let _ = repo.set_ref(&name, c, "kept hand edits");
            log(&format!("kept files edited by hand in {name}"));
        }
    }

    /// How many files differ between the work tree and main.
    pub(super) fn hand_edit_count(&self) -> usize {
        let Ok(repo) = self.repo() else { return 0 };
        let (Some(main), Ok(tree)) = (repo.ref_id(MAIN), repo.tree_from_dir(&self.p.repo)) else { return 0 };
        let Ok(main_tree) = repo.tree_of(main) else { return 0 };
        repo.diff(main_tree, tree).map_or(0, |(changes, _)| changes.len())
    }

    /// Puts the work tree back on main (after a candidate was not used).
    pub(super) fn reset_worktree(&self) {
        let Ok(repo) = self.repo() else { return };
        if let Some(main) = repo.ref_id(MAIN)
            && let Ok(tree) = repo.tree_of(main)
        {
            let _ = repo.checkout(tree, &self.p.repo);
            let _ = repo.write_index(tree);
        }
    }
}
