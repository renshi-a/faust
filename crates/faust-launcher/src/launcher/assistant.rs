//! The coding assistant that changes the source: Claude Code, or a model Faust runs itself
//! (see `model.rs`), whichever the user chose.
//!
//! Either may only read, edit, write and search files in the user's repository. Claude Code
//! runs in its restricted mode, which ignores every settings file (the user's, and any the
//! repository holds, since the repository may have been rewritten) and confines the file tools
//! to the working directory; hooks are off and there are no MCP servers. A model gets only the
//! tools Faust carries out itself. Building and testing are the launcher's job.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::model::{self, Endpoint};
use crate::proc::{self, Limits, Output};
use crate::repo::GitGuard;

/// One run may take this long.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// An answer to what the user said may take this long.
pub(crate) const TALK_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const TOOLS: &str = "Read,Edit,Write,Glob,Grep";
/// Answering what the user said needs only to read the source.
const READ_TOOLS: &str = "Read,Glob,Grep";
/// Settings given on the command line, which restricted mode still applies.
const SETTINGS: &str = r#"{"disableAllHooks":true}"#;

/// The assistant that makes the changes.
#[derive(Debug, Clone)]
pub(crate) struct Assistant {
    /// What commit messages name it by: "2.1.300 (Claude Code)", "qwen3-coder (localhost:11434)".
    pub(crate) version: String,
    pub(crate) backend: Backend,
}

#[derive(Debug, Clone)]
pub(crate) enum Backend {
    /// An installed Claude Code.
    ClaudeCode {
        path: PathBuf,
        /// Has `--restricted`. Faust doesn't run a Claude Code without it.
        restricted: bool,
    },
    /// A model behind an OpenAI-compatible API, with Faust's own tools.
    Model(Endpoint),
}

impl Assistant {
    pub(crate) fn model(e: Endpoint) -> Assistant {
        Assistant { version: format!("{} ({})", e.model, e.host()), backend: Backend::Model(e) }
    }

    /// "Claude Code", or the model's name, for sentences.
    pub(crate) fn name(&self) -> &str {
        match &self.backend {
            Backend::ClaudeCode { .. } => "Claude Code",
            Backend::Model(e) => &e.model,
        }
    }

    /// "2.1.300 (Claude Code) (~/.local/bin/claude)", "qwen3-coder (http://localhost:11434/v1)",
    /// for showing.
    pub(crate) fn title(&self) -> String {
        match &self.backend {
            Backend::ClaudeCode { path, .. } => format!("{} ({})", self.version, super::paths::Paths::tilde(path)),
            Backend::Model(e) => format!("{} ({})", e.model, e.url),
        }
    }

    /// Just the version number: "2.1.300".
    pub(crate) fn short_version(&self) -> &str {
        self.version.split_whitespace().next().unwrap_or(&self.version)
    }

    /// Who answers, for remembering whom the user let read what they say to the app:
    /// Claude Code, or a model at its address.
    pub(crate) fn identity(&self) -> String {
        match &self.backend {
            Backend::ClaudeCode { .. } => "claude-code".into(),
            Backend::Model(e) => format!("{} {}", e.url, e.model),
        }
    }

    /// A Claude Code without restricted mode, which Faust doesn't run.
    pub(crate) fn too_old(&self) -> bool {
        matches!(self.backend, Backend::ClaudeCode { restricted: false, .. })
    }
}

/// Which assistant the user chose, kept in `faust/assistant.json`. Without one, Claude Code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(crate) enum Choice {
    ClaudeCode,
    Model(Endpoint),
}

pub(crate) fn load(file: &Path) -> Option<Choice> {
    serde_json::from_slice(&std::fs::read(file).ok()?).ok()
}

/// Saves the choice where only this user can read it: it may hold an API key.
pub(crate) fn save(file: &Path, choice: &Choice) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let tmp = file.with_extension("json.tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
    f.write_all(&serde_json::to_vec_pretty(choice).map_err(std::io::Error::other)?)?;
    std::fs::rename(tmp, file)
}

/// The assistant the user chose, or else Claude Code if it is installed.
pub(crate) fn chosen(file: &Path) -> Option<Assistant> {
    match load(file) {
        Some(Choice::Model(e)) => Some(Assistant::model(e)),
        _ => find(),
    }
}

