//! An assistant Faust runs itself, for any model behind an OpenAI-compatible chat API: one on
//! this Mac (Ollama, LM Studio, llama.cpp, MLX, vLLM) or a hosted one (OpenAI, OpenRouter, …).
//!
//! The model only asks for tools; Faust carries them out. They list, read, search, write and
//! edit files in the repository, and nothing else: paths are relative to it, and a way out with
//! `..`, `.git` and symbolic links are refused. There is no tool that runs anything. Requests go
//! out through `/usr/bin/curl`, like Faust's downloads, so no HTTP stack is linked in.

use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::proc::{self, Limits};

/// One run may take this long: models on a Mac are slower than hosted ones.
const TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// Requests to the model in one run, at most.
const MAX_STEPS: usize = 100;
/// What one tool may hand back, so a small context isn't filled by one file.
const MAX_RESULT: usize = 60_000;
const MAX_LINES: usize = 2000;
const MAX_MATCHES: usize = 200;
const MAX_FILES: usize = 1000;
/// Files larger than this aren't read or searched.
const MAX_FILE: u64 = 2 << 20;

const SYSTEM: &str = "You are a careful programmer. You change files only through the tools you are given. \
     Paths are relative to the root of the repository. Read a file before you edit it.";

/// Where the model is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Endpoint {
    /// The API's base address, such as `http://localhost:11434/v1`.
    pub(crate) url: String,
    pub(crate) model: String,
    /// Sent as a bearer token; empty for most models on this Mac.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) key: String,
}

impl Endpoint {
    /// `http://localhost:11434/v1` -> `localhost:11434`, for showing.
    pub(crate) fn host(&self) -> &str {
        let rest = self.url.split_once("://").map_or(self.url.as_str(), |(_, r)| r);
        rest.split('/').next().unwrap_or(rest)
    }
}

/// The models the server lists, by id.
pub(crate) fn models(e: &Endpoint, log: &Path) -> Result<Vec<String>> {
    static NEVER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let limits = Limits { cancel: &NEVER, timeout: Some(Duration::from_secs(15)) };
    let list = request(e, "models", None, log, limits)?;
    Ok(list["data"].as_array().into_iter().flatten().filter_map(|m| m["id"].as_str().map(str::to_owned)).collect())
}

/// Asks the model to do `prompt` in `cwd`, after the conversation in `history` (empty for a
/// new one). Returns its summary and the conversation, to continue it after a failed build.
pub(crate) fn run(
    e: &Endpoint,
    cwd: &Path,
    prompt: &str,
    history: Vec<Value>,
    log: &Path,
    limits: Limits<'_>,
    on_log: &mut dyn FnMut(&str),
) -> Result<(String, Vec<Value>)> {
    let deadline = Instant::now() + TIMEOUT;
    let model = &e.model;
    on_log(
        &tr!(en: "Started ({model})", ja: "開始しました（{model}）", de: "Gestartet ({model})", it: "Avviato ({model})"),
    );
    let mut post = |body: &Value| {
        let left = deadline.saturating_duration_since(Instant::now());
        request(e, "chat/completions", Some(body), log, Limits { cancel: limits.cancel, timeout: Some(left) })
    };
    converse(&mut post, model, cwd, prompt, history, on_log)
}

