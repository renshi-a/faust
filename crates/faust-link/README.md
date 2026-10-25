# faust-link

The app's side of [Faust](https://github.com/renshi-a/faust): a few calls that let a
repairable app tell the launcher it is up, ask for a change, open the version list, and quit
when asked. It has no dependencies.

```toml
[dependencies]
faust-link = { git = "https://github.com/renshi-a/faust", tag = "v0.1.0" }
```

```rust
// Once the first window is on screen:
if let Some(link) = faust_link::get() {
    link.ready();
}

// From "Request a Change…" in the app's menu:
if let Some(link) = faust_link::get() {
    link.request_change(None);
}

// From inside the app, when it understood what the user wants. Faust shows the text to the
// user and asks before anything happens:
if let Some(link) = faust_link::get() {
    link.request_change(Some("Make the font larger"));
}

// In the event loop:
if let Some(link) = faust_link::get() {
    for event in link.events() {
        if event == faust_link::Event::Quit {
            // save and quit; Faust is switching to another version
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