/// The first Claude Code that answers `--version`, from where installers put it.
/// `FAUST_ASSISTANT` names one to use instead.
pub(crate) fn find() -> Option<Assistant> {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let candidates = match super::overrides::get("FAUST_ASSISTANT") {
        Some(c) => vec![PathBuf::from(c)],
        None => {
            let mut c = vec![home.join(".local/bin/claude"), home.join(".claude/local/claude")];
            if let Ok(entries) = std::fs::read_dir(home.join(".vscode/extensions")) {
                let mut found: Vec<PathBuf> = entries
                    .flatten()
                    .filter(|e| e.file_name().to_string_lossy().starts_with("anthropic.claude-code-"))
                    .map(|e| e.path().join("resources/native-binary/claude"))
                    .collect();
                found.sort_by_key(|p| std::cmp::Reverse(extension_version(p)));
                c.extend(found);
            }
            c.extend([PathBuf::from("/opt/homebrew/bin/claude"), PathBuf::from("/usr/local/bin/claude")]);
            c
        }
    };
    candidates.into_iter().filter(|c| c.is_file()).find_map(|path| {
        let out = Command::new(&path).arg("--version").env("PATH", search_path()).output().ok()?;
        let version = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if !out.status.success() || !version.contains("Claude Code") {
            return None;
        }
        let help = Command::new(&path).arg("--help").env("PATH", search_path()).output().ok()?;
        let restricted = String::from_utf8_lossy(&help.stdout).contains("--restricted");
        Some(Assistant { version, backend: Backend::ClaudeCode { path, restricted } })
    })
}

/// `anthropic.claude-code-2.1.300-darwin-arm64` -> `[2, 1, 300]`.
fn extension_version(p: &Path) -> Vec<u32> {
    let name =
        p.ancestors().nth(3).and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let v = name.trim_start_matches("anthropic.claude-code-");
    v.split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty()).take(3).map(|s| s.parse().unwrap_or(0)).collect()
}

/// An app opened from Finder has a short PATH; the npm build of Claude Code needs node.
fn search_path() -> &'static str {
    "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"
}

/// The message for a Claude Code too old to run restricted.
pub(crate) fn outdated(version: &str) -> String {
    tr!(
        en: "This Claude Code ({version}) is too old to run safely. Please update it, then ask again.",
        ja: "この Claude Code（{version}）は古いため、安全に動かせません。更新してから、もう一度頼んでください。",
        de: "Dieses Claude Code ({version}) ist zu alt, um es sicher auszuführen. Aktualisiere es bitte und frag dann noch einmal.",
        it: "Questo Claude Code ({version}) è troppo vecchio per essere eseguito in sicurezza. Aggiornalo e poi chiedi di nuovo.",
    )
}

/// What a run produced.
#[derive(Debug)]
pub(crate) struct Outcome {
    /// What it says it changed, in the user's language.
    pub(crate) summary: String,
    /// To continue the same conversation (after a failed build).
    pub(crate) session: Option<Session>,
}

/// A conversation to continue.
#[derive(Debug, Clone)]
pub(crate) enum Session {
    /// Claude Code's session id.
    ClaudeCode(String),
    /// The messages so far.
    Model(Vec<Value>),
}

/// The facts every prompt starts from.
#[derive(Debug, Clone)]
pub(crate) struct Brief {
    pub(crate) app: String,
    /// The rules file, relative to the repository.
    pub(crate) rules: String,
}

