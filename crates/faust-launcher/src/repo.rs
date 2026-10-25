//! The user's copy of the app's source: a plain git repository, handled with gitoxide
//! because `/usr/bin/git` is only a Command Line Tools shim on a Mac without them (running
//! it pops up the installer).
//!
//! Refs:
//!   `refs/heads/upstream`    official releases, one commit each
//!   `refs/heads/main`        the version in use
//!   `refs/heads/try/<n>`     every candidate, kept
//!   `refs/heads/wip/<time>`  files the user edited by hand, saved before they were replaced
//!   `refs/heads/before/<release>`  the user's changes, set aside for an official version

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use gix::ObjectId;
use gix::bstr::ByteSlice;
use gix::objs::tree::{EntryKind, EntryMode};
use gix::refs::transaction::PreviousValue;

pub(crate) const MAIN: &str = "refs/heads/main";
pub(crate) const UPSTREAM: &str = "refs/heads/upstream";

#[derive(Debug)]
pub(crate) struct Repo {
    repo: gix::Repository,
}

/// Who made a commit, and when (seconds since the epoch).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Signature<'a> {
    pub(crate) name: &'a str,
    pub(crate) email: &'a str,
    pub(crate) time: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileKind {
    File,
    Executable,
    Link,
}

/// path -> (kind, blob id)
pub(crate) type Files = BTreeMap<String, (FileKind, ObjectId)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChangeKind {
    Added,
    Removed,
    Modified,
}

