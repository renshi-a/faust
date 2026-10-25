# Getting started

This guide makes a small app, builds it without Xcode, opens it the way a user would, changes
it by asking, breaks it on purpose to watch Faust roll it back, and ends with a release you
can hand out. It takes about fifteen minutes.

You need an Apple silicon Mac with macOS 11 or later, [rustup](https://rustup.rs), git, and
[Claude Code](https://claude.com/claude-code) (signed in) for the change steps.

## 1. Install Faust

```sh
cargo install --git https://github.com/renshi-a/faust faust-launcher
faust --version
```

The `faust` command is also the launcher: `faust package` copies this very executable into the
apps it makes.

## 2. Create an app

```sh
faust new "Happy Day"
cd "Happy Day"
```

```
Happy Day/
├── Cargo.toml       an ordinary Rust package, plus faust-link
├── faust.toml       who the app is, and how Faust builds and checks it
├── FAUST.md         the rules Claude Code follows when it changes the app
└── src/main.rs      one window, and a menu with "Request a Change…"
```

The template app reaches AppKit through Objective-C messages and loads it at run time, so it
links nothing but `libSystem`, `libobjc` and Foundation. Any Rust UI works with Faust as long
as it builds without Xcode; the template is just the smallest thing that shows the loop.

Pin the Rust release the app is built with. `faust.toml` names it, and the user's Mac will
install exactly that release:

```sh
rustup toolchain install 1.96.1 --profile minimal   # skip if your stable is 1.96.1
```

## 3. Build it the way the user's Mac will

```sh
faust build
```

```
== stage 1: link against empty stubs with -undefined dynamic_lookup
== mini SDK: the symbols the binaries import
== stage 2: two-level namespace link against the mini SDK
== bundle
built …/Happy Day/dist/Happy Day.app
```

`faust build` runs cargo with no `/usr/bin` on its path and a tripwire in place of every
Xcode and C tool, so if this passes on your Mac it passes on a Mac without Xcode. The two link
stages are explained in the [architecture](architecture.md#building-without-xcode); the second
build onwards usually skips stage 1.

`faust test` runs the tests the same way. Commit, including `Cargo.lock`: the user's Mac fetches
exactly the crates it pins.

```sh
git init && git add -A && git commit -m "Start Happy Day"
```

## 4. Open it as a user would

```sh
faust dev
```

This packages the working tree into `target/faust/dev/Happy Day.app` and opens it. You see
what a user sees on the first start: an installer that explains what will happen, then builds
the first version. `faust dev` keeps its state under `target/faust/dev/support` and uses your
own rustup, so this takes seconds rather than minutes. (`faust dev --fresh` starts over and
installs Rust from scratch, as a user's Mac does.)

Click through, and the app opens.

## 5. Ask for a change

In the app, choose **Happy Day ▸ Request a Change…** and write what you want, for example:

> Make the greeting say good morning, and show today's date under it.

Faust shows the request, what Claude Code may touch, and what it can do, and asks before
anything starts. Then a panel shows Claude Code at work: the files it reads and edits. When it
is done, Faust builds the new version, runs the tests and `--selftest`, and asks whether to
switch. **Show Changes** opens the diff. Switch, and the app restarts as the new version.

If the build fails, Faust sends the end of the log back to Claude Code and lets it try again,
twice at most. You are only asked to switch to a version that passed.

## 6. Break it, and watch it come back

Ask for something that makes the app quit at once, for example:

> When the app starts, quit immediately with exit code 3.

The build and the tests pass, so Faust offers the switch. The new version starts, exits before
its window appears, and Faust puts the previous version back and tells you why. The broken
attempt is kept as a git branch (`try/0003`), but it is never used again.

## 7. Go back further

**Happy Day ▸ Choose a Version…** lists every good version with its date and what was asked
for. Choosing one switches to it at once, without building. Hold Option while opening the app
to see the list before anything starts.

Everything lives in a plain git repository you can inspect:

```sh
cd "target/faust/dev/support/faust/repo"
git log --oneline --graph --all
```

## 8. Ship an update

Change the app yourself, commit, and run `faust dev` again. The new working tree arrives as an
*official update*. Faust merges it with the changes "the user" (you, a moment ago) asked for:
cleanly when you touched different lines, with Claude Code's help when you didn't. In
`faust dev` the update is taken without asking; a user is asked first.

## 9. Release

`faust release` packages HEAD, signs the app with your Developer ID, notarizes it, staples the
ticket, and does the same for a `.dmg` with an Applications link:

```sh
xcrun notarytool store-credentials faust-notary   # once
faust release
```

```
dist/release/Happy Day.app
dist/release/Happy Day.dmg
dist/release/Happy Day.dmg.sha256
```

Hand out the `.dmg`. To publish an update, release again: users who open the new app get the
new source as an official update, merged with whatever they changed.

## Next

- [Designing repairable apps](repairable-apps.md): what to put in `FAUST.md`, and how to keep a
  user's data safe when they go back a version.
- [Configuration](configuration.md): every key in `faust.toml`.
- [Protocol](protocol.md): the messages behind `faust-link`, for apps in other languages.
