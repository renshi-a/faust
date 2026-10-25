# Changelog

All notable changes to Faust are listed here. Faust follows [Semantic Versioning](https://semver.org);
before 1.0, minor versions may change the configuration and the protocol.

## 0.1.0 (unreleased)

The first release, split out of the launcher of the VisualClaw desk robot app.

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
