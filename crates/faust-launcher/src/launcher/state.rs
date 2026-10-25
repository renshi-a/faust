//! `faust/state.json`: the versions that were built, which one is in use and which one is
//! on trial.

use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Good versions kept besides the current and fallback ones.
pub(crate) const KEEP_GOOD: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Status {
    /// Built and tested, not started yet.
    Trial,
    /// Started and kept running.
    Good,
    /// Could not start; its folder is gone.
    Bad,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Kind {
    /// An official release, as shipped.
    Official,
    /// A change the user asked for.
    Change,
    /// An official update, merged with the user's changes.
    Update,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Version {
    pub(crate) id: u32,
    pub(crate) commit: String,
    pub(crate) status: Status,
    pub(crate) kind: Kind,
    /// What was asked for, or the official version's name.
    pub(crate) title: String,
    pub(crate) made: i64,
    pub(crate) used: i64,
}

#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct State {
    /// The Rust release setup installed (empty before setup).
    pub(crate) rust: String,
    pub(crate) next_id: u32,
    /// The official release (`release.json`'s `release`) last put on refs/heads/upstream.
    pub(crate) upstream_release: u64,
    pub(crate) current: Option<u32>,
    /// Where to go back to if the current (trial) version does not start.
    pub(crate) fallback: Option<u32>,
    pub(crate) versions: Vec<Version>,
    /// The assistants the user let answer what they say to the app ([`Assistant::identity`]).
    ///
    /// [`Assistant::identity`]: super::assistant::Assistant::identity
    pub(crate) talk_allowed: Vec<String>,
}

pub(crate) fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs().try_into().unwrap_or(i64::MAX))
}

impl State {
    /// The saved state, or a fresh one when there is none (or it can't be read).
    pub(crate) fn load(path: &Path) -> State {
        let mut s: State =
            std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        s.next_id = s.next_id.max(1);
        s
    }

    /// Writes atomically (a temporary file, then a rename).
    pub(crate) fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)? + "\n")?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    pub(crate) fn take_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub(crate) fn get(&self, id: u32) -> Option<&Version> {
        self.versions.iter().find(|v| v.id == id)
    }

    pub(crate) fn get_mut(&mut self, id: u32) -> Option<&mut Version> {
        self.versions.iter_mut().find(|v| v.id == id)
    }

    pub(crate) fn current(&self) -> Option<&Version> {
        self.current.and_then(|id| self.get(id))
    }

    /// Good versions, most recently used first.
    pub(crate) fn good(&self) -> Vec<&Version> {
        let mut v: Vec<&Version> = self.versions.iter().filter(|v| v.status == Status::Good).collect();
        v.sort_by_key(|v| std::cmp::Reverse((v.used, v.id)));
        v
    }

    /// The good version to fall back to from `id`.
    pub(crate) fn previous_good(&self, id: u32) -> Option<u32> {
        self.good().into_iter().find(|v| v.id != id).map(|v| v.id)
    }

    /// Versions whose folders can go: bad ones, and good ones past the newest [`KEEP_GOOD`]
    /// (the current and fallback versions always stay).
    pub(crate) fn prune(&mut self) -> Vec<u32> {
        let keep: Vec<u32> =
            self.good().iter().take(KEEP_GOOD).map(|v| v.id).chain(self.current).chain(self.fallback).collect();
        let gone: Vec<u32> =
            self.versions.iter().filter(|v| !keep.contains(&v.id) && v.status != Status::Trial).map(|v| v.id).collect();
        self.versions.retain(|v| !gone.contains(&v.id));
        gone
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(id: u32, status: Status, used: i64) -> Version {
        Version {
            id,
            commit: format!("c{id}"),
            status,
            kind: Kind::Change,
            title: format!("v{id}"),
            made: id.into(),
            used,
        }
    }

    #[test]
    fn round_trips_and_prunes() {
        let mut s =
            State { rust: "1.96.1".into(), next_id: 10, current: Some(9), fallback: Some(8), ..State::default() };
        for id in 1..=8 {
            s.versions.push(v(id, Status::Good, id.into()));
        }
        s.versions.push(v(9, Status::Trial, 0));
        s.get_mut(3).unwrap().status = Status::Bad;
        let p = std::env::temp_dir().join(format!("faust-state-{}.json", std::process::id()));
        s.save(&p).unwrap();
        let mut t = State::load(&p);
        assert_eq!(t, s);
        assert_eq!(t.previous_good(8), Some(7));
        let mut gone = t.prune();
        gone.sort_unstable();
        // Good: 8 7 6 5 4 kept (the newest five), 2 and 1 go, 3 is bad, 9 is on trial.
        assert_eq!(gone, [1, 2, 3]);
        assert_eq!(t.take_id(), 10);
        let _ = std::fs::remove_file(p);
        assert_eq!(State::load(Path::new("/nonexistent/state.json")).next_id, 1);
    }
}