impl Brief {
    fn language() -> &'static str {
        crate::i18n::pick(["English", "Japanese", "German", "Italian"])
    }

    fn preamble(&self) -> String {
        let (app, rules) = (&self.app, &self.rules);
        format!(
            "You are changing the source of {app}, a macOS app, at the request of the person who uses it.\n\
             Work only inside this directory (a git repository). Before you start, read {rules} and follow it.\n\
             You can't run commands: after you finish, the app is built, tested and started for you."
        )
    }

    /// The first request: the user's words.
    pub(crate) fn change(&self, request: &str) -> String {
        let lang = Brief::language();
        format!(
            "{}\n\nTheir request:\n{request}\n\nWhen you are done, reply with what you changed and how, in at most three lines of {lang}.",
            self.preamble()
        )
    }

    /// What the user said to the app, for the assistant to answer as the app. `first` is
    /// whether this starts the conversation (later ones continue it).
    pub(crate) fn talk(&self, text: &str, first: bool) -> String {
        let (app, rules, lang) = (&self.app, &self.rules, Brief::language());
        let rules_text = format!(
            "Answer as {app}, in at most three short sentences of plain {lang}.\n\
             If they want something {app} can't do yet, and changing its source would make it possible, \
             say in one sentence what you would change, and end your reply with this line:\n\
             [[change: the change, as a request to the programmer who will change the source, in {lang}]]\n\
             They are asked before anything changes, so don't ask them first. \
             Leave the line out when {app} can already do it, when they are just talking, \
             or when changing the source wouldn't help."
        );
        if first {
            format!(
                "You are {app}, a macOS app, and the person who uses it just wrote to it. \
                 This directory (a git repository) is {app}'s source: read {rules} first, \
                 and read the source when you need to know what {app} can do. \
                 You can only read files here; you can't change them or run anything.\n\n\
                 {rules_text}\n\nWhat they wrote:\n{text}"
            )
        } else {
            format!("They wrote:\n{text}\n\n(As before: {rules_text})")
        }
    }

    /// After a failed build or test: the end of the log.
    pub(crate) fn fix(&self, failure: &str) -> String {
        let lang = Brief::language();
        format!(
            "The build or the tests failed. The end of the log follows. Please fix it, then reply with a summary of all your changes so far, in at most three lines of {lang}.\n\n{failure}"
        )
    }

    /// An official update conflicts with the user's changes.
    pub(crate) fn merge(&self, conflicts: &[String], requests: &str, notes: &str) -> String {
        let lang = Brief::language();
        let files = conflicts.iter().map(|c| format!("- {c}")).collect::<Vec<_>>().join("\n");
        format!(
            "{}\n\nAn official update of {app} conflicts with changes this person asked for earlier. \
             These files have <<<<<<< ======= >>>>>>> markers: \"yours\" is their version, \"official\" is the update. \
             Keep the intent of both, and remove every marker.\n\n\
             Conflicting files:\n{files}\n\nWhat they asked for earlier:\n{requests}\n\nWhat the update brings:\n{notes}\n\n\
             When you are done, reply with how you combined them, in at most three lines of {lang}.",
            self.preamble(),
            app = self.app,
        )
    }

    /// An official update conflicts, and the user asked for their earlier requests to be made
    /// again on it, one at a time: this is one of them.
    pub(crate) fn replay(&self, request: &str, update: &str, notes: &str) -> String {
        let lang = Brief::language();
        let notes = if notes.trim().is_empty() { "(no notes)" } else { notes };
        format!(
            "{}\n\n{app} was just updated to a new official version ({update}). Earlier, on the previous version, \
             this person asked for the change below. The source here is the new official version, with their requests \
             before this one already made again on it. Make this change again here, and keep what the update brings.\n\n\
             Their request:\n{request}\n\nWhat the update brings:\n{notes}\n\n\
             When you are done, reply with what you changed and how, in at most three lines of {lang}.",
            self.preamble(),
            app = self.app,
        )
    }
}

/// Runs the assistant in `cwd` (the repository), continuing `resume` if given. `on_log` gets
/// what it is doing as it goes (files read and changed, what it says).
pub(crate) fn run(
    a: &Assistant,
    cwd: &Path,
    prompt: &str,
    resume: Option<Session>,
    log: &Path,
    limits: Limits<'_>,
    on_log: &mut dyn FnMut(&str),
) -> Result<Outcome> {
    let guard = GitGuard::take(cwd)?;
    let out = run_in(a, cwd, prompt, resume, log, limits, on_log);
    let changed = guard.restore(cwd).map_err(|e| anyhow!("{} changed the repository's history: {e:#}", a.name()))?;
    if !changed.is_empty() {
        super::log(&format!("{} changed .git, put back: {}", a.name(), changed.join(", ")));
    }
    out
}

fn run_in(
    a: &Assistant,
    cwd: &Path,
    prompt: &str,
    resume: Option<Session>,
    log: &Path,
    limits: Limits<'_>,
    on_log: &mut dyn FnMut(&str),
) -> Result<Outcome> {
    match &a.backend {
        Backend::ClaudeCode { path, restricted } => {
            if !restricted {
                bail!(outdated(&a.version));
            }
            let id = match resume {
                Some(Session::ClaudeCode(id)) => Some(id),
                _ => None,
            };
            claude(path, cwd, prompt, id.as_deref(), log, limits, on_log)
        }
        Backend::Model(e) => {
            let history = match resume {
                Some(Session::Model(m)) => m,
                _ => Vec::new(),
            };
            let (summary, messages) = model::run(e, cwd, prompt, history, log, limits, on_log)?;
            Ok(Outcome { summary, session: Some(Session::Model(messages)) })
        }
    }
}