/// The conversation, with `post` sending each request.
fn converse(
    post: &mut dyn FnMut(&Value) -> Result<Value>,
    model: &str,
    cwd: &Path,
    prompt: &str,
    mut messages: Vec<Value>,
    on_log: &mut dyn FnMut(&str),
) -> Result<(String, Vec<Value>)> {
    if messages.is_empty() {
        messages.push(json!({"role": "system", "content": SYSTEM}));
    }
    messages.push(json!({"role": "user", "content": prompt}));
    let tools = tools();
    for _ in 0..MAX_STEPS {
        let answer = post(&json!({"model": model, "messages": messages, "tools": tools}))?;
        let message = &answer["choices"][0]["message"];
        if !message.is_object() {
            bail!(tr!(
                en: "Can't read the assistant's result",
                ja: "アシスタントの結果を読めません",
                de: "Das Ergebnis des Assistenten ist nicht lesbar",
                it: "Impossibile leggere il risultato dell'assistente",
            ));
        }
        let content = message["content"].as_str().unwrap_or("");
        let text = without_thinking(content);
        let calls = message["tool_calls"].as_array().cloned().unwrap_or_default();
        // Only what the API takes back: some servers refuse fields they didn't expect.
        let mut kept = json!({"role": "assistant", "content": content});
        if !calls.is_empty() {
            kept["tool_calls"] = Value::Array(calls.clone());
        }
        messages.push(kept);
        if calls.is_empty() {
            on_log(
                &tr!(en: "Done with the changes", ja: "変更が終わりました", de: "Änderungen abgeschlossen", it: "Modifiche completate"),
            );
            return Ok((text, messages));
        }
        text.lines().map(str::trim).filter(|l| !l.is_empty()).for_each(|l| on_log(&super::assistant::short(l, 200)));
        for call in calls {
            let name = call["function"]["name"].as_str().unwrap_or("");
            // Arguments come as a JSON string, or from some servers as an object.
            let args = match &call["function"]["arguments"] {
                Value::String(s) => serde_json::from_str(s).unwrap_or(Value::Null),
                other => other.clone(),
            };
            on_log(&describe(name, &args));
            let result = tool(cwd, name, &args).unwrap_or_else(|e| {
                let first = super::assistant::short(&format!("{e:#}"), 160);
                on_log(&tr!(en: "  Didn't work: {first}", ja: "  うまくいきませんでした: {first}", de: "  Hat nicht geklappt: {first}", it: "  Non è riuscito: {first}"));
                format!("Error: {e:#}")
            });
            messages.push(json!({"role": "tool", "tool_call_id": call["id"], "content": result}));
        }
    }
    bail!(tr!(
        en: "The assistant didn't finish in {MAX_STEPS} steps",
        ja: "アシスタントが {MAX_STEPS} 手順のうちに終わりませんでした",
        de: "Der Assistent ist in {MAX_STEPS} Schritten nicht fertig geworden",
        it: "L'assistente non ha finito in {MAX_STEPS} passaggi",
    ))
}

/// The text without the `<think>…</think>` that reasoning models put before their answer.
fn without_thinking(text: &str) -> String {
    let mut rest = text;
    let mut out = String::new();
    while let Some(start) = rest.find("<think>") {
        out.push_str(&rest[..start]);
        rest = rest[start..].split_once("</think>").map_or("", |(_, after)| after);
    }
    out.push_str(rest);
    out.trim().to_owned()
}

/// A tool call, as a line for the progress log.
fn describe(name: &str, args: &Value) -> String {
    let path = args["path"].as_str().unwrap_or("");
    let claude_name = match name {
        "list_files" => "Glob",
        "read_file" => "Read",
        "search" => "Grep",
        "write_file" => "Write",
        "edit_file" => "Edit",
        other => other,
    };
    super::assistant::tool_line(claude_name, path, args["text"].as_str().unwrap_or(""))
}

fn tools() -> Value {
    let f = |name: &str, description: &str, properties: Value, required: &[&str]| {
        json!({"type": "function", "function": {"name": name, "description": description,
            "parameters": {"type": "object", "properties": properties, "required": required}}})
    };
    let path = |what: &str| json!({"type": "string", "description": format!("{what}, relative to the repository.")});
    json!([
        f(
            "list_files",
            "List the files in a folder of the repository and in its subfolders.",
            json!({"path": path("The folder; empty for the whole repository")}),
            &[],
        ),
        f(
            "read_file",
            "Read a text file. Each line starts with its number and a tab, which are not part of the file.",
            json!({
                "path": path("The file"),
                "offset": {"type": "integer", "description": "The first line to read; 1 by default."},
                "limit": {"type": "integer", "description": format!("How many lines to read; {MAX_LINES} at most.")},
            }),
            &["path"],
        ),
        f(
            "search",
            "Find the lines that contain a text (exactly, not a pattern) in the files of a folder.",
            json!({"text": {"type": "string"}, "path": path("The folder; empty for the whole repository")}),
            &["text"],
        ),
        f(
            "write_file",
            "Create a file, or replace everything in it.",
            json!({"path": path("The file"), "content": {"type": "string"}}),
            &["path", "content"],
        ),
        f(
            "edit_file",
            "Replace a piece of a file. old_text must appear in it exactly once, unless all is true; include enough lines around it.",
            json!({
                "path": path("The file"),
                "old_text": {"type": "string"},
                "new_text": {"type": "string"},
                "all": {"type": "boolean", "description": "Replace every place old_text appears."},
            }),
            &["path", "old_text", "new_text"],
        ),
    ])
}

