# Changelog

All notable changes to Faust are listed here. Faust follows [Semantic Versioning](https://semver.org);
before 1.0, minor versions may change the configuration and the protocol.

## Unreleased

- Talking to the app no longer always asks for a change. The template's field passes on what
  the user writes; the assistant answers as the app, reading its source with tools that only
  read, and suggests a change only when the app can't do what was asked yet. Faust asks once
  per assistant whether it may read what the user writes.
- Protocol 2: the app sends `say`, and Faust answers with `reply`. `faust-link` adds
  `Link::say` and `Event::Reply`; `say` falls back to a change request with an older launcher.
- The template's window shows water instead of a line of text: a wall of water falls onto a
  small dark planet and runs over it, ripples as the user types and follows the pointer. It is
  a fluid simulated on the GPU and drawn as glowing drops on a dark background, in shades that
  drift around its colour and with foam on the crests, through Metal, which needs no Xcode
  either. The window is always dark.

## 0.2.0 (2026-10-25)

- Changes can be made by any model with an OpenAI-compatible API, not just Claude Code:
  ChatGPT and Gemini by name with an API key, or a model at an address the user gives (Ollama,
  LM Studio, OpenRouter, …). Faust carries out its five file tools itself.
- The template asks for changes from a field at the bottom of its window instead of a menu
  item, and shows who will make the change. How an app takes requests is the user's to change,
  too.
- Protocol: `assistant.choose` opens Faust's window for choosing the assistant, and Faust
  sends `assistant` with the choice. `faust-link` 0.2 adds `Link::choose_assistant` and
  `Event::Assistant`.
- `faust dev` builds a new app first when it has no `Cargo.lock` yet.

## 0.1.0 (2026-10-25)

The first release.

- `faust new`, `build`, `test`, `dev`, `package` and `release`.
- The launcher: setup with its own Rust, changes by Claude Code with two confirmations, a
  sandboxed build-test-self-test gate, trial runs with rollback, a version list, official
  updates merged three ways with the user's changes, uninstalling.
- Builds without Xcode for any app: stubs for every system library and framework, a mini SDK
  from what the binaries import, tripwires on Apple and C tools, and rebuilds that skip the
  first link stage.
- `faust-link` 0.1, the app's side of protocol 1, with no dependencies.
- Claude Code runs in its restricted mode, with file tools only and hooks off.
- Builds, tests and self-tests run in a sandbox that may write only to the version's folder,
  cargo's output and caches, and the temporary folders.
- Protected paths, and confirmations answered from trusted, paired devices with signatures
  that cover the question shown.
- Test and development switches only in `faust dev` bundles and `test-hooks` builds.
- Windows in English, Japanese, German and Italian.