impl ChangeKind {
    /// A one-character mark for lists: `+`, `−` or `~`.
    pub(crate) fn mark(self) -> char {
        match self {
            ChangeKind::Added => '+',
            ChangeKind::Removed => '−',
            ChangeKind::Modified => '~',
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Change {
    pub(crate) path: String,
    pub(crate) kind: ChangeKind,
    pub(crate) added: u32,
    pub(crate) removed: u32,
}

#[derive(Debug)]
pub(crate) struct Merge {
    pub(crate) tree: ObjectId,
    /// Paths that could not be merged; the tree has conflict markers in them.
    pub(crate) conflicts: Vec<String>,
}

/// Names that are never part of a snapshot: `.git`, `.DS_Store`, and what the top-level
/// `.gitignore` lists. Only plain names (`target`, `/dist`, `build/`) and suffixes
/// (`*.log`) are understood; that covers what Rust projects put there.
#[derive(Debug)]
pub(crate) struct Ignore {
    top: BTreeSet<String>,
    anywhere: BTreeSet<String>,
    suffixes: Vec<String>,
}

impl Ignore {
    pub(crate) fn read(dir: &Path) -> Ignore {
        let mut ig = Ignore {
            top: BTreeSet::from([".git".to_owned()]),
            anywhere: BTreeSet::from([".DS_Store".to_owned()]),
            suffixes: Vec::new(),
        };
        let text = std::fs::read_to_string(dir.join(".gitignore")).unwrap_or_default();
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('!'))
        {
            let line = line.trim_end_matches('/');
            if let Some(name) = line.strip_prefix('/') {
                ig.top.insert(name.to_owned());
            } else if let Some(suffix) = line.strip_prefix('*') {
                ig.suffixes.push(suffix.to_owned());
            } else {
                ig.anywhere.insert(line.to_owned());
            }
        }
        ig
    }

    /// Also leaves out these relative paths (and everything under them).
    pub(crate) fn with_top(mut self, names: impl IntoIterator<Item = String>) -> Ignore {
        self.top.extend(names);
        self
    }

    fn skips(&self, rel: &str, name: &str) -> bool {
        self.top.contains(rel)
            || self.anywhere.contains(name)
            || self.suffixes.iter().any(|s| name.ends_with(s.as_str()))
    }
}

/// Every file under `dir` (relative paths with '/'), minus ignored ones.
pub(crate) fn walk(dir: &Path, ignore: &Ignore) -> Result<Vec<(String, PathBuf)>> {
    fn visit(dir: &Path, prefix: &str, ignore: &Ignore, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
        let mut entries: Vec<_> =
            std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))?.flatten().collect();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            let rel = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            if ignore.skips(&rel, &name) {
                continue;
            }
            if e.file_type()?.is_dir() {
                visit(&e.path(), &rel, ignore, out)?;
            } else {
                out.push((rel, e.path()));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    visit(dir, "", ignore, &mut out)?;
    Ok(out)
}

fn kind_of(meta: &std::fs::Metadata) -> FileKind {
    if meta.file_type().is_symlink() {
        FileKind::Link
    } else if meta.permissions().mode() & 0o111 != 0 {
        FileKind::Executable
    } else {
        FileKind::File
    }
}

fn entry_kind(k: FileKind) -> EntryKind {
    match k {
        FileKind::File => EntryKind::Blob,
        FileKind::Executable => EntryKind::BlobExecutable,
        FileKind::Link => EntryKind::Link,
    }
}

fn kind_from_mode(m: EntryMode) -> Option<FileKind> {
    match m.kind() {
        EntryKind::Blob => Some(FileKind::File),
        EntryKind::BlobExecutable => Some(FileKind::Executable),
        EntryKind::Link => Some(FileKind::Link),
        EntryKind::Tree | EntryKind::Commit => None,
    }
}

fn read_entry(path: &Path, kind: FileKind) -> Result<Vec<u8>> {
    if kind == FileKind::Link {
        let target = std::fs::read_link(path).with_context(|| path.display().to_string())?;
        Ok(target.as_os_str().as_encoded_bytes().to_vec())
    } else {
        std::fs::read(path).with_context(|| path.display().to_string())
    }
}

/// Writes one file of a tree to `path`, replacing whatever is there.
pub(crate) fn write_entry(path: &Path, kind: FileKind, data: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| parent.display().to_string())?;
    }
    if kind == FileKind::Link {
        use std::os::unix::ffi::OsStrExt;
        std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(data), path)
            .with_context(|| path.display().to_string())?;
    } else {
        std::fs::write(path, data).with_context(|| path.display().to_string())?;
        let mode = if kind == FileKind::Executable { 0o755 } else { 0o644 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

impl Repo {
    /// Opens the repository at `root`, creating it (with HEAD on main) if needed.
    pub(crate) fn open_or_init(root: &Path) -> Result<Repo> {
        if !root.join(".git").is_dir() {
            std::fs::create_dir_all(root)?;
            gix::init(root).with_context(|| format!("creating a repository in {}", root.display()))?;
            std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n")?;
        }
        // Writing a ref also writes the reflog, which needs a committer. Most Macs have no
        // ~/.gitconfig (no git at all), so give one that is used only when git's config has none.
        let opts = gix::open::Options::default().config_overrides([
            "gitoxide.committer.nameFallback=Faust",
            "gitoxide.committer.emailFallback=faust@localhost",
            "gitoxide.author.nameFallback=Faust",
            "gitoxide.author.emailFallback=faust@localhost",
        ]);
        let repo =
            gix::open_opts(root, opts).with_context(|| format!("opening the repository in {}", root.display()))?;
        Ok(Repo { repo })
    }

    /// The repository that holds `dir` (in it or a parent folder), and `dir`'s path inside
    /// its work tree.
    pub(crate) fn discover(dir: &Path) -> Option<(Repo, PathBuf)> {
        let repo = gix::discover(dir).ok()?;
        let workdir = repo.workdir()?.canonicalize().ok()?;
        let rel = dir.canonicalize().ok()?.strip_prefix(&workdir).ok()?.to_owned();
        Some((Repo { repo }, rel))
    }

    /// The tree at `rel` inside `tree` (the tree itself for an empty path).
    pub(crate) fn subtree(&self, tree: ObjectId, rel: &Path) -> Result<ObjectId> {
        if rel.as_os_str().is_empty() {
            return Ok(tree);
        }
        let entry = self
            .repo
            .find_tree(tree)?
            .lookup_entry_by_path(rel)?
            .with_context(|| format!("{} is not in the commit", rel.display()))?;
        Ok(entry.oid().to_owned())
    }

    pub(crate) fn ref_id(&self, name: &str) -> Option<ObjectId> {
        let mut r = self.repo.try_find_reference(name).ok()??;
        r.peel_to_id().ok().map(gix::Id::detach)
    }

    pub(crate) fn set_ref(&self, name: &str, id: ObjectId, why: &str) -> Result<()> {
        self.repo.reference(name, id, PreviousValue::Any, why).with_context(|| format!("setting {name}"))?;
        Ok(())
    }

    pub(crate) fn tree_of(&self, commit: ObjectId) -> Result<ObjectId> {
        let c = self.repo.find_commit(commit).with_context(|| format!("commit {commit} not found"))?;
        Ok(c.tree_id()?.detach())
    }

    /// The committer's time, in seconds since the epoch.
    pub(crate) fn commit_time(&self, commit: ObjectId) -> Result<i64> {
        let c = self.repo.find_commit(commit).with_context(|| format!("commit {commit} not found"))?;
        Ok(c.time()?.seconds)
    }

    pub(crate) fn message_of(&self, commit: ObjectId) -> String {
        self.repo
            .find_commit(commit)
            .ok()
            .and_then(|c| c.message_raw().ok().map(|m| m.to_str_lossy().into_owned()))
            .unwrap_or_default()
    }

    /// Writes every file under `dir` as blobs and returns the tree.
    pub(crate) fn tree_from_dir(&self, dir: &Path) -> Result<ObjectId> {
        self.tree_from_dir_with(dir, &Ignore::read(dir))
    }

    pub(crate) fn tree_from_dir_with(&self, dir: &Path, ignore: &Ignore) -> Result<ObjectId> {
        let mut editor = self.repo.edit_tree(ObjectId::empty_tree(self.repo.object_hash()))?;
        for (rel, path) in walk(dir, ignore)? {
            let meta = std::fs::symlink_metadata(&path).with_context(|| rel.clone())?;
            let kind = kind_of(&meta);
            let blob = self.repo.write_blob(read_entry(&path, kind)?)?;
            editor.upsert(rel.as_str(), entry_kind(kind), blob.detach()).with_context(|| rel.clone())?;
        }
        Ok(editor.write()?.detach())
    }

    pub(crate) fn commit(
        &self,
        tree: ObjectId,
        parents: &[ObjectId],
        message: &str,
        sig: Signature<'_>,
    ) -> Result<ObjectId> {
        let s = gix::actor::Signature {
            name: sig.name.into(),
            email: sig.email.into(),
            time: gix::date::Time { seconds: sig.time, offset: 0 },
        };
        let mut time_buf = gix::date::parse::TimeBuf::default();
        let r = s.to_ref(&mut time_buf);
        let c = self.repo.new_commit_as(r, r, message, tree, parents.iter().copied()).context("committing")?;
        Ok(c.id)
    }

    pub(crate) fn files(&self, tree: ObjectId) -> Result<Files> {
        let t = self.repo.find_tree(tree).with_context(|| format!("tree {tree} not found"))?;
        let mut recorder = gix::traverse::tree::Recorder::default();
        t.traverse().breadthfirst(&mut recorder)?;
        Ok(recorder
            .records
            .into_iter()
            .filter_map(|e| Some((e.filepath.to_str_lossy().into_owned(), (kind_from_mode(e.mode)?, e.oid))))
            .collect())
    }

    pub(crate) fn blob(&self, id: ObjectId) -> Result<Vec<u8>> {
        Ok(self.repo.find_blob(id).with_context(|| format!("blob {id} not found"))?.data.clone())
    }

    /// The text of `path` in `tree`, if it is there.
    pub(crate) fn text_at(&self, tree: ObjectId, path: &str) -> Result<Option<String>> {
        let files = self.files(tree)?;
        files.get(path).map(|(_, id)| self.blob(*id).map(|b| String::from_utf8_lossy(&b).into_owned())).transpose()
    }

    /// Makes `dir` hold exactly the files of `tree` (ignored paths such as `target/` are left
    /// alone). Files that already match are not touched, so cargo's fingerprints stay valid.
    pub(crate) fn checkout(&self, tree: ObjectId, dir: &Path) -> Result<()> {
        let files = self.files(tree)?;
        std::fs::create_dir_all(dir)?;
        for (rel, path) in walk(dir, &Ignore::read(dir))? {
            if files.contains_key(&rel) {
                continue;
            }
            std::fs::remove_file(&path).with_context(|| rel.clone())?;
            // Drop folders that became empty, up to `dir`.
            let mut parent = path.parent();
            while let Some(d) = parent.filter(|d| *d != dir) {
                if std::fs::remove_dir(d).is_err() {
                    break;
                }
                parent = d.parent();
            }
        }
        for (rel, (kind, id)) in &files {
            let path = dir.join(rel);
            let data = self.blob(*id)?;
            if let Ok(meta) = std::fs::symlink_metadata(&path) {
                if meta.is_dir() {
                    std::fs::remove_dir_all(&path).with_context(|| rel.clone())?;
                } else if kind_of(&meta) == *kind && read_entry(&path, *kind).ok().as_deref() == Some(&data[..]) {
                    continue;
                } else {
                    std::fs::remove_file(&path).with_context(|| rel.clone())?;
                }
            }
            write_entry(&path, *kind, &data)?;
        }
        Ok(())
    }

    /// Writes the files of `tree` into `dir`, which must not hold anything else.
    pub(crate) fn export(&self, tree: ObjectId, dir: &Path) -> Result<()> {
        for (rel, (kind, id)) in self.files(tree)? {
            write_entry(&dir.join(&rel), kind, &self.blob(id)?)?;
        }
        Ok(())
    }

    /// Rewrites `.git/index` from `tree`, so `git status` (for people who have git) shows a
    /// clean tree after Faust's commits.
    pub(crate) fn write_index(&self, tree: ObjectId) -> Result<()> {
        let mut index = self.repo.index_from_tree(&tree)?;
        index.write(gix::index::write::Options::default())?;
        Ok(())
    }

    /// What changed from `old` to `new`, and all of it as a unified diff.
    pub(crate) fn diff(&self, old: ObjectId, new: ObjectId) -> Result<(Vec<Change>, String)> {
        use gix::diff::blob::{Algorithm, Diff, InternedInput};
        let (a, b) = (self.files(old)?, self.files(new)?);
        let paths: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
        let mut changes = Vec::new();
        let mut text = String::new();
        for p in paths {
            let (x, y) = (a.get(p), b.get(p));
            if x == y {
                continue;
            }
            let before = x.map(|(_, id)| self.blob(*id)).transpose()?.unwrap_or_default();
            let after = y.map(|(_, id)| self.blob(*id)).transpose()?.unwrap_or_default();
            let kind = match (x, y) {
                (None, _) => ChangeKind::Added,
                (_, None) => ChangeKind::Removed,
                _ => ChangeKind::Modified,
            };
            let (Ok(before), Ok(after)) = (std::str::from_utf8(&before), std::str::from_utf8(&after)) else {
                let _ = writeln!(text, "--- a/{p}\n+++ b/{p}\nBinary files differ");
                changes.push(Change { path: p.clone(), kind, added: 0, removed: 0 });
                continue;
            };
            let input = InternedInput::new(before, after);
            let mut d = Diff::compute(Algorithm::Histogram, &input);
            d.postprocess_lines(&input);
            let from = if x.is_some() { format!("a/{p}") } else { "/dev/null".into() };
            let to = if y.is_some() { format!("b/{p}") } else { "/dev/null".into() };
            let _ = writeln!(text, "--- {from}\n+++ {to}");
            text.push_str(&unified(&d, before, after));
            changes.push(Change { path: p.clone(), kind, added: d.count_additions(), removed: d.count_removals() });
        }
        Ok((changes, text))
    }

    /// Is `ancestor` reachable from `commit` (or the same commit)?
    pub(crate) fn contains(&self, commit: ObjectId, ancestor: ObjectId) -> bool {
        if commit == ancestor {
            return true;
        }
        let Ok(walk) = self.repo.rev_walk([commit]).all() else { return false };
        walk.flatten().any(|info| info.id == ancestor)
    }

    /// The nearest common ancestor of two commits.
    pub(crate) fn merge_base(&self, a: ObjectId, b: ObjectId) -> Option<ObjectId> {
        self.repo.merge_base(a, b).ok().flatten().map(gix::Id::detach)
    }

    /// Commits reachable from `from` but not from `upstream`, newest first (at most `limit`).
    pub(crate) fn commits_since(&self, from: ObjectId, upstream: ObjectId, limit: usize) -> Vec<ObjectId> {
        let Ok(walk) = self.repo.rev_walk([from]).all() else { return Vec::new() };
        walk.flatten().take(limit).map(|info| info.id).filter(|id| !self.contains(upstream, *id)).collect()
    }

    /// Three-way merge of two commits (the base is found from history).
    pub(crate) fn merge(&self, ours: ObjectId, theirs: ObjectId, labels: MergeLabels<'_>) -> Result<Merge> {
        use gix::merge::tree::TreatAsUnresolved;
        let labels = gix::merge::blob::builtin_driver::text::Labels {
            ancestor: Some(labels.base.into()),
            current: Some(labels.ours.into()),
            other: Some(labels.theirs.into()),
        };
        let options = self.repo.tree_merge_options()?;
        let mut out =
            self.repo.merge_commits(ours, theirs, labels, options.into()).map_err(|e| anyhow!("merging: {e}"))?;
        let how = TreatAsUnresolved::default();
        let conflicts: BTreeSet<String> = out
            .tree_merge
            .conflicts
            .iter()
            .filter(|c| c.is_unresolved(how))
            .map(|c| c.ours.location().to_str_lossy().into_owned())
            .collect();
        let tree = out.tree_merge.tree.write()?.detach();
        Ok(Merge { tree, conflicts: conflicts.into_iter().collect() })
    }
}

/// The names written into conflict markers.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MergeLabels<'a> {
    pub(crate) base: &'a str,
    pub(crate) ours: &'a str,
    pub(crate) theirs: &'a str,
}

/// The hunks of `d` in unified format, each with up to three lines of context.
fn unified(d: &gix::diff::blob::Diff, before: &str, after: &str) -> String {
    const CONTEXT: u32 = 3;
    let a: Vec<&str> = before.split_inclusive('\n').collect();
    let b: Vec<&str> = after.split_inclusive('\n').collect();
    let line = |out: &mut String, sign: char, l: &str| {
        out.push(sign);
        out.push_str(l);
        if !l.ends_with('\n') {
            out.push_str("\n\\ No newline at end of file\n");
        }
    };
    let mut out = String::new();
    for h in d.hunks() {
        let pre = h.before.start.min(CONTEXT);
        let post = (a.len() as u32 - h.before.end).min(CONTEXT);
        let (b0, b1) = (h.before.start - pre, h.before.end + post);
        let (a0, a1) = (h.after.start - pre, h.after.end + post);
        let _ = writeln!(out, "@@ -{},{} +{},{} @@", b0 + 1, b1 - b0, a0 + 1, a1 - a0);
        for i in b0..h.before.start {
            line(&mut out, ' ', a[i as usize]);
        }
        for i in h.before.clone() {
            line(&mut out, '-', a[i as usize]);
        }
        for i in h.after.clone() {
            line(&mut out, '+', b[i as usize]);
        }
        for i in h.before.end..b1 {
            line(&mut out, ' ', a[i as usize]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("faust-repo-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    const SIG: Signature<'static> = Signature { name: "test", email: "test@faust.invalid", time: 1_700_000_000 };
    const LABELS: MergeLabels<'static> = MergeLabels { base: "base", ours: "yours", theirs: "official" };

    #[test]
    fn snapshot_checkout_diff_merge() {
        let src = tmp("src");
        std::fs::write(src.join(".gitignore"), "/target\n").unwrap();
        std::fs::create_dir_all(src.join("src")).unwrap();
        std::fs::create_dir_all(src.join("target")).unwrap();
        std::fs::write(src.join("target/junk"), "x").unwrap();
        std::fs::write(src.join("src/a.rs"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(src.join("run.sh"), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(src.join("run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();

        let root = tmp("repo");
        let r = Repo::open_or_init(&root).unwrap();
        let t0 = r.tree_from_dir(&src).unwrap();
        let files = r.files(t0).unwrap();
        assert_eq!(files.keys().collect::<Vec<_>>(), [".gitignore", "run.sh", "src/a.rs"]);
        assert_eq!(files["run.sh"].0, FileKind::Executable);
        let c0 = r.commit(t0, &[], "base", SIG).unwrap();
        // Same content, author and time: the same commit on every Mac.
        assert_eq!(c0, r.commit(t0, &[], "base", SIG).unwrap());
        r.set_ref(MAIN, c0, "test").unwrap();
        assert_eq!(r.ref_id(MAIN), Some(c0));

        r.checkout(t0, &root).unwrap();
        std::fs::write(root.join("extra.txt"), "drop me").unwrap();
        std::fs::write(root.join("src/a.rs"), "one\nTWO\nthree\n").unwrap();
        let t1 = r.tree_from_dir(&root).unwrap();
        let (changes, text) = r.diff(t0, t1).unwrap();
        assert_eq!(changes.len(), 2);
        assert!(text.contains("-two\n+TWO\n"), "{text}");
        r.checkout(t0, &root).unwrap();
        assert!(!root.join("extra.txt").exists());
        assert_eq!(std::fs::read_to_string(root.join("src/a.rs")).unwrap(), "one\ntwo\nthree\n");
        r.write_index(t0).unwrap();

        // Ours changes line 1, theirs line 3: a clean merge.
        let tree_with = |text: &str| {
            std::fs::write(src.join("src/a.rs"), text).unwrap();
            r.tree_from_dir(&src).unwrap()
        };
        let ours = r.commit(tree_with("ONE\ntwo\nthree\n"), &[c0], "ours", SIG).unwrap();
        let theirs = r.commit(tree_with("one\ntwo\nTHREE\n"), &[c0], "theirs", SIG).unwrap();
        let m = r.merge(ours, theirs, LABELS).unwrap();
        assert!(m.conflicts.is_empty());
        assert_eq!(r.text_at(m.tree, "src/a.rs").unwrap().as_deref(), Some("ONE\ntwo\nTHREE\n"));
        assert!(r.contains(ours, c0) && !r.contains(c0, ours));
        assert_eq!(r.merge_base(ours, theirs), Some(c0));
        assert_eq!(r.commits_since(ours, c0, 10), [ours]);

        // Both change line 2: a conflict with markers in the tree.
        let theirs2 = r.commit(tree_with("ONE\nzwei\nthree\n"), &[c0], "theirs2", SIG).unwrap();
        let ours2 = r.commit(tree_with("ONE\ndeux\nthree\n"), &[c0], "ours2", SIG).unwrap();
        let m = r.merge(ours2, theirs2, LABELS).unwrap();
        assert_eq!(m.conflicts, ["src/a.rs"]);
        let text = r.text_at(m.tree, "src/a.rs").unwrap().unwrap();
        assert!(text.contains("<<<<<<< yours") && text.contains("deux") && text.contains("zwei"), "{text}");

        let out = tmp("export");
        r.export(t0, &out).unwrap();
        assert!(out.join("run.sh").is_file() && !out.join("target").exists());
        for d in [src, root, out] {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}
