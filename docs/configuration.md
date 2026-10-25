# Configuration: `faust.toml`

`faust.toml` sits at the root of the app's repository, next to `Cargo.toml`. Unknown keys are
an error, so a typo doesn't go unnoticed.

```toml
[app]
name = "Happy Day"
id = "com.example.happyday"
description = "A calendar that remembers the good days."
icon = "assets/AppIcon.icns"
resources = ["assets"]

[app.descriptions]
ja = "いい日を覚えておくカレンダー。"

[app.info_plist]
LSUIElement = true
NSMicrophoneUsageDescription = "Happy Day listens when you hold the space bar."

[toolchain]
rust = "1.96.1"

[check]
tests = true
selftest = ["--selftest"]
ready_timeout_secs = 60
healthy_after_secs = 8

[repair]
rules = "FAUST.md"
protect = ["faust.toml", "LICENSE", "legal/"]

[package]
exclude = ["design", "notes.md"]
```

## Which copy counts

There are two copies of `faust.toml` on a user's Mac, and they decide different things.

| Copy | Comes from | Decides |
|---|---|---|
| The shipped one | the packaged app (signed) | the app's name and id, where its state lives, the Rust release to install, the rules file and protected paths, all of `[check]` |
| The version's own | that version's source (may have been changed) | how that version is built and bundled: binary, resources, icon, `Info.plist` |

The assistant can change a version's `faust.toml` unless it is protected (the template protects
it). Nothing that protects the user is read from a version's copy.

## `[app]`

| Key | Default | Meaning |
|---|---|---|
| `name` | required | Shown everywhere: the bundle's name, window titles, the installer. No `/`, and not starting with `.`. |
| `id` | required | The bundle identifier, such as `com.example.happyday`. Faust keeps its state in `~/Library/Application Support/<id>/faust`; the app may use the rest of that folder. The launcher's own bundle gets `<id>.faust`. |
| `binary` | the package name | The Cargo binary to bundle. |
| `version` | the package version | `CFBundleShortVersionString`, and part of the release's name. |
| `description` | `""` | One or two sentences for the installer's welcome page. |
| `descriptions` | `{}` | The same in other languages, by code: `en`, `ja`, `de`, `it`. |
| `icon` | none | An `.icns` file, relative to the repository. |
| `resources` | `[]` | Files and folders copied into `Contents/Resources`. |
| `minimum_macos` | `"11.0"` | `LSMinimumSystemVersion`, and the deployment target the app is built for. |
| `info_plist` | `{}` | Extra `Info.plist` keys. Strings, booleans and integers. They override Faust's own keys of the same name. |

## `[toolchain]`

| Key | Default | Meaning |
|---|---|---|
| `rust` | `"1.96.1"` | The Rust release, installed on the user's Mac with rustup (minimal profile). On your Mac, `faust build` also accepts a toolchain installed under another name (such as `stable`) when it is the same release. An official update that asks for a newer release installs it after asking the user. |

## `[check]`

A new version is used only when all of these pass.

| Key | Default | Meaning |
|---|---|---|
| `tests` | `true` | Run `cargo test --workspace`. |
| `selftest` | `[]` | Arguments that make the built app check itself and exit with 0, such as `["--selftest"]`. Without them there is no self-test. It runs in the build sandbox: it may write only to temporary folders, and should need no window. |
| `ready_signal` | `true` | The app calls `faust_link`'s `ready()` once its first window is up. Set it to `false` for an app that doesn't use `faust-link`: it then only has to keep running for `healthy_after_secs`. |
| `ready_timeout_secs` | `60` | How long a new version may take to call `ready()` before it is stopped and dropped. |
| `healthy_after_secs` | `8` | How long it must then keep running to count as good. |

## `[repair]`

| Key | Default | Meaning |
|---|---|---|
| `rules` | `"FAUST.md"` | The file Claude Code reads before it changes anything. |
| `protect` | `[]` | Paths the assistant must leave alone: files, or folders ending in `/`. If it changes one, Faust puts it back before committing. |

## `[package]`

| Key | Default | Meaning |
|---|---|---|
| `exclude` | `[]` | Paths left out of the source that ships in the app, besides `target`, `dist`, `.git` and what `.gitignore` lists. |

`faust package` also copies path dependencies that live outside the repository into
`.faust/vendor/` and points `Cargo.toml` at the copies, so the shipped source builds on its own.

## Environment variables

For developers and tests. Except `FAUST_LANG` and the `faust release` credentials, they count
only in a bundle `faust dev` made, or in a `faust` built with the `test-hooks` feature
(`scripts/e2e.sh` builds one): a released launcher ignores them.

| Variable | Used by | Meaning |
|---|---|---|
| `FAUST_SUPPORT_DIR` | launcher | Use this folder instead of `~/Library/Application Support/<id>`. |
| `FAUST_SYSTEM_RUST=1` | launcher | Use your `~/.rustup` and `~/.cargo` instead of installing Rust. (`faust dev` sets it.) |
| `FAUST_DEV=1` | launcher | Take official updates without asking. (`faust dev` sets it.) |
| `FAUST_LANG` | launcher | `en`, `ja`, `de` or `it` instead of the Mac's language. Passed on to the app. |
| `FAUST_ASSISTANT` | launcher | The Claude Code executable to use. |
| `FAUST_RESOURCES` | launcher | Read the shipped source from this folder instead of the bundle. |
| `FAUST_TEST_ANSWERS` | launcher | Answers for the next dialogs, separated by `\|`: a button index, `text:…` for a text field, `button:item` for the version list, `install` and `start` for the installer. |
| `FAUST_TEST_MESSAGES` | launcher | Messages to act on as if the app sent them, one per start: `change`, `versions`, `uninstall`. |
| `FAUST_TEST_QUIT_AFTER` | launcher | Ask the app to quit after this many seconds. |
| `FAUST_TEST_SNAPSHOT` | launcher | Write the setup window as a PNG and exit (`FAUST_TEST_SNAPSHOT_PAGE=0..3` or `failed`). |
| `FAUST_SIGN_IDENTITY`, `FAUST_NOTARY_PROFILE`, `FAUST_NOTARY_KEY`, `FAUST_NOTARY_KEY_ID`, `FAUST_NOTARY_ISSUER` | `faust release` | Signing identity and notarization credentials. |