/// Carries out one tool call in `root`. The error goes back to the model.
fn tool(root: &Path, name: &str, a: &Value) -> Result<String> {
    let s = |k: &str| a[k].as_str().unwrap_or("");
    let file = || {
        if s("path").trim().is_empty() {
            bail!("path is missing");
        }
        inside(root, s("path"))
    };
    let rel = |p: &Path| p.strip_prefix(root).unwrap_or(p).display().to_string();
    match name {
        "list_files" => {
            let mut files = Vec::new();
            walk(root, &inside(root, s("path"))?, &mut files)?;
            files.sort();
            let n = files.len();
            files.truncate(MAX_FILES);
            if n > MAX_FILES {
                files.push(format!("(and {} more)", n - MAX_FILES));
            }
            Ok(if files.is_empty() { "(no files)".into() } else { files.join("\n") })
        }
        "read_file" => {
            let text = read_text(&file()?)?;
            let first = a["offset"].as_u64().unwrap_or(1).max(1) as usize;
            let count = a["limit"].as_u64().map_or(MAX_LINES, |l| (l as usize).clamp(1, MAX_LINES));
            let mut out = String::new();
            for (n, line) in text.lines().enumerate().skip(first - 1).take(count) {
                out.push_str(&format!("{:>6}\t{line}\n", n + 1));
                if out.len() > MAX_RESULT {
                    out.push_str(&format!("(cut at line {}: read on with offset)\n", n + 1));
                    break;
                }
            }
            let total = text.lines().count();
            if first.saturating_add(count) <= total && out.len() <= MAX_RESULT {
                out.push_str(&format!("({total} lines in all: read on with offset)\n"));
            }
            Ok(if out.is_empty() { "(empty)".into() } else { out })
        }
        "search" => {
            let needle = s("text");
            if needle.is_empty() {
                bail!("text is missing");
            }
            let mut files = Vec::new();
            walk(root, &inside(root, s("path"))?, &mut files)?;
            files.sort();
            let mut out = Vec::new();
            for f in &files {
                let Ok(text) = read_text(&root.join(f)) else { continue };
                for (n, line) in text.lines().enumerate().filter(|(_, l)| l.contains(needle)) {
                    out.push(format!("{f}:{}: {}", n + 1, super::assistant::short(line.trim(), 200)));
                }
            }
            let n = out.len();
            out.truncate(MAX_MATCHES);
            if n > MAX_MATCHES {
                out.push(format!("(and {} more)", n - MAX_MATCHES));
            }
            Ok(if out.is_empty() { "(nothing found)".into() } else { out.join("\n") })
        }
        "write_file" => {
            let path = file()?;
            if path.is_dir() {
                bail!("{} is a folder", rel(&path));
            }
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&path, s("content"))?;
            Ok(format!("Wrote {}", rel(&path)))
        }
        "edit_file" => {
            let path = file()?;
            let (old, new) = (s("old_text"), s("new_text"));
            if old.is_empty() {
                bail!("old_text is missing");
            }
            let text = read_text(&path)?;
            let found = text.matches(old).count();
            let all = a["all"].as_bool().unwrap_or(false);
            match found {
                0 => bail!("old_text isn't in {}: read the file again and copy it exactly", rel(&path)),
                1 => {}
                n if !all => {
                    bail!("old_text appears {n} times in {}: include more lines around it, or set all", rel(&path))
                }
                _ => {}
            }
            std::fs::write(&path, text.replace(old, new))?;
            Ok(format!(
                "Changed {} in {}",
                if found == 1 { "1 place".into() } else { format!("{found} places") },
                rel(&path)
            ))
        }
        other => bail!("there is no tool called {other}"),
    }
}

/// `rel` inside `root`, or why not: no way out with `..`, no `.git`, no symbolic link on the way.
fn inside(root: &Path, rel: &str) -> Result<PathBuf> {
    let given = Path::new(rel.trim());
    // Models sometimes give the full path.
    let rel = given.strip_prefix(root).unwrap_or(given);
    let mut out = root.to_owned();
    let mut depth = 0usize;
    for c in rel.components() {
        match c {
            Component::CurDir => {}
            Component::Normal(part) => {
                if part == ".git" {
                    bail!("{} is inside .git, which is off limits", given.display());
                }
                out.push(part);
                depth += 1;
                if out.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink()) {
                    bail!("{} is a symbolic link, which is off limits", rel.display());
                }
            }
            Component::ParentDir if depth > 0 => {
                out.pop();
                depth -= 1;
            }
            _ => bail!("{} is outside the repository", given.display()),
        }
    }
    Ok(out)
}

