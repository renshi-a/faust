//! The app side of [Faust](https://github.com/renshi-a/faust): a few calls that let a
//! repairable app work with the launcher that builds, starts and switches it.
//!
//! When Faust starts your app, it passes one end of a socket and a few facts about the
//! version that is running. [`get`] returns the link, or `None` when the app was started
//! some other way (from `cargo run`, say), so every call site can stay optional:
//!
//! ```no_run
//! // Once the first window is on screen:
//! if let Some(link) = faust_link::get() {
//!     link.ready();
//! }
//!
//! // From the field where the user talks to the app:
//! if let Some(link) = faust_link::get() {
//!     link.say("make the text larger");
//! }
//!
//! // In the event loop:
//! if let Some(link) = faust_link::get() {
//!     for event in link.events() {
//!         match event {
//!             faust_link::Event::Quit => { /* save and quit */ }
//!             _ => {}
//!         }
//!     }
//! }
//! ```
//!
//! An app that never calls [`Link::ready`] still works: Faust then counts it as started
//! once it has kept running for a few seconds. The messages themselves are described in
//! the repository's `docs/protocol.md`, for apps that are not written in Rust.

mod json;

use std::io::{BufRead, BufReader, Write};
use std::os::fd::FromRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Mutex, OnceLock};

use json::{Value, object};

/// The environment variable that names the socket's file descriptor.
pub const FD_VAR: &str = "FAUST_LINK_FD";

/// The version of the protocol this crate speaks.
pub const PROTOCOL: u32 = 2;

/// What Faust tells the app about the version that is running.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Info {
    /// The version's number, shown to the user as `#12`.
    pub version: u32,
    /// What the version is: "Official version 1.2.0", or the change that was asked for.
    pub version_title: String,
    /// The user's copy of the app's source (a git repository).
    pub source_dir: PathBuf,
    /// The language of Faust's windows, such as `en` or `ja`, for the app to follow.
    pub language: String,
    /// The protocol the launcher speaks.
    pub protocol: u32,
}

impl Info {
    fn from_env() -> Info {
        let var = |k: &str| std::env::var(k).unwrap_or_default();
        Info {
            version: var("FAUST_VERSION").parse().unwrap_or(0),
            version_title: var("FAUST_VERSION_TITLE"),
            source_dir: PathBuf::from(var("FAUST_SOURCE_DIR")),
            language: var("FAUST_LANG"),
            protocol: var("FAUST_PROTOCOL").parse().unwrap_or(PROTOCOL),
        }
    }
}

/// Something the launcher asks of the app.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Event {
    /// Quit soon: Faust is switching to another version, or uninstalling the app.
    /// After ten seconds it stops the app itself.
    Quit,
    /// Faust is asking the user something. An app that pairs with other devices may
    /// show it there too, and relay a signed answer with [`Link::answer_confirmation`].
    Confirmation(Confirmation),
    /// The confirmation with this id was answered (on the Mac or on a device).
    ConfirmationClosed(String),
    /// Who makes the next change, by name, such as `Claude Code` or `qwen3-coder`; empty
    /// when there is no assistant yet. Sent once the app is ready and whenever the user
    /// chooses another one, for the app to show. Only Faust's own window chooses it: see
    /// [`Link::choose_assistant`].
    Assistant(String),
    /// The answer to something the user said through [`Link::say`].
    Reply(Reply),
}

/// Faust's answer, as the app, to something the user said.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Reply {
    /// The id [`Link::say`] returned.
    pub id: String,
    /// What to show, in the user's language. Empty when there is nothing to say (the user
    /// didn't let the assistant answer, say).
    pub text: String,
    /// The change the assistant suggested because the app can't do this yet. Faust is
    /// asking the user whether to make it.
    pub change: Option<String>,
}

/// A question Faust is asking the user, such as whether to switch to a new version.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Confirmation {
    /// New for every confirmation, so a signed answer cannot be replayed.
    pub id: String,
    /// `change` (before the assistant starts) or `switch` (before using a new version).
    pub kind: String,
    /// The question, in the user's language.
    pub title: String,
    /// What the question is about, in a few lines.
    pub text: String,
    /// The possible answers, the default first.
    pub choices: Vec<Choice>,
}

/// One of the answers to a [`Confirmation`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Choice {
    /// What the answer sends back, such as `switch` or `cancel`.
    pub id: String,
    /// The button's title, in the user's language.
    pub label: String,
}

/// The connection to the launcher.
#[derive(Debug)]
pub struct Link {
    writer: Mutex<UnixStream>,
    events: Mutex<Receiver<Event>>,
    info: Info,
}

/// The link to the launcher, or `None` when the app was not started by Faust.
///
/// The first call takes over the socket and starts a thread that reads from it; later
/// calls return the same link.
pub fn get() -> Option<&'static Link> {
    static LINK: OnceLock<Option<Link>> = OnceLock::new();
    LINK.get_or_init(connect).as_ref()
}