/// `claude -p` with the prompt on stdin; events come back as JSON lines and the last one is
/// the result.
fn claude(
    path: &Path,
    cwd: &Path,
    prompt: &str,
    resume: Option<&str>,
    log: &Path,
    limits: Limits<'_>,
    on_log: &mut dyn FnMut(&str),
) -> Result<Outcome> {
    let cmd = claude_command(path, cwd, TOOLS, resume);
    let out = proc::run(cmd, Some(prompt), log, Limits { cancel: limits.cancel, timeout: Some(TIMEOUT) }, &mut |o| {
        if let Output::Line(l) = o {
            for line in describe(l, cwd) {
                on_log(&line);
            }
        }
    })?;
    parse(&out)
}

/// `claude -p` in restricted mode with only `tools`, in `cwd`.
fn claude_command(path: &Path, cwd: &Path, tools: &str, resume: Option<&str>) -> Command {
    let mut cmd = Command::new(path);
    cmd.arg("-p")
        .args(["--output-format", "stream-json", "--verbose"])
        .arg("--restricted")
        .args(["--settings", SETTINGS])
        .args(["--permission-mode", "acceptEdits"])
        .args(["--tools", tools])
        .arg("--strict-mcp-config");
    if let Some(id) = resume {
        cmd.args(["--resume", id]);
    }
    cmd.current_dir(cwd).env("PATH", search_path());
    for (k, _) in std::env::vars_os().filter(|(k, _)| k.to_string_lossy().starts_with("FAUST_")) {
        cmd.env_remove(k);
    }
    cmd
}

/// Lets the assistant answer what the user said (`prompt`, from [`Brief::talk`]), reading
/// the repository in `cwd` and nothing more. Continues `resume` if given.
pub(crate) fn talk(a: &Assistant, cwd: &Path, prompt: &str, resume: Option<Session>, log: &Path) -> Result<Outcome> {
    static NEVER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let limits = Limits { cancel: &NEVER, timeout: Some(TALK_TIMEOUT) };
    match &a.backend {
        Backend::ClaudeCode { path, restricted } => {
            if !restricted {
                bail!(outdated(&a.version));
            }
            let id = match resume {
                Some(Session::ClaudeCode(id)) => Some(id),
                _ => None,
            };
            let cmd = claude_command(path, cwd, READ_TOOLS, id.as_deref());
            parse(&proc::run(cmd, Some(prompt), log, limits, &mut |_| {})?)
        }
        Backend::Model(e) => {
            let history = match resume {
                Some(Session::Model(m)) => m,
                _ => Vec::new(),
            };
            let (summary, messages) = model::talk(e, cwd, prompt, history, log, limits)?;
            Ok(Outcome { summary, session: Some(Session::Model(messages)) })
        }
    }
}

/// An answer without its `[[change: …]]` line, and the change it suggests, if any.
pub(crate) fn take_change(answer: &str) -> (String, Option<String>) {
    let Some(start) = answer.find("[[change:") else { return (answer.trim().to_owned(), None) };
    let rest = &answer[start + "[[change:".len()..];
    let (change, after) = rest.split_once("]]").unwrap_or((rest, ""));
    let text = format!("{}{}", answer[..start].trim_end(), after.trim_end());
    let change = change.trim();
    (text.trim().to_owned(), (!change.is_empty()).then(|| change.to_owned()))
}

