## What changes for users

<!-- What someone using an app made with Faust (or a developer running `faust`) will notice. -->

## How it works

<!-- The shape of the change, and why this shape. Link docs/architecture.md or docs/security.md sections it touches. -->

## How it was tested

<!-- What you ran and what you checked by hand. Say plainly what wasn't tested. -->

## Checklist

- [ ] `cargo fmt --check`
- [ ] `cargo clippy --all-targets -- -D warnings`
- [ ] `cargo test`
- [ ] `scripts/e2e.sh`, or why it wasn't needed
- [ ] Text users see goes through `tr!` (en, ja, de, it), or the languages I couldn't write are listed above
- [ ] No dependency that compiles C or needs Xcode
- [ ] Nothing that protects the user is read from an app's source; test switches go through `launcher::overrides`
- [ ] `examples/hello` still matches the template
- [ ] Docs updated (README, `docs/`)