fn connect() -> Option<Link> {
    let fd: i32 = std::env::var(FD_VAR).ok()?.parse().ok()?;
    if fd < 0 {
        return None;
    }
    // SAFETY: Faust opened this descriptor for the app and named it in FAUST_LINK_FD.
    // `get` runs this once, so nothing else in the process owns it.
    let stream = unsafe { UnixStream::from_raw_fd(fd) };
    let reader = stream.try_clone().ok()?;
    let (tx, rx) = channel();
    std::thread::Builder::new()
        .name("faust-link".into())
        .spawn(move || {
            for line in BufReader::new(reader).lines() {
                let Ok(line) = line else { break };
                if let Some(event) = parse_event(&line)
                    && tx.send(event).is_err()
                {
                    break;
                }
            }
        })
        .ok()?;
    Some(Link { writer: Mutex::new(stream), events: Mutex::new(rx), info: Info::from_env() })
}

/// Reads one line from the launcher. Lines this version does not know are skipped.
fn parse_event(line: &str) -> Option<Event> {
    let v = Value::parse(line).ok()?;
    let field = |k: &str| v.get(k).as_str().unwrap_or("").to_owned();
    match v.get("type").as_str()? {
        "quit" => Some(Event::Quit),
        "confirmation" => Some(Event::Confirmation(Confirmation {
            id: field("id"),
            kind: field("kind"),
            title: field("title"),
            text: field("text"),
            choices: v
                .get("choices")
                .as_array()
                .iter()
                .filter_map(|c| {
                    Some(Choice { id: c.get("id").as_str()?.to_owned(), label: c.get("label").as_str()?.to_owned() })
                })
                .collect(),
        })),
        "confirmation.closed" => Some(Event::ConfirmationClosed(field("id"))),
        "assistant" => Some(Event::Assistant(field("name"))),
        "reply" => Some(Event::Reply(Reply {
            id: field("id"),
            text: field("text"),
            change: v.get("change").as_str().map(str::to_owned).filter(|c| !c.is_empty()),
        })),
        _ => None,
    }
}

impl Link {
    /// What Faust said about the running version.
    pub fn info(&self) -> &Info {
        &self.info
    }

    /// Tells Faust the app is up: call it once the first window is on screen.
    ///
    /// A new version counts as good when it calls this within a minute of starting and
    /// keeps running for a few seconds more; otherwise Faust goes back to the previous one.
    pub fn ready(&self) {
        self.send(&object([("type", "ready".into())]));
    }

    /// Asks Faust to change the app. With `None`, Faust asks the user what to change; with
    /// a request (from a conversation in the app, say), it shows that request instead.
    /// Nothing changes until the user confirms it in Faust's own window.
    pub fn request_change(&self, request: Option<&str>) {
        let mut fields = vec![("type".to_owned(), Value::from("change"))];
        if let Some(text) = request.map(str::trim).filter(|t| !t.is_empty()) {
            fields.push(("request".to_owned(), text.into()));
        }
        self.send(&Value::Object(fields));
    }

