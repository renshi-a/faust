//! The coding assistant that changes the source: Claude Code, for now.
//!
//! It may only read, edit, write and search files in the user's repository. It runs in
//! Claude Code's restricted mode, which ignores every settings file (the user's, and any the
//! repository holds, since the repository may have been rewritten) and confines the file tools
//! to the working directory; hooks are off and there are no MCP servers. Building and testing
//! are the launcher's job.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde_json::Value;

use crate::proc::{self, Limits, Output};

/// One run may take this long.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(15 * 60);
const TOOLS: &str = "Read,Edit,Write,Glob,Grep";
/// Settings given on the command line, which restricted mode still applies.
const SETTINGS: &str = r#"{"disableAllHooks":true}"#;

/// An installed Claude Code.
#[derive(Debug, Clone)]
pub(crate) struct Assistant {
    pub(crate) path: PathBuf,
    pub(crate) version: String,
    /// Has `--restricted`. Faust doesn't run a Claude Code without it.
    pub(crate) restricted: bool,
}

impl Assistant {
    /// "2.1.300 (Claude Code) (~/.local/bin/claude)", for showing.
    pub(crate) fn title(&self) -> String {
        format!("{} ({})", self.version, super::paths::Paths::tilde(&self.path))
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
        Some(Assistant { path, version, restricted })
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
    pub(crate) session: Option<String>,
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
}

/// `claude -p` with the prompt on stdin; events come back as JSON lines and the last one is
/// the result. `on_log` gets what it is doing as it goes (files read and changed, what it says).
pub(crate) fn run(
    a: &Assistant,
    cwd: &Path,
    prompt: &str,
    resume: Option<&str>,
    log: &Path,
    limits: Limits<'_>,
    on_log: &mut dyn FnMut(&str),
) -> Result<Outcome> {
    let mut cmd = Command::new(&a.path);
    if !a.restricted {
        bail!(outdated(&a.version));
    }
    cmd.arg("-p")
        .args(["--output-format", "stream-json", "--verbose"])
        .arg("--restricted")
        .args(["--settings", SETTINGS])
        .args(["--permission-mode", "acceptEdits"])
        .args(["--tools", TOOLS])
        .arg("--strict-mcp-config");
    if let Some(id) = resume {
        cmd.args(["--resume", id]);
    }
    cmd.current_dir(cwd).env("PATH", search_path());
    for k in ["FAUST_LINK_FD", "FAUST_VERSION", "FAUST_SUPPORT_DIR"] {
        cmd.env_remove(k);
    }
    let out = proc::run(cmd, Some(prompt), log, Limits { cancel: limits.cancel, timeout: Some(TIMEOUT) }, &mut |o| {
        if let Output::Line(l) = o {
            for line in describe(l, cwd) {
                on_log(&line);
            }
        }
    })?;
    parse(&out)
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
                        let pattern = s(&input["pattern"]);
                        out.push(match c["name"].as_str().unwrap_or("?") {
                            "Read" => tr!(en: "Read {path}", ja: "読む: {path}", de: "Lesen: {path}", it: "Lettura: {path}"),
                            "Edit" => tr!(en: "Edit {path}", ja: "編集: {path}", de: "Bearbeiten: {path}", it: "Modifica: {path}"),
                            "Write" => tr!(en: "Write {path}", ja: "書き込み: {path}", de: "Schreiben: {path}", it: "Scrittura: {path}"),
                            "Glob" | "Grep" if path.is_empty() => tr!(en: "Search {pattern}", ja: "検索: {pattern}", de: "Suchen: {pattern}", it: "Ricerca: {pattern}"),
                            "Glob" | "Grep" => tr!(en: "Search {pattern} in {path}", ja: "検索: {pattern}（{path}）", de: "Suchen: {pattern} in {path}", it: "Ricerca: {pattern} in {path}"),
                            other => other.to_owned(),
                        });
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

fn short(s: &str, n: usize) -> String {
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
    Ok(Outcome { summary, session: result["session_id"].as_str().map(str::to_owned) })
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
        assert_eq!((o.summary.as_str(), o.session.as_deref()), ("Made the eyes bigger", Some("abc")));
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
    }
}
