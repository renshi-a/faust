//! The long work that runs while the app stays usable: let the assistant change the source,
//! commit it, write the commit out to `versions/<n>` and check it there. It ends with an
//! [`Event::Job`]: a candidate to switch to, or why there is none.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gix::ObjectId;

use super::assistant::{self, Assistant, Brief};
use super::checks;
use super::paths::Paths;
use super::state::{Kind, now};
use super::{Event, send};
use crate::proc::Limits;
use crate::repo::{Change, ChangeKind, MAIN, Repo, Signature};
use crate::ui;

/// Rounds of "the build failed, please fix it" after the first try.
const FIX_ROUNDS: usize = 2;

/// What a job does.
#[derive(Debug)]
pub(crate) enum Plan {
    /// Let the assistant do what the user asked.
    Change { request: String, assistant: Assistant },
    /// Check a commit that already exists (an official release, a clean merge).
    Build { commit: ObjectId, title: String, summary: String },
    /// An official update that conflicts: the assistant resolves the markers, then check.
    Resolve {
        merged: ObjectId,
        conflicts: Vec<String>,
        ours: ObjectId,
        upstream: ObjectId,
        requests: String,
        notes: String,
        title: String,
        assistant: Assistant,
    },
}

/// A version ready to be switched to.
#[derive(Debug)]
pub(crate) struct Candidate {
    pub(crate) id: u32,
    pub(crate) commit: ObjectId,
    pub(crate) kind: Kind,
    pub(crate) title: String,
    pub(crate) summary: String,
    pub(crate) changes: Vec<Change>,
    pub(crate) diff: PathBuf,
    /// The Rust release it was built with.
    pub(crate) rust: String,
}

#[derive(Debug)]
pub(crate) enum Outcome {
    Candidate(Candidate),
    Failed { title: String, detail: String, log: PathBuf },
    Cancelled,
}

#[derive(Debug)]
pub(crate) struct Spec {
    pub(crate) id: u32,
    pub(crate) rust: String,
    pub(crate) brief: Brief,
    /// Paths the assistant must leave alone (from the shipped `faust.toml`).
    pub(crate) protect: Vec<String>,
    /// What a version must pass (from the shipped `faust.toml`).
    pub(crate) gate: crate::config::Check,
    pub(crate) plan: Plan,
    pub(crate) cancel: Arc<AtomicBool>,
}

/// Runs the job on its own thread.
pub(crate) fn start(p: Paths, spec: Spec) {
    std::thread::spawn(move || {
        let id = spec.id;
        let cancel = spec.cancel.clone();
        let out = run(&p, spec);
        if out.is_err() {
            // Nothing will use what was checked out and built; the logs and `try/<n>` stay.
            let _ = std::fs::remove_dir_all(p.version_dir(id));
        }
        let out = match out {
            Err(_) if cancel.load(Ordering::Relaxed) => Outcome::Cancelled,
            Err(f) => Outcome::Failed { title: f.title, detail: f.detail, log: p.log(&format!("{id:04}-build.log")) },
            Ok(c) => Outcome::Candidate(c),
        };
        send(Event::Job(out));
    });
}

/// Why a job ended without a candidate.
#[derive(Debug)]
struct Fail {
    title: String,
    detail: String,
}

fn fail(title: String) -> impl Fn(anyhow::Error) -> Fail {
    move |e| Fail { title: title.clone(), detail: format!("{e:#}") }
}

pub(crate) const CHANGE_SIGNATURE: Signature<'static> = Signature { name: "Faust", email: "faust@localhost", time: 0 };

pub(crate) fn signature_now() -> Signature<'static> {
    Signature { time: now(), ..CHANGE_SIGNATURE }
}

/// The commit message of a change. [`requests_in`] reads the request back.
fn change_message(request: &str, summary: &str, assistant: &Assistant) -> String {
    let first: String = request.lines().next().unwrap_or("").chars().take(60).collect();
    format!("{first}\n\nRequest:\n{request}\n\nSummary:\n{summary}\n\nAssistant: {}\n", assistant.version)
}

