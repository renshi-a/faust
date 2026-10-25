# Protocol

Faust and the app talk over a Unix socket. `faust-link` implements the app's side for Rust;
this page is for apps in other languages, and for anyone curious.

## The socket

Faust starts the app with one end of a socket pair on a file descriptor and these variables:

| Variable | Meaning |
|---|---|
| `FAUST_LINK_FD` | The descriptor (3). Absent when the app was not started by Faust. |
| `FAUST_PROTOCOL` | The protocol version the launcher speaks (1). |
| `FAUST_VERSION` | The running version's number, as shown to the user (`#12`). |
| `FAUST_VERSION_TITLE` | What it is: `Official version 1.2.0`, or the change that was asked for. |
| `FAUST_SOURCE_DIR` | The user's copy of the source (a git repository). |
| `FAUST_LANG` | The language of Faust's windows (`en`, `ja`, `de`, `it`), for the app to follow. |

Both sides write one JSON object per line, with a `type`. A side ignores types it doesn't
know, so either can be newer. Writing to a socket whose other end is gone is not an error.

## From the app

| Message | Meaning |
|---|---|
| `{"type":"ready"}` | The first window is on screen. Send it once. |
| `{"type":"change"}` | Ask for a change: Faust asks the user what to change. |
| `{"type":"change","request":"…"}` | Ask for this change (the app understood it from the user). Faust shows it and asks before anything happens. |
| `{"type":"versions"}` | Open the list of versions. |
| `{"type":"assistant.choose"}` | Open Faust's window for choosing who makes the changes. The choice is made there, never by the app. |
| `{"type":"uninstall"}` | Uninstall, after asking the user. |
| `{"type":"device.trust","name":"…","public_key":"…"}` | A device was paired in the app. Faust asks the user whether that device may answer its confirmations. The key is a 32-byte Ed25519 public key in base64url. |
| `{"type":"device.forget","public_key":"…"}` | The device was unpaired. |
| `{"type":"confirmation.answer","id":"…","choice":"…","public_key":"…","signature":"…"}` | A device's answer to a confirmation (see below). |

## From Faust

| Message | Meaning |
|---|---|
| `{"type":"quit"}` | Quit soon: Faust is switching versions or uninstalling. After ten seconds Faust stops the app itself. |
| `{"type":"confirmation","id":"…","kind":"change"\|"switch","title":"…","text":"…","choices":[{"id":"…","label":"…"}]}` | Faust is asking the user something. An app that pairs with other devices may show it there too. |
| `{"type":"confirmation.closed","id":"…"}` | That confirmation was answered, on the Mac or on a device. |
| `{"type":"assistant","name":"…"}` | Who makes the next change, such as `Claude Code` or a model's name; empty when there is none yet. Sent after `ready` and whenever the user chooses another, for the app to show. |

## Starting

A new version counts as good when it sends `ready` within `ready_timeout_secs` of starting and
then keeps running for `healthy_after_secs` (see [configuration](configuration.md#check)). One
that sends nothing in time is stopped and dropped, and the previous version comes back; so is
one that exits before it is good. An app that doesn't use `faust-link` sets
`ready_signal = false` and only has to keep running.

Exiting with status 0 after `ready` means the user quit, even soon after the start: Faust
quits too, once any job it is running is done. A version still on trial stays on trial.

## Answers from other devices

Confirmations are the two questions that let a change happen: whether to ask the assistant
(`kind: "change"`) and whether to switch to a new version (`kind: "switch"`). The app may relay
them to a device the user paired with it, a phone for instance, and relay the answer back.

The app can be rewritten, so Faust doesn't take the app's word for an answer. It counts one
only when:

1. the confirmation is still open (an answer to a closed one is ignored),
2. the device's key was approved by the user in Faust's own dialog (after `device.trust`), and
3. `signature` is an Ed25519 signature, by that key, of the UTF-8 bytes

   ```
   faust-confirm|v2|<id>|<kind>|<digest>|<choice>
   ```

   where `digest` is the SHA-256 of the confirmation's `title`, a newline and its `text`, in
   lowercase hex. The device computes it from what it shows, so a signature is only good for
   the question the user saw.

The `id` is random and new for every confirmation, so a signature can't be replayed. Faust
shows the key's fingerprint (the first six bytes of its SHA-256, as `AB12 CD34 EF56`) when it
asks the user to trust it; show the same on the device. The approved keys are kept in
`faust/devices.json`, outside the source.

## Versions

The protocol number goes up when a change would confuse an older app. Launchers keep talking
to older apps in their version.