    /// Passes on what the user said to the app. Faust's assistant answers as the app with
    /// [`Event::Reply`], carrying the id this returns, and when the app can't do what was
    /// asked yet, it suggests a change, which Faust asks the user about. The assistant only
    /// reads the source to answer; nothing changes until the user confirms.
    ///
    /// A launcher older than protocol 2 can't answer: there the words become a change
    /// request, as with [`Link::request_change`], and this returns `None`.
    pub fn say(&self, text: &str) -> Option<String> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        if self.info.protocol < 2 {
            self.request_change(Some(text));
            return None;
        }
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = format!("say-{}", NEXT.fetch_add(1, Ordering::Relaxed));
        self.send(&object([("type", "say".into()), ("id", id.as_str().into()), ("text", text.into())]));
        Some(id)
    }

    /// Opens Faust's list of versions, to go back to an earlier one.
    pub fn choose_version(&self) {
        self.send(&object([("type", "versions".into())]));
    }

    /// Opens Faust's window for choosing who makes the changes: Claude Code, or a model the
    /// user picks. Faust tells the app the choice with [`Event::Assistant`].
    pub fn choose_assistant(&self) {
        self.send(&object([("type", "assistant.choose".into())]));
    }

    /// Asks Faust to uninstall the app (after asking the user).
    pub fn uninstall(&self) {
        self.send(&object([("type", "uninstall".into())]));
    }

    /// A device was paired in the app. Faust asks the user whether answers signed with
    /// this Ed25519 key (32 bytes, base64url) may count for its confirmations.
    pub fn trust_device(&self, name: &str, public_key: &str) {
        self.send(&object([("type", "device.trust".into()), ("name", name.into()), ("public_key", public_key.into())]));
    }

    /// The device with this key was unpaired: its answers no longer count.
    pub fn forget_device(&self, public_key: &str) {
        self.send(&object([("type", "device.forget".into()), ("public_key", public_key.into())]));
    }

    /// Relays a device's answer to a [`Confirmation`]. It counts only when `signature`
    /// (Ed25519, base64url) is the device's, made with a key the user trusted in Faust, over
    /// the confirmation and the choice (see `docs/protocol.md`), so the app itself cannot
    /// answer for the user.
    pub fn answer_confirmation(&self, id: &str, choice: &str, public_key: &str, signature: &str) {
        self.send(&object([
            ("type", "confirmation.answer".into()),
            ("id", id.into()),
            ("choice", choice.into()),
            ("public_key", public_key.into()),
            ("signature", signature.into()),
        ]));
    }

    /// The next event, if one is waiting. Never blocks.
    pub fn try_event(&self) -> Option<Event> {
        self.events.lock().expect("faust-link event queue poisoned").try_recv().ok()
    }

    /// Every event that is waiting, oldest first. Never blocks.
    pub fn events(&self) -> Vec<Event> {
        self.events.lock().expect("faust-link event queue poisoned").try_iter().collect()
    }

    /// Writes one message. A launcher that went away is not an error: the app keeps running.
    fn send(&self, message: &Value) {
        let line = format!("{message}\n");
        let mut writer = self.writer.lock().expect("faust-link writer poisoned");
        let _ = writer.write_all(line.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_launchers_messages() {
        assert_eq!(parse_event(r#"{"type":"quit"}"#), Some(Event::Quit));
        assert_eq!(
            parse_event(r#"{"type":"confirmation.closed","id":"c1"}"#),
            Some(Event::ConfirmationClosed("c1".into()))
        );
        let line = r#"{"type":"confirmation","id":"c2","kind":"switch","title":"Switch?","text":"…","choices":[{"id":"switch","label":"Switch"},{"id":"cancel","label":"Cancel"}]}"#;
        let Some(Event::Confirmation(c)) = parse_event(line) else { panic!("not a confirmation: {line}") };
        assert_eq!((c.id.as_str(), c.kind.as_str(), c.choices.len()), ("c2", "switch", 2));
        assert_eq!(c.choices[1], Choice { id: "cancel".into(), label: "Cancel".into() });
        assert_eq!(
            parse_event(r#"{"type":"assistant","name":"Claude Code"}"#),
            Some(Event::Assistant("Claude Code".into()))
        );
        assert_eq!(
            parse_event(r#"{"type":"reply","id":"say-1","text":"I can't yet.","change":"Make the water red"}"#),
            Some(Event::Reply(Reply {
                id: "say-1".into(),
                text: "I can't yet.".into(),
                change: Some("Make the water red".into())
            }))
        );
        assert_eq!(
            parse_event(r#"{"type":"reply","id":"say-2","text":"Hello!"}"#),
            Some(Event::Reply(Reply { id: "say-2".into(), text: "Hello!".into(), change: None }))
        );
        assert_eq!(parse_event(r#"{"type":"from-the-future"}"#), None);
        assert_eq!(parse_event("not json"), None);
    }

    #[test]
    fn writes_messages_the_launcher_reads() {
        let (ours, theirs) = UnixStream::pair().expect("socket pair");
        let (_tx, rx) = channel();
        let link = Link { writer: Mutex::new(ours), events: Mutex::new(rx), info: Info::default() };
        link.ready();
        link.request_change(Some("  make the button blue \n"));
        link.request_change(Some("   "));
        link.choose_assistant();
        drop(link);
        let lines: Vec<String> = BufReader::new(theirs).lines().map_while(Result::ok).collect();
        assert_eq!(
            lines,
            [
                r#"{"type":"ready"}"#,
                r#"{"type":"change","request":"make the button blue"}"#,
                r#"{"type":"change"}"#,
                r#"{"type":"assistant.choose"}"#
            ]
        );
    }

    #[test]
    fn says_what_the_user_wrote() {
        let talk = |protocol: u32| {
            let (ours, theirs) = UnixStream::pair().expect("socket pair");
            let (_tx, rx) = channel();
            let info = Info { protocol, ..Info::default() };
            let link = Link { writer: Mutex::new(ours), events: Mutex::new(rx), info };
            let ids = [link.say(" Hello \n"), link.say("  ")];
            drop(link);
            (ids, BufReader::new(theirs).lines().map_while(Result::ok).collect::<Vec<String>>())
        };
        let ([id, none], lines) = talk(2);
        let id = id.expect("an id");
        assert_eq!(none, None);
        assert_eq!(lines, [format!(r#"{{"type":"say","id":"{id}","text":"Hello"}}"#)]);
        // An older launcher gets a change request, as before.
        let ([id, _], lines) = talk(1);
        assert_eq!(id, None);
        assert_eq!(lines, [r#"{"type":"change","request":"Hello"}"#]);
    }
}