/// One line of the assistant's output, as lines for the progress log. Events that say
/// nothing new (tool results, thinking) give none.
fn describe(line: &str, cwd: &Path) -> Vec<String> {
    let Ok(event) = serde_json::from_str::<Value>(line.trim()) else {
        // Not an event (stderr): as it is.
        return vec![short(line.trim(), 200)];
    };
    let rel = |p: &str| {
        let root = format!("{}/", cwd.display());
        p.strip_prefix(&root).unwrap_or(p).to_owned()
    };
    let s = |v: &Value| v.as_str().unwrap_or("").to_owned();
    let mut out = Vec::new();
    match event["type"].as_str() {
        Some("system") if event["subtype"] == "init" => {
            let model = s(&event["model"]);
            out.push(tr!(en: "Started ({model})", ja: "開始しました（{model}）", de: "Gestartet ({model})", it: "Avviato ({model})"));
        }
        Some("assistant") => {
            for c in event["message"]["content"].as_array().into_iter().flatten() {
                match c["type"].as_str() {
                    Some("text") => out.extend(s(&c["text"]).lines().map(str::trim).filter(|l| !l.is_empty()).map(|l| short(l, 200))),
                    Some("tool_use") => {
                        let input = &c["input"];
                        let path = rel(input["file_path"].as_str().or(input["path"].as_str()).unwrap_or(""));
                        out.push(tool_line(c["name"].as_str().unwrap_or("?"), &path, input["pattern"].as_str().unwrap_or("")));
                    }
                    _ => {}
                }
            }
        }
        Some("user") => {
            for c in event["message"]["content"].as_array().into_iter().flatten() {
                if c["type"] == "tool_result" && c["is_error"] == true {
                    let text = c["content"].as_str().or(c["content"][0]["text"].as_str()).unwrap_or("");
                    let first = short(text.lines().next().unwrap_or(""), 160);
                    out.push(tr!(en: "  Didn't work: {first}", ja: "  うまくいきませんでした: {first}", de: "  Hat nicht geklappt: {first}", it: "  Non è riuscito: {first}"));
                }
            }
        }
        Some("result") => out.push(if event["is_error"] == true {
            tr!(en: "Ended with an error", ja: "エラーで終了しました", de: "Mit einem Fehler beendet", it: "Terminato con un errore")
        } else {
            tr!(en: "Done with the changes", ja: "変更が終わりました", de: "Änderungen abgeschlossen", it: "Modifiche completate")
        }),
        _ => {}
    }
    out
}

/// A tool call, by Claude Code's name for the tool, as a line for the progress log.
pub(super) fn tool_line(name: &str, path: &str, pattern: &str) -> String {
    match name {
        "Read" => tr!(en: "Read {path}", ja: "読む: {path}", de: "Lesen: {path}", it: "Lettura: {path}"),
        "Edit" => tr!(en: "Edit {path}", ja: "編集: {path}", de: "Bearbeiten: {path}", it: "Modifica: {path}"),
        "Write" => tr!(en: "Write {path}", ja: "書き込み: {path}", de: "Schreiben: {path}", it: "Scrittura: {path}"),
        "Glob" | "Grep" if pattern.is_empty() => {
            tr!(en: "List {path}", ja: "一覧: {path}", de: "Auflisten: {path}", it: "Elenco: {path}")
        }
        "Glob" | "Grep" if path.is_empty() => {
            tr!(en: "Search {pattern}", ja: "検索: {pattern}", de: "Suchen: {pattern}", it: "Ricerca: {pattern}")
        }
        "Glob" | "Grep" => {
            tr!(en: "Search {pattern} in {path}", ja: "検索: {pattern}（{path}）", de: "Suchen: {pattern} in {path}", it: "Ricerca: {pattern} in {path}")
        }
        other => other.to_owned(),
    }
}

pub(super) fn short(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_owned() } else { s.chars().take(n).collect::<String>() + "…" }
}

