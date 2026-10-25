# Notes for coding agents

Faust is a launcher for macOS apps that their users change by asking: it builds the app from
source on the user's Mac, lets an assistant rewrite it, and switches versions only after a
build, tests, a self-test and the user's consent. Read [README.md](README.md), then
[docs/architecture.md](docs/architecture.md) before changing the launcher.

## Layout

- `crates/faust-launcher`: the `faust` command and the launcher (one executable).
  - `src/build/`: the Xcode-free build (tripwires, the two link stages, the bundle).
  - `src/launcher/`: what runs on the user's Mac. `controller.rs` is the loop; `setup`,
    `change`, `talk`, `versions`, `update` and `uninstall` add their parts to `Controller`.
  - `src/ui/`: AppKit through Objective-C messages, on the main thread.
  - `templates/app/`: what `faust new` writes. `examples/hello` is the same, filled in.
- `crates/faust-link`: the app's side of the protocol. No dependencies, and no workspace
  inheritance in its `Cargo.toml` (apps may carry a copy of it).

## Rules

- Everything is English: code, comments, logs, docs, commit messages. Text users see goes
  through `tr!(en: …, ja: …, de: …, it: …)`.
- No dependency that compiles C or needs Xcode, in Faust or in the template.
- Nothing that protects the user may be read from an app's source; it belongs in the launcher
  or in the shipped `faust.toml`. See [docs/security.md](docs/security.md).
- Environment variables that change what the launcher trusts or asks go through
  `launcher::overrides`, so a release ignores them.
- Keep `examples/hello` identical to the template (a unit test checks it).

## Checks

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
scripts/e2e.sh        # the whole loop with a fake assistant; opens windows for a few seconds
```