/// The request in a change's commit message, if it is one.
pub(crate) fn requests_in(message: &str) -> Option<&str> {
    let rest = message.split_once("\n\nRequest:\n")?.1;
    Some(rest.split("\n\nSummary:").next().unwrap_or(rest).trim())
}

/// A step of the work: on the panel's status line, and in its log.
fn say(text: &str) {
    ui::progress::update(None, text);
    ui::progress::log(&format!("== {text}"));
}

fn cant_commit() -> String {
    tr!(en: "Couldn't commit", ja: "コミットできませんでした", de: "Commit fehlgeschlagen", it: "Impossibile eseguire il commit")
}

fn cant_read_changes() -> String {
    tr!(en: "Couldn't read the changes", ja: "変更を読めませんでした", de: "Die Änderungen konnten nicht gelesen werden", it: "Impossibile leggere le modifiche")
}

fn update_check_failed() -> String {
    tr!(
        en: "The official update didn't pass the build or the tests",
        ja: "公式の更新がビルドかテストを通りませんでした",
        de: "Das offizielle Update hat den Build oder die Tests nicht bestanden",
        it: "L'aggiornamento ufficiale non ha superato la compilazione o i test",
    )
}

fn run(p: &Paths, spec: Spec) -> Result<Candidate, Fail> {
    let repo = Repo::open_or_init(&p.repo).map_err(fail(tr!(
        en: "Couldn't open the repository",
        ja: "リポジトリを開けませんでした",
        de: "Das Repository konnte nicht geöffnet werden",
        it: "Impossibile aprire il repository",
    )))?;
    let main = repo
        .ref_id(MAIN)
        .ok_or_else(|| Fail { title: cant_read_changes(), detail: "refs/heads/main is missing".into() })?;
    let main_tree = repo.tree_of(main).map_err(fail(cant_read_changes()))?;
    let limits = Limits { cancel: &spec.cancel, timeout: None };
    let id = spec.id;
    let build_log = p.log(&format!("{id:04}-build.log"));
    let assistant_log = p.log(&format!("{id:04}-assistant.log"));
    if !p.system_rust && !super::toolchain::installed(p, &spec.rust) {
        let rust = &spec.rust;
        say(
            &tr!(en: "Adding Rust {rust}…", ja: "Rust {rust} を追加しています…", de: "Rust {rust} wird hinzugefügt…", it: "Aggiunta di Rust {rust}…"),
        );
        super::toolchain::add_release(p, rust, limits, &mut ui::progress::log)
            .map_err(fail(super::toolchain::not_installed(&format!("Rust {rust}"))))?;
    }
    let _ = std::fs::remove_file(&build_log);
    let try_ref = format!("refs/heads/try/{id:04}");

    // Writes `commit` out to versions/<id> and checks it there.
    let check = |commit: ObjectId| -> anyhow::Result<()> {
        let tree = repo.tree_of(commit)?;
        let dir = p.version_dir(id);
        let _ = std::fs::remove_dir_all(&dir);
        repo.checkout(tree, &dir)?;
        let building = tr!(en: "Building…", ja: "ビルドしています…", de: "Build läuft…", it: "Compilazione…");
        say(&building);
        ui::progress::update(Some(0.0), &building);
        let gate = checks::Gate { paths: p, rust: &spec.rust, check: &spec.gate };
        checks::run(gate, &dir, id, &build_log, limits, &mut |f, step, line| {
            let line: String = line.chars().take(70).collect();
            ui::progress::update(Some(f), &format!("{}: {line}", step.name()));
        })
    };

    let (commit, kind, title, summary) = match spec.plan {
        Plan::Change { request, assistant } => {
            let name = assistant.name();
            say(&tr!(
                en: "{name} is making the changes…",
                ja: "{name} が変更しています…",
                de: "{name} nimmt die Änderungen vor…",
                it: "{name} sta apportando le modifiche…",
            ));
            let couldnt = tr!(
                en: "{name} couldn't make the changes",
                ja: "{name} が変更できませんでした",
                de: "{name} konnte die Änderungen nicht vornehmen",
                it: "{name} non è riuscito ad apportare le modifiche",
            );
            let mut out = assistant::run(
                &assistant,
                &p.repo,
                &spec.brief.change(&request),
                None,
                &assistant_log,
                limits,
                &mut ui::progress::log,
            )
            .map_err(fail(couldnt))?;
            let mut round = 0;
            loop {
                restore_protected(&repo, main_tree, p, &spec.protect).map_err(fail(cant_read_changes()))?;
                let tree = repo.tree_from_dir(&p.repo).map_err(fail(cant_read_changes()))?;
                if tree == main_tree {
                    let title = tr!(en: "The source didn't change", ja: "ソースは変わりませんでした", de: "Der Quellcode hat sich nicht geändert", it: "Il codice sorgente non è cambiato");
                    return Err(Fail { title, detail: out.summary });
                }
                let commit = repo
                    .commit(tree, &[main], &change_message(&request, &out.summary, &assistant), signature_now())
                    .map_err(fail(cant_commit()))?;
                repo.set_ref(&try_ref, commit, "candidate").map_err(fail(cant_commit()))?;
                match check(commit) {
                    Ok(()) => break (commit, Kind::Change, request.clone(), out.summary),
                    Err(e) if round < FIX_ROUNDS && !spec.cancel.load(Ordering::Relaxed) => {
                        round += 1;
                        let name = assistant.name();
                        say(&tr!(
                            en: "The build or the tests failed, so {name} is fixing it (try {round})…",
                            ja: "ビルドかテストが通らないので、{name} が直しています（{round} 回目）…",
                            de: "Build oder Tests sind fehlgeschlagen, {name} korrigiert das (Versuch {round})…",
                            it: "La compilazione o i test non sono riusciti, {name} sta correggendo (tentativo {round})…",
                        ));
                        let couldnt = tr!(
                            en: "{name} couldn't fix it",
                            ja: "{name} が直せませんでした",
                            de: "{name} konnte es nicht korrigieren",
                            it: "{name} non è riuscito a correggere",
                        );
                        let prompt = spec.brief.fix(&format!("{e:#}"));
                        out = assistant::run(
                            &assistant,
                            &p.repo,
                            &prompt,
                            out.session.take(),
                            &assistant_log,
                            limits,
                            &mut ui::progress::log,
                        )
                        .map_err(fail(couldnt))?;
                    }
                    Err(e) => {
                        let title = tr!(
                            en: "The build or the tests failed",
                            ja: "ビルドかテストが通りませんでした",
                            de: "Build oder Tests sind fehlgeschlagen",
                            it: "La compilazione o i test non sono riusciti",
                        );
                        return Err(Fail { title, detail: format!("{e:#}") });
                    }
                }
            }
        }
        Plan::Build { commit, title, summary } => {
            repo.set_ref(&try_ref, commit, "official update").map_err(fail(cant_commit()))?;
            check(commit).map_err(fail(update_check_failed()))?;
            (commit, Kind::Update, title, summary)
        }
        Plan::Resolve { merged, conflicts, ours, upstream, requests, notes, title, assistant } => {
            repo.checkout(merged, &p.repo).map_err(fail(cant_read_changes()))?;
            let name = assistant.name();
            say(&tr!(
                en: "{name} is combining your changes with the official update…",
                ja: "{name} があなたの変更と公式の更新を合わせています…",
                de: "{name} führt deine Änderungen mit dem offiziellen Update zusammen…",
                it: "{name} sta unendo le tue modifiche con l'aggiornamento ufficiale…",
            ));
            let couldnt = tr!(
                en: "{name} couldn't combine them",
                ja: "{name} が合わせられませんでした",
                de: "{name} konnte sie nicht zusammenführen",
                it: "{name} non è riuscito a unirle",
            );
            let out = assistant::run(
                &assistant,
                &p.repo,
                &spec.brief.merge(&conflicts, &requests, &notes),
                None,
                &assistant_log,
                limits,
                &mut ui::progress::log,
            )
            .map_err(fail(couldnt))?;
            let official = repo.tree_of(upstream).map_err(fail(cant_read_changes()))?;
            restore_protected(&repo, official, p, &spec.protect).map_err(fail(cant_read_changes()))?;
            let tree = repo.tree_from_dir(&p.repo).map_err(fail(cant_read_changes()))?;
            for c in &conflicts {
                let text = repo.text_at(tree, c).map_err(fail(cant_read_changes()))?.unwrap_or_default();
                if text.contains("<<<<<<< ") {
                    let summary = &out.summary;
                    return Err(Fail {
                        title: tr!(
                            en: "Couldn't fully combine it with the official update",
                            ja: "公式の更新と合わせきれませんでした",
                            de: "Das Zusammenführen mit dem offiziellen Update ist nicht ganz gelungen",
                            it: "Non è stato possibile unire del tutto l'aggiornamento ufficiale",
                        ),
                        detail: tr!(
                            en: "{c} still has conflict markers.\n\n{summary}",
                            ja: "{c} にまだ衝突の印があります。\n\n{summary}",
                            de: "{c} enthält noch Konfliktmarkierungen.\n\n{summary}",
                            it: "{c} contiene ancora indicatori di conflitto.\n\n{summary}",
                        ),
                    });
                }
            }
            let message = format!("Merge {title}\n\nSummary:\n{}\n\nAssistant: {}\n", out.summary, assistant.version);
            let commit =
                repo.commit(tree, &[ours, upstream], &message, signature_now()).map_err(fail(cant_commit()))?;
            repo.set_ref(&try_ref, commit, "official update").map_err(fail(cant_commit()))?;
            check(commit).map_err(fail(update_check_failed()))?;
            (commit, Kind::Update, title, out.summary)
        }
    };

    let new_tree = repo.tree_of(commit).map_err(fail(cant_read_changes()))?;
    let (changes, text) = repo.diff(main_tree, new_tree).map_err(fail(cant_read_changes()))?;
    let diff = p.log(&format!("{id:04}.diff"));
    let _ = std::fs::write(&diff, text);
    Ok(Candidate { id, commit, kind, title, summary, changes, diff, rust: spec.rust })
}

