# faust-link

The app's side of [Faust](https://github.com/renshi-a/faust): a few calls that let a
repairable app tell the launcher it is up, pass on what the user says to it, ask for a change,
open the version list, and quit when asked. It has no dependencies.

```toml
[dependencies]
faust-link = { git = "https://github.com/renshi-a/faust", tag = "v0.3.0" }
```

```rust
// Once the first window is on screen:
if let Some(link) = faust_link::get() {
    link.ready();
}

// From the field where the user talks to the app. Faust's assistant answers as the app, and
// suggests a change only when the app can't do what was asked yet:
if let Some(link) = faust_link::get() {
    let id = link.say("Make the font larger");
}

// Or ask for a change directly, when the app understood what the user wants. Faust shows the
// text to the user and asks before anything happens:
if let Some(link) = faust_link::get() {
    link.request_change(Some("Make the font larger"));
}

// In the event loop:
if let Some(link) = faust_link::get() {
    for event in link.events() {
        match event {
            faust_link::Event::Quit => { /* save and quit; Faust is switching versions */ }
            faust_link::Event::Reply(reply) => { /* show reply.text; reply.id is say()'s id */ }
            _ => {}
        }
    }
}
```

`get()` returns `None` when the app was not started by Faust (from `cargo run`, say), so every
call site can stay optional. The messages are described in
[docs/protocol.md](https://github.com/renshi-a/faust/blob/main/docs/protocol.md), for apps that
aren't written in Rust.

## License

MIT or Apache-2.0, at your option.
