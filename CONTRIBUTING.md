# Contributing to Faust

Thank you for helping. Bug reports, ideas, documentation fixes and code are all welcome.

## Before you start

- For a bug, open an issue with what you did, what happened, and the end of
  `~/Library/Application Support/<app id>/faust/logs/launcher.log` (and `<n>-build.log` if a
  build failed).
- For a larger change, open an issue first so we can agree on the shape before you write it.
- Security problems go to [SECURITY.md](SECURITY.md), not to the issue tracker.

## Building and testing

You need an Apple silicon Mac and rustup.

```sh
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

The end-to-end test runs the whole loop on `examples/hello` without clicking: setup, a change
whose first build fails and is fixed, a version that can't start and is rolled back, a
protected file put back, a clean and a conflicting official update, going back to an older
version, and uninstalling. A fake Claude Code (`scripts/e2e/fake-claude.sh`) makes the changes.
Windows appear for a few seconds in each scenario.

```sh
scripts/e2e.sh                 # uses your rustup, about three minutes
FRESH=1 scripts/e2e.sh         # installs Rust into the work folder, as on a user's Mac
```

`examples/hello` is the `faust new` template, filled in; a unit test keeps them identical. Change
`crates/faust-launcher/templates/app/` and the example together.

## Style

- `cargo fmt` (see `rustfmt.toml`) and `cargo clippy` with no warnings.
- Comments say why, not what. Every module starts with a `//!` line saying what it is for.
- `unsafe` only for Objective-C messages and system calls, each block with a `// SAFETY:`
  comment.
- No dependency that compiles C: Faust and the apps it builds must build with `rust-lld` alone.
- Text users see goes through `tr!` with English, Japanese, German and Italian. If you can't
  write a language, put the English text there and say so in the pull request.

## Commits and pull requests

- One topic per pull request, with a description of what changes for users and how you tested
  it.
- Commit messages in English, in the imperative ("Keep hand edits when switching versions").
- By contributing, you agree that your contribution is licensed under both the MIT and the
  Apache 2.0 licenses, as the README says.
