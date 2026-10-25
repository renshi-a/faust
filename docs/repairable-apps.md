# Designing repairable apps

Any app Faust can build is repairable in principle. These habits make the difference between
an app the assistant changes well and one it keeps breaking.

## Write `FAUST.md` for a newcomer

Claude Code reads `FAUST.md` before every change, and knows nothing else about the app. Write
it the way you would brief a capable contractor on their first day:

- **What the app is**, in two sentences, and where things live: which file draws the window,
  where data is saved, what talks to the network.
- **What must keep working**: the `faust_link` calls, the self-test, the data format, anything
  users rely on.
- **What not to touch**, and why. Then also list the paths in `[repair] protect`, which Faust
  enforces.
- **How to finish**: a few plain sentences on what changed, since they appear in the switch
  dialog and the commit message.

Keep it short. A page the assistant follows beats three it skims.

## Keep data readable by the previous version

The user can go back to any good version at any time, so data written by a newer version must
stay readable by an older one.

- Store data outside the source, in the app's support folder
  (`~/Library/Application Support/<id>`), never next to the code.
- Give the format a version number, and make readers ignore fields they don't know.
- Add fields; don't rename or remove them. A new meaning gets a new field.
- If a format must change, write both the old and the new form for a while.

Say this in `FAUST.md`: it is the rule the assistant is most likely to break without being told.

## Give the gate something to check

A change is only as safe as the checks it must pass.

- **Tests** that exercise behavior users care about, not just that the code compiles. When a
  test fails, Faust shows the assistant the failure, so a good failure message is a fix half
  written.
- **A self-test** (`[check] selftest`) that starts the parts that can fail at run time, without
  a window: load the settings, open the database, parse the assets, then exit 0. It runs before
  any version is offered.
- **`ready()` after the first window**, not before: a version counts as started only when it
  calls it, so call it once the user can see the app.

## Stay buildable without Xcode

The user's Mac has no C compiler. Prefer the standard library and pure-Rust crates, and run
`faust build` after adding a dependency: a crate with a C part trips a wire and the build stops.
Many crates offer a pure-Rust feature (`rustls` instead of `native-tls`, `miniz_oxide` instead
of zlib).

Link system frameworks the ordinary way (`#[link(name = "AppKit", kind = "framework")]` or a
crate such as `objc2-app-kit`) or load them at run time; both work with Faust's SDK.

## Keep the code easy to change

What helps a person new to the code helps the assistant too: small modules with one job each,
names that say what things are, comments that explain why, and no clever indirection. The
template's `src/main.rs` is deliberately plain.

## Let the app ask

Faust's menu item works for any app, but an app that understands what the user wants can ask
directly: `link.request_change(Some("Make the font larger"))`. Faust shows that text to the user
before anything happens, so the app suggests and the user decides.

## Updates

Your releases are merged with what each user changed. Small, focused commits merge more
cleanly than sweeping reformatting; a release that reformats every file guarantees conflicts for
every user who changed anything. Write release notes (the commit's subject becomes the update's
notes): they reach the assistant when it resolves a conflict.
