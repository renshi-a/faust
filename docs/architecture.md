# Architecture

Faust is one executable with two jobs. On a developer's Mac it is the `faust` command. Inside
a packaged app it is the launcher: `faust package` copies the `faust` executable into the
bundle, and when it runs there with no arguments, it acts as the launcher.

```
crates/
  faust-launcher/   the `faust` command and the launcher
    src/build/      the Xcode-free build: tripwires, the two link stages, the bundle
    src/launcher/   the loop that runs on the user's Mac
    src/ui/         alerts, the setup window, the progress panel (AppKit, loaded at run time)
    templates/app/  what `faust new` writes
  faust-link/       the app's side of the protocol
examples/hello/     the template, filled in, for the tests
scripts/e2e.sh      the whole loop, without clicking
```

## On the user's Mac

```
~/Library/Application Support/<app id>/
  faust/
    state.json        the versions, which one is in use, which one is on trial
    repo/             the user's copy of the source: a git repository
    versions/0001/    each version's source, checked out and built
    target/           cargo's output for the version last chosen, which the next one starts from
    toolchain/        rustup and Rust
    logs/             launcher.log, app.log, and per version: <n>-build.log, <n>-assistant.log, <n>.diff
    devices.json      keys of devices the user trusts to answer confirmations
    assistant.json    the assistant the user chose, if not Claude Code (mode 600: it may hold an API key)
    lock              held while the launcher runs: one launcher per app
  …                   the app's own data, if it keeps any here
```

## The loop

The launcher's controller runs on its own thread; AppKit stays on the main thread, and the
controller hops there for every dialog. Long work (the assistant, builds) runs on a job
thread, so the app stays usable meanwhile.

**First start.** The setup window explains what will be installed and where, and installs it
when the user agrees: rustup and the pinned Rust release (from `static.rust-lang.org`, checked
against a SHA-256 built into Faust), the shipped source as the first commit of the
repository, the crates `Cargo.lock` pins, and the first version, built and tested.

**Every start.** The launcher takes a lock (a second launcher brings the running app forward
and leaves), starts the current version with the socket on fd 3, and watches it. A version
that calls `ready()` and then keeps running for a few seconds is *good*: `main` moves to its
commit and old versions beyond the newest five are removed. Holding Option at start opens the
list of versions first.