/// Puts the protected paths in the work tree back as they are in `reference`.
fn restore_protected(repo: &Repo, reference: ObjectId, p: &Paths, protect: &[String]) -> anyhow::Result<()> {
    if protect.is_empty() {
        return Ok(());
    }
    let tree = repo.tree_from_dir(&p.repo)?;
    let (changes, _) = repo.diff(reference, tree)?;
    let before = repo.files(reference)?;
    for change in changes.iter().filter(|c| crate::config::is_protected(protect, &c.path)) {
        let path = p.repo.join(&change.path);
        match change.kind {
            ChangeKind::Added => std::fs::remove_file(&path)?,
            ChangeKind::Removed | ChangeKind::Modified => {
                let (kind, blob) = before.get(&change.path).expect("a removed or modified file was in the reference");
                if std::fs::symlink_metadata(&path).is_ok() {
                    std::fs::remove_file(&path)?;
                }
                crate::repo::write_entry(&path, *kind, &repo.blob(*blob)?)?;
            }
        }
        ui::progress::log(&format!("== {} is protected: put back as it was", change.path));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip_through_commit_messages() {
        let a = Assistant {
            version: "2.0 (Claude Code)".into(),
            backend: assistant::Backend::ClaudeCode { path: PathBuf::from("/c"), restricted: true },
        };
        let m = change_message("Make the button blue\nand round", "Changed the button.", &a);
        assert!(m.starts_with("Make the button blue\n\nRequest:\n"), "{m}");
        assert_eq!(requests_in(&m), Some("Make the button blue\nand round"));
        assert_eq!(requests_in("Official version 1.0"), None);
    }
}