/// The files under `dir`, relative to `root`. Skips `.git`, `target` and symbolic links.
fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    if dir.is_file() {
        out.push(dir.strip_prefix(root).unwrap_or(dir).display().to_string());
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)?.flatten() {
        let name = entry.file_name();
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() && name != ".git" && !(name == "target" && dir == root) {
            walk(root, &entry.path(), out)?;
        } else if kind.is_file() {
            out.push(entry.path().strip_prefix(root).unwrap_or(&entry.path()).display().to_string());
        }
    }
    Ok(())
}

fn read_text(path: &Path) -> Result<String> {
    let size = std::fs::metadata(path)?.len();
    if size > MAX_FILE {
        bail!("the file is too large ({size} bytes)");
    }
    String::from_utf8(std::fs::read(path)?).map_err(|_| anyhow!("not a text file"))
}

/// Deletes the file when dropped.
struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Sends one request (a GET without `body`) and returns the answer's JSON.
fn request(e: &Endpoint, path: &str, body: Option<&Value>, log: &Path, limits: Limits<'_>) -> Result<Value> {
    let url = format!("{}/{path}", e.url.trim_end_matches('/'));
    let mut cmd = Command::new("/usr/bin/curl");
    // `-q` first: no ~/.curlrc, whose proxy or headers would see the key and the source.
    cmd.args(["-q", "-sS", "--max-time", "900", "-w", "\n%{http_code}", "-H", "Content-Type: application/json"]);
    // The key goes in a file only this user can read, not on the command line, which the log
    // shows and other processes can see.
    let _headers = if e.key.is_empty() {
        None
    } else {
        let file =
            TempFile(std::env::temp_dir().join(format!("faust-{}.headers", proc::hex(&proc::random_bytes::<8>()))));
        let mut f =
            std::os::unix::fs::OpenOptionsExt::mode(std::fs::OpenOptions::new().write(true).create_new(true), 0o600)
                .open(&file.0)?;
        writeln!(f, "Authorization: Bearer {}", e.key.trim())?;
        cmd.arg("-H").arg(format!("@{}", file.0.display()));
        Some(file)
    };
    if body.is_some() {
        cmd.args(["--data-binary", "@-"]);
    }
    cmd.arg(&url);
    let mut curl_error = String::new();
    let out = proc::run(cmd, body.map(Value::to_string).as_deref(), log, limits, &mut |o| {
        if let proc::Output::Line(l) = o
            && l.starts_with("curl: (")
        {
            l.clone_into(&mut curl_error);
        }
    });
    let out = match out {
        Err(_) if !curl_error.is_empty() => {
            bail!(tr!(
                en: "Can't reach the model at {url}: {curl_error}",
                ja: "{url} のモデルにつながりません: {curl_error}",
                de: "Das Modell unter {url} ist nicht erreichbar: {curl_error}",
                it: "Impossibile raggiungere il modello su {url}: {curl_error}",
            ))
        }
        r => r?,
    };
    let (text, code) = out.trim_end().rsplit_once('\n').unwrap_or(("", out.trim()));
    let answer: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    if !code.starts_with('2') {
        let why = answer["error"]["message"]
            .as_str()
            .or(answer["error"].as_str())
            .map_or_else(|| super::assistant::short(text.trim(), 300), str::to_owned);
        bail!(tr!(
            en: "The model at {url} answered {code}: {why}",
            ja: "{url} のモデルが {code} を返しました: {why}",
            de: "Das Modell unter {url} hat mit {code} geantwortet: {why}",
            it: "Il modello su {url} ha risposto {code}: {why}",
        ));
    }
    if answer.is_null() {
        bail!(tr!(
            en: "The model at {url} didn't answer in JSON",
            ja: "{url} のモデルが JSON で答えませんでした",
            de: "Das Modell unter {url} hat nicht in JSON geantwortet",
            it: "Il modello su {url} non ha risposto in JSON",
        ));
    }
    Ok(answer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "faust-model-{}-{}",
            std::process::id(),
            proc::hex(&proc::random_bytes::<4>())
        ));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {\n    println!(\"Hello!\");\n}\n").unwrap();
        dir
    }

    #[test]
    fn keeps_tools_inside_the_repository() {
        let root = repo();
        assert_eq!(inside(&root, "src/../src/main.rs").unwrap(), root.join("src/main.rs"));
        assert_eq!(inside(&root, &root.join("src").display().to_string()).unwrap(), root.join("src"));
        assert_eq!(inside(&root, "").unwrap(), root);
        for bad in ["../x", "src/../../x", "/etc/passwd", ".git/config", "src/.git/x"] {
            assert!(inside(&root, bad).is_err(), "{bad}");
        }
        std::os::unix::fs::symlink("/tmp", root.join("out")).unwrap();
        assert!(inside(&root, "out/x").is_err());
        assert!(tool(&root, "write_file", &json!({"path": "out/x", "content": "x"})).is_err());
        let listed = tool(&root, "list_files", &json!({})).unwrap();
        assert_eq!(listed, "src/main.rs");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn tools_read_search_and_edit() {
        let root = repo();
        let read = tool(&root, "read_file", &json!({"path": "src/main.rs", "offset": 2, "limit": 1})).unwrap();
        assert!(read.starts_with("     2\t    println!(\"Hello!\");\n"), "{read}");
        let past = tool(&root, "read_file", &json!({"path": "src/main.rs", "offset": u64::MAX})).unwrap();
        assert!(!past.contains("lines in all"), "{past}");
        let found = tool(&root, "search", &json!({"text": "Hello"})).unwrap();
        assert_eq!(found, "src/main.rs:2: println!(\"Hello!\");");
        let missing = tool(&root, "edit_file", &json!({"path": "src/main.rs", "old_text": "Bye", "new_text": "x"}));
        assert!(missing.is_err());
        let twice = tool(&root, "edit_file", &json!({"path": "src/main.rs", "old_text": "n", "new_text": "N"}));
        assert!(twice.is_err());
        tool(&root, "edit_file", &json!({"path": "src/main.rs", "old_text": "Hello!", "new_text": "Hi!"})).unwrap();
        tool(&root, "write_file", &json!({"path": "src/new/a.rs", "content": "// a\n"})).unwrap();
        assert!(std::fs::read_to_string(root.join("src/main.rs")).unwrap().contains("Hi!"));
        assert_eq!(std::fs::read_to_string(root.join("src/new/a.rs")).unwrap(), "// a\n");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn converses_until_the_model_stops_calling_tools() {
        let root = repo();
        let mut answers = vec![
            json!({"choices": [{"message": {"content": "<think>hmm</think>Looking first", "tool_calls": [
                {"id": "c1", "type": "function", "function": {"name": "edit_file",
                 "arguments": "{\"path\":\"src/main.rs\",\"old_text\":\"Hello!\",\"new_text\":\"Hello, friend!\"}"}}]}}]}),
            json!({"choices": [{"message": {"content": "<think>done</think>\nMade the greeting friendlier."}}]}),
        ]
        .into_iter();
        let mut sent = Vec::new();
        let mut post = |body: &Value| {
            sent.push(body.clone());
            Ok(answers.next().unwrap())
        };
        let mut lines = Vec::new();
        let (summary, history) =
            converse(&mut post, "m", &root, "make it friendlier", Vec::new(), &mut |l| lines.push(l.to_owned()))
                .unwrap();
        assert_eq!(summary, "Made the greeting friendlier.");
        assert!(std::fs::read_to_string(root.join("src/main.rs")).unwrap().contains("Hello, friend!"));
        assert_eq!(lines, ["Looking first", "Edit src/main.rs", "Done with the changes"]);
        // system, user, assistant (tool call), tool, assistant
        assert_eq!(history.len(), 5);
        assert_eq!(history[3]["tool_call_id"], "c1");
        assert_eq!(sent[1]["messages"].as_array().unwrap().len(), 4);
        assert_eq!(sent[0]["tools"].as_array().unwrap().len(), 5);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn shows_the_host() {
        let e = Endpoint { url: "http://localhost:11434/v1".into(), model: "m".into(), key: String::new() };
        assert_eq!(e.host(), "localhost:11434");
        assert_eq!(without_thinking("a<think>b</think>c<think>d"), "ac");
    }
}
