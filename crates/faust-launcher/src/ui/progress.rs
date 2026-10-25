//! The floating panel shown while a change is made or a version is built: a status line, a
//! progress bar, a log of what the assistant does, and a Cancel button.

use std::cell::RefCell;
use std::ptr::null;

use objc2::rc::Allocated;
use objc2::runtime::{AnyObject, Bool};
use objc2::{class, msg_send};

use super::{Button, add, button, label, on_main, on_main_async, rect, set_frame, set_text};
use crate::objc::{Id, nsstring};

struct Panel {
    panel: Id,
    text: Id,
    bar: Id,
    log: Id,
    lines: Vec<String>,
}

thread_local! {
    static PANEL: RefCell<Option<Panel>> = const { RefCell::new(None) };
}

/// Lines kept in the panel's log (all of it is in `logs/<n>-assistant.log`).
const LOG_LINES: usize = 500;

pub(crate) fn show(title: &str) {
    let title = title.to_owned();
    on_main(move || {
        close_now();
        let (w, h) = (560.0, 380.0);
        let preparing = tr!(en: "Preparing…", ja: "準備しています…", de: "Wird vorbereitet…", it: "Preparazione…");
        // SAFETY: AppKit's documented messages on the main thread, with matching argument types.
        let panel = unsafe {
            let p: Allocated<AnyObject> = msg_send![class!(NSPanel), alloc];
            // Titled, utility window.
            let panel: Id = msg_send![p, initWithContentRect: rect(0.0, 0.0, w, h), styleMask: 1usize | (1 << 4), backing: 2usize, defer: Bool::NO];
            let _: () = msg_send![&*panel, setReleasedWhenClosed: Bool::NO];
            let _: () = msg_send![&*panel, setFloatingPanel: Bool::YES];
            let _: () = msg_send![&*panel, setHidesOnDeactivate: Bool::NO];
            let _: () = msg_send![&*panel, setTitle: &*nsstring(&title)];
            let content: Id = msg_send![&*panel, contentView];
            let text = label(&preparing, 13.0, false, false);
            let _: () = msg_send![&*text, setLineBreakMode: 4usize];
            set_frame(&text, rect(20.0, h - 40.0, w - 40.0, 18.0));
            add(&content, &text);
            let bar: Allocated<AnyObject> = msg_send![class!(NSProgressIndicator), alloc];
            let bar: Id = msg_send![bar, initWithFrame: rect(20.0, h - 68.0, w - 40.0, 16.0)];
            let _: () = msg_send![&*bar, setIndeterminate: Bool::YES];
            let _: () = msg_send![&*bar, startAnimation: null::<AnyObject>()];
            add(&content, &bar);
            // The log: read-only, the newest line at the bottom.
            let log_rect = rect(20.0, 56.0, w - 40.0, h - 68.0 - 12.0 - 56.0);
            let sv: Allocated<AnyObject> = msg_send![class!(NSScrollView), alloc];
            let sv: Id = msg_send![sv, initWithFrame: log_rect];
            let _: () = msg_send![&*sv, setHasVerticalScroller: Bool::YES];
            let _: () = msg_send![&*sv, setBorderType: 2usize];
            let tv: Allocated<AnyObject> = msg_send![class!(NSTextView), alloc];
            let log: Id = msg_send![tv, initWithFrame: rect(0.0, 0.0, log_rect.w, log_rect.h)];
            let _: () = msg_send![&*log, setRichText: Bool::NO];
            let _: () = msg_send![&*log, setEditable: Bool::NO];
            let _: () = msg_send![&*log, setAutoresizingMask: 2usize];
            let mono: Id = msg_send![class!(NSFont), monospacedSystemFontOfSize: 11.0f64, weight: 0.0f64];
            let _: () = msg_send![&*log, setFont: &*mono];
            let _: () = msg_send![&*sv, setDocumentView: &*log];
            add(&content, &sv);
            let cancel_title = tr!(en: "Cancel", ja: "中止", de: "Abbrechen", it: "Annulla");
            let cancel = button(&cancel_title, Button::Cancel, rect(w - 20.0 - 90.0, 14.0, 90.0, 30.0), false);
            add(&content, &cancel);
            let _: () = msg_send![&*panel, center];
            let _: () = msg_send![&*panel, orderFrontRegardless];
            Panel { panel, text, bar, log, lines: Vec::new() }
        };
        PANEL.with(|slot| *slot.borrow_mut() = Some(panel));
    });
}

/// Adds a line to the panel's log and scrolls to it.
pub(crate) fn log(line: &str) {
    let line = line.to_owned();
    on_main_async(move || {
        PANEL.with(|p| {
            let mut p = p.borrow_mut();
            let Some(p) = p.as_mut() else { return };
            p.lines.push(line);
            let over = p.lines.len().saturating_sub(LOG_LINES);
            p.lines.drain(..over);
            // SAFETY: NSTextView setters on the main thread.
            unsafe {
                let _: () = msg_send![&*p.log, setString: &*nsstring(&p.lines.join("\n"))];
                let _: () = msg_send![&*p.log, scrollToEndOfDocument: null::<AnyObject>()];
            }
        });
    });
}

/// `fraction`: `None` keeps the bar moving without a known end.
pub(crate) fn update(fraction: Option<f64>, text: &str) {
    let text = text.to_owned();
    on_main_async(move || {
        PANEL.with(|p| {
            let p = p.borrow();
            let Some(p) = p.as_ref() else { return };
            set_text(&p.text, &text);
            // SAFETY: NSProgressIndicator setters on the main thread.
            unsafe {
                match fraction {
                    Some(f) => {
                        let _: () = msg_send![&*p.bar, setIndeterminate: Bool::NO];
                        let _: () = msg_send![&*p.bar, setMinValue: 0.0f64];
                        let _: () = msg_send![&*p.bar, setMaxValue: 1.0f64];
                        let _: () = msg_send![&*p.bar, setDoubleValue: f.clamp(0.0, 1.0)];
                    }
                    None => {
                        let _: () = msg_send![&*p.bar, setIndeterminate: Bool::YES];
                        let _: () = msg_send![&*p.bar, startAnimation: null::<AnyObject>()];
                    }
                }
            }
        });
    });
}

pub(crate) fn close() {
    on_main(close_now);
}

fn close_now() {
    if let Some(p) = PANEL.with(|p| p.borrow_mut().take()) {
        // SAFETY: orderOut: with a nil sender, on the main thread.
        let _: () = unsafe { msg_send![&*p.panel, orderOut: null::<AnyObject>()] };
    }
}