**A change.** The app sends `change` (or the user picks the menu item the app provides). Faust
shows the request and asks. On yes, the assistant runs in the repository with its tools
limited to reading, writing and searching files (see [the assistant](#the-assistant)). Faust commits the result on `try/<n>`, checks it out
into `versions/<n>`, and runs the gate: the build, the tests and the self-test, each in the
sandbox, where it may write only to `versions/<n>` and the temporary folders (see the
[security model](security.md)). The crates are fetched before that, and the build uses
`versions/<n>/target`, an APFS clone of `target/`; when the user switches to the version,
its copy becomes `target/`. When the gate fails, the assistant
gets the end of the log and two more tries, in the same conversation. A candidate that passes is offered to the user with its diff; on yes, the app is
asked to quit and the candidate starts *on trial*.

**A trial.** A candidate that can't start (it exits early, or shows nothing within the
timeout) is marked bad, its folder is removed, the work tree goes back to `main`, and the
previous version starts again, with an alert that says why. A good version that later fails to
start twice in a row is replaced by the previous good one.

**An official update.** The packaged app carries `release.json` with a release number (the
commit's time, or the moment a working tree was packaged). When it is larger than the last one
imported, the shipped source becomes a new commit on `upstream`. If the user changed nothing,
the update is checked as it is; otherwise `main` and `upstream` are merged three ways. A clean
merge is checked like any candidate. A conflicting one is offered to the assistant, with what the
user asked for so far (read from the commit messages) and the update's notes; or the user's
changes are set aside on `before/<release>`; or the user waits. A newer Rust release is
installed first, after asking.

## The assistant

Two kinds of assistant can make a change, and the user picks one from the change dialog
(**Another Assistant…**) or from the app, which can only open that window (`assistant.choose`).
ChatGPT and Gemini are offered by name: they are OpenAI-compatible APIs whose address Faust
knows, so the user gives only a key. Without a choice, Faust uses Claude Code when it is installed.

| | Claude Code | Any other model |
|---|---|---|
| Who runs the loop | Claude Code (`claude -p`, `stream-json`) | Faust (`launcher/model.rs`) |
| How Faust reaches it | the installed executable | an OpenAI-compatible `/chat/completions` API, through `/usr/bin/curl` |
| Tools | `Read,Edit,Write,Glob,Grep`, in restricted mode | `list_files`, `read_file`, `search`, `write_file`, `edit_file`, carried out by Faust |
| A second try | `--resume` with the session id | the same messages, continued |

The second column covers models on the user's Mac (Ollama, LM Studio, llama.cpp's server,
MLX, vLLM) and hosted ones (OpenAI, OpenRouter, and anything else that speaks the same API).
Faust lists the server's models (`GET /models`) and the user picks one; the address, the model
and an optional API key are kept in `faust/assistant.json`.

The split follows the one agent gateways such as OpenClaw make between *CLI backends* (a
local agent that owns its loop) and an *embedded runtime* (the gateway's own loop, talking to
any provider over HTTP). Faust keeps both deliberately small. It writes each backend's
contract in the launcher, never in settings or the source: Claude Code's flags, and the
model's five tools. Other agent CLIs are not run, because none has a mode that confines it to
file tools the way `--restricted` does, and Faust's promise to the user ("it can't run
commands") would then depend on the CLI. With its own loop, that promise doesn't depend on the
model at all: a model can only ask, and Faust decides what a tool call may touch.

## The repository

Faust handles git with [gitoxide](https://github.com/GitoxideLabs/gitoxide): `/usr/bin/git`
on a Mac without the Command Line Tools is only a shim that offers to install them.

| Ref | Holds |
|---|---|
| `main` | the version in use |
| `upstream` | official releases, one commit each, authored by the app with the release's own time, so the same release makes the same commit on every Mac |
| `try/<n>` | every candidate, used or not |
| `wip/<time>` | files the user edited by hand, saved before a switch replaced them |
| `before/<release>` | the user's changes, set aside for an official version |

A change's commit message keeps the request and the assistant's summary:

```
Make the greeting friendlier

Request:
Make the greeting friendlier

Summary:
Changed the greeting to "Hello, friend!".

Assistant: 2.1.310 (Claude Code)
```

## Building without Xcode

A Mac without Xcode has no linker and no SDK. Rust ships its own linker, `rust-lld`, but
linking a Mac binary still needs the SDK's `.tbd` stubs: text files that list what each
system library exports. Faust writes its own, in two stages.

1. **Stage 1** links against stubs that are empty (one per library in `/usr/lib` that Rust
   code uses, and one per framework in `/System/Library/Frameworks`), with
   `-undefined dynamic_lookup` so that nothing has to resolve. The result runs, but with
   flat-namespace lookups.
2. **The mini SDK.** Faust reads every symbol the stage-1 binaries import (the app, build
   scripts, proc macros), asks the running system which library defines each one (`dlsym`,
   then `dladdr`), and writes stubs that list exactly those symbols. Symbols from libSystem's
   parts bind to libSystem, from a framework's sub-frameworks to the framework, the way the
   real SDK re-exports them.
3. **Stage 2** links normally (two-level namespace) against the mini SDK. This is the binary
   that ships.

The mini SDK changes only when the code starts to use a system function it didn't use before.
So a rebuild tries stage 2 with the last mini SDK first, and goes back to stage 1 only when the
link reports an undefined symbol. With the release profile's `incremental = true` (the
template sets it), a one-line change rebuilds in about a second.

While cargo runs, its `PATH` has no `/usr/bin` and holds a tripwire for `cc`, `ld`, `xcrun`,
`codesign` and the other Apple and C tools: a script that logs the call and fails. A build
counts only when no tripwire fired. `DEVELOPER_DIR` points nowhere, so nothing finds Xcode by
other means. Tests and doctests link the same way as stage 1.

## The UI

The launcher's windows (alerts, the installer, the progress panel) use AppKit through
Objective-C messages, with AppKit loaded by `dlopen`, so Faust itself links nothing but
libSystem, libobjc and Foundation. They follow the Mac's language when it is English,
Japanese, German or Italian, and fall back to English.