fn parse(out: &str) -> Result<Outcome> {
    // The result is the last event; anything before it is the work so far, or noise.
    let result = out
        .lines()
        .rev()
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .find(|v| v["type"] == "result")
        .ok_or_else(|| {
            anyhow!(tr!(
                en: "Can't read the assistant's result",
                ja: "アシスタントの結果を読めません",
                de: "Das Ergebnis des Assistenten ist nicht lesbar",
                it: "Impossibile leggere il risultato dell'assistente",
            ))
        })?;
    let summary = result["result"].as_str().unwrap_or("").trim().to_owned();
    if result["is_error"] == true {
        if summary.is_empty() {
            bail!(tr!(
                en: "The assistant returned an error",
                ja: "アシスタントがエラーを返しました",
                de: "Der Assistent hat einen Fehler gemeldet",
                it: "L'assistente ha restituito un errore",
            ));
        }
        bail!(summary);
    }
    Ok(Outcome { summary, session: result["session_id"].as_str().map(|id| Session::ClaudeCode(id.to_owned())) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_results() {
        let o = parse(
            "{\"type\":\"result\",\"is_error\":false,\"result\":\" Made the eyes bigger \",\"session_id\":\"abc\"}\n",
        )
        .unwrap();
        assert_eq!(o.summary, "Made the eyes bigger");
        assert!(matches!(o.session, Some(Session::ClaudeCode(id)) if id == "abc"));
        assert!(parse("{\"type\":\"result\",\"is_error\":true,\"result\":\"limit\"}").is_err());
        assert!(parse("nothing").is_err());
        let stream = "{\"type\":\"system\",\"subtype\":\"init\",\"model\":\"m\"}\n{\"type\":\"result\",\"is_error\":false,\"result\":\"ok\",\"session_id\":\"s\"}\n";
        assert_eq!(parse(stream).unwrap().summary, "ok");
        let p = Path::new(
            "/x/.vscode/extensions/anthropic.claude-code-2.1.300-darwin-arm64/resources/native-binary/claude",
        );
        assert_eq!(extension_version(p), [2, 1, 300]);
    }

    #[test]
    fn describes_events() {
        let d = |l: &str| describe(l, Path::new("/r"));
        assert_eq!(d(r#"{"type":"system","subtype":"init","model":"opus"}"#), ["Started (opus)"]);
        assert_eq!(
            d(
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Looking\n\nfirst app.rs"},{"type":"tool_use","name":"Read","input":{"file_path":"/r/src/app.rs"}},{"type":"tool_use","name":"Grep","input":{"pattern":"center","path":"/r/src"}}]}}"#
            ),
            ["Looking", "first app.rs", "Read src/app.rs", "Search center in src"]
        );
        assert_eq!(
            d(
                r#"{"type":"user","message":{"content":[{"type":"tool_result","is_error":true,"content":"String not found"}]}}"#
            ),
            ["  Didn't work: String not found"]
        );
        assert!(
            d(r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"1 fn main"}]}}"#).is_empty()
        );
        assert_eq!(d("warning: x"), ["warning: x"]);
    }

    #[test]
    fn keeps_the_choice_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let file = std::env::temp_dir().join(format!("faust-assistant-{}.json", std::process::id()));
        assert!(load(&file).is_none());
        let e = Endpoint { url: "http://localhost:11434/v1".into(), model: "qwen3-coder".into(), key: "k".into() };
        save(&file, &Choice::Model(e.clone())).unwrap();
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(load(&file), Some(Choice::Model(e)));
        let a = chosen(&file).unwrap();
        assert_eq!((a.name(), a.version.as_str()), ("qwen3-coder", "qwen3-coder (localhost:11434)"));
        save(&file, &Choice::ClaudeCode).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap().trim(), "{\n  \"kind\": \"claude-code\"\n}");
        let _ = std::fs::remove_file(file);
    }

    #[test]
    fn takes_the_suggested_change() {
        assert_eq!(take_change(" Hello! I'm water. "), ("Hello! I'm water.".into(), None));
        assert_eq!(
            take_change("I can't sing yet. I'd add a tune.\n[[change: Play a tune when the water is touched]]\n"),
            ("I can't sing yet. I'd add a tune.".into(), Some("Play a tune when the water is touched".into()))
        );
        assert_eq!(take_change("Cut off [[change: half"), ("Cut off".into(), Some("half".into())));
        assert_eq!(take_change("Empty [[change: ]]"), ("Empty".into(), None));
    }

    #[test]
    fn prompts_name_the_rules() {
        let b = Brief { app: "Hello".into(), rules: "FAUST.md".into() };
        let p = b.change("make it blue");
        assert!(
            p.contains("source of Hello")
                && p.contains("read FAUST.md")
                && p.contains("make it blue")
                && p.contains("lines of English"),
            "{p}"
        );
        let p = b.replay("make it blue", "Official update 2.0", "");
        assert!(
            p.contains("read FAUST.md")
                && p.contains("new official version (Official update 2.0)")
                && p.contains("Their request:\nmake it blue")
                && p.contains("(no notes)"),
            "{p}"
        );
        let t = b.talk("Hello", true);
        assert!(
            t.contains("You are Hello")
                && t.contains("read FAUST.md")
                && t.contains("[[change:")
                && t.ends_with("Hello"),
            "{t}"
        );
        let more = b.talk("Make it red", false);
        assert!(more.starts_with("They wrote:\nMake it red") && more.contains("[[change:"), "{more}");
    }
}
