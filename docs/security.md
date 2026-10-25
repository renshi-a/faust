# Security model

Faust lets an AI assistant rewrite an app on someone's Mac, at that person's request, and runs
the result. This page says who is trusted with what, what Faust does about it, and what it
does not cover.

## Who is trusted

| Party | Trusted to | Not trusted to |
|---|---|---|
| The user | decide what changes, and whether to use the result | |
| The developer | ship a launcher and a first source that behave | |
| The launcher | hold the rules below; it is signed and never rewritten | |
| The source, once changed | | anything: it may have been rewritten in any way |
| The assistant | edit files in the user's copy of the source | run commands, reach the network, touch anything else |
| The app, while it runs | ask for things (a change, the version list) | answer for the user |

The rest follows from one rule: **nothing that protects the user is read from the source**,
because the source is what changes. The launcher's code, and the copy of `faust.toml` that
shipped inside the signed bundle, hold the rules.

## What Faust does

**The user decides, twice.** Nothing is sent to the assistant until the user has seen the
request (in full), what the assistant may touch, and what it can do, and said yes. No version
is used until it passed the gate and the user said yes again, with the diff a click away. A
request that comes from the app (`change` with text) is shown the same way: the app can
suggest, not decide.

**The assistant has a small box.** Claude Code runs in its restricted mode (`--restricted`):
every settings file is ignored, the user's and any the repository holds, so a rewritten source
can't add hooks or permissions; the file tools are confined to the working directory, which
is the user's copy of the source. On top of that: `--tools Read,Edit,Write,Glob,Grep`, hooks
turned off, `--strict-mcp-config` with no servers, a fixed `PATH`, and Faust's own variables
removed from its environment. A Claude Code too old to have restricted mode isn't run. Paths
listed in the shipped `[repair] protect` are restored after every run, including when it
resolves a conflicting update.

**Building is sandboxed.** The build runs code from the source: build scripts, proc macros,
tests and the app's self-test. That happens before the user agrees to use the version, so each
step runs under `sandbox-exec`. It may write only to that version's own folder, cargo's
output, cargo's download cache (not its `bin`) and the temporary folders. The repository, the
state, the trusted devices, the other versions and the toolchain are read-only to it. It can't
start `open`, `osascript`, `launchctl` and similar tools, nor reach LaunchServices or Apple
Events, which would run work outside the sandbox. The crates are fetched in the same sandbox.
Whether the tests and the self-test run is decided by the shipped `faust.toml`, not by the
version's own copy.

**Bad versions don't stick.** A candidate that can't show a window in time, or exits before it
is good, is dropped and the previous version returns. Every version and candidate stays a git
commit, and the user can go back to any good version from the menu.

**Answers from other devices must be signed.** The app may relay Faust's confirmations to a
paired device, but it can't answer them through the protocol itself: an answer counts only
with an Ed25519 signature from a key the user approved in Faust's own dialog, after comparing
fingerprints. The signature covers a fresh random id, the kind of question, a digest of its
title and text, and the choice, so it can't be replayed or attached to a different question.
See the [protocol](protocol.md#answers-from-other-devices).

**No test switches in a release.** The environment variables that let tests and `faust dev`
drive the launcher (answering dialogs, choosing the assistant, moving the state) count only in
a bundle `faust dev` made, or in a `faust` built with the `test-hooks` feature.

**Downloads are pinned.** rustup-init is checked against a SHA-256 built into Faust. Crates
come from crates.io as `Cargo.lock` pins them (`cargo fetch --locked` on first start).

**Nothing needs an administrator.** Faust installs everything into the app's support folder
and changes no shell settings. Uninstalling from the app's menu removes that folder and moves
the app to the Trash.

## What Faust does not cover

- **The assistant's judgment.** Claude Code writes code that runs as the user. The two
  confirmations, the diff and the rollback are there so a person stays in charge, but a change
  that does something harmful and still builds, passes its tests and starts is possible. Read
  the diff for anything you would not hand to a stranger.
- **Prompt injection.** The assistant reads the source, including text a previous change put
  there. A crafted comment could try to steer a later change. Its tool limits bound what it can
  do directly; what it writes still goes through the same gate and confirmations.
- **Reading and the network.** The sandbox stops writes, not reads, and Faust does not block
  the network during builds: build scripts and tests can read what the user can, and code in
  the source could send it somewhere.
- **A perfect jail.** `sandbox-exec` is not a virtual machine. The profile closes the ways out
  we know of, but code that runs in it should still be treated as code you are running.
- **The app at run time.** Once the user switches to a version, it runs with the user's
  rights, like any app: it could edit Faust's files (including the list of trusted devices) or
  anything else the user can. Faust decides which version runs, and keeps every earlier one to
  go back to; it doesn't limit what the running version may do.
- **`sandbox-exec`.** Apple has deprecated it, though it still works on current macOS. Where it
  is missing, Faust builds without the sandbox.
- **The developer.** Faust doesn't vet what you ship. Users trust your signed launcher and
  your first source the way they trust any app they install.

## Reporting a vulnerability

Please follow [SECURITY.md](../SECURITY.md).
