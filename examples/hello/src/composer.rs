//! The field where the person asks for a change, in the style of a chat input: a rounded
//! box with a terracotta frame, a text view that grows to six lines (Enter sends, Shift+Enter
//! is a new line), a pill that shows who makes the change and opens Faust's window for
//! choosing another, and a send button. The box draws its own backgrounds, so they follow
//! light and dark mode.

use std::cell::Cell;
use std::ffi::{CStr, c_char, c_void};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, Bool, NSObject, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, class, define_class, extern_class, msg_send, sel};

use crate::{Id, Point, Rect, Size, nsstring};

const LINE_HEIGHT: f64 = 22.0;
const MAX_LINES: f64 = 6.0;
/// Inside the frame line: the text and the button row are this far from it.
const PADDING: f64 = 12.0;
/// Width of the soft glow outside the frame line.
const INSET: f64 = 4.0;
/// The row under the text, with the assistant pill and the send button.
const BAR_H: f64 = 32.0;
const PILL_H: f64 = 28.0;
/// The drawn pill is this much wider than its title on each side.
const PILL_PAD: f64 = 12.0;
const SEND: f64 = 32.0;
const EDGE: f64 = INSET + PADDING;
/// NSEventModifierFlagShift
const SHIFT: usize = 1 << 17;

/// The box's height for a text area `text_h` high.
fn height(text_h: f64) -> f64 {
    EDGE + BAR_H + PADDING + 1.0 + PADDING + text_h + EDGE
}

/// Where the send button sits in a box with these bounds.
fn send_frame(b: Rect) -> Rect {
    Rect { x: b.w - EDGE - SEND, y: EDGE, w: SEND, h: SEND }
}

/// The frame's terracotta.
fn accent(alpha: f64) -> Id {
    // SAFETY: an NSColor class method with matching types.
    unsafe { msg_send![class!(NSColor), colorWithSRGBRed: 0.70f64, green: 0.33f64, blue: 0.24f64, alpha: alpha] }
}

fn rounded(r: Rect, radius: f64) -> Id {
    // SAFETY: an NSBezierPath class method with matching types.
    unsafe { msg_send![class!(NSBezierPath), bezierPathWithRoundedRect: r, xRadius: radius, yRadius: radius] }
}

/// Fills a rounded rectangle; only inside drawRect:.
fn fill(r: Rect, radius: f64, color: &AnyObject) {
    // SAFETY: NSColor and NSBezierPath messages while a view draws.
    unsafe {
        let _: () = msg_send![color, set];
        let _: () = msg_send![&*rounded(r, radius), fill];
    }
}

/// Strokes a rounded rectangle; only inside drawRect:.
fn stroke(r: Rect, radius: f64, width: f64, color: &AnyObject) {
    // SAFETY: NSColor and NSBezierPath messages while a view draws.
    unsafe {
        let _: () = msg_send![color, set];
        let path = rounded(r, radius);
        let _: () = msg_send![&*path, setLineWidth: width];
        let _: () = msg_send![&*path, stroke];
    }
}

/// One of AppKit's string constants, such as `NSFontAttributeName`.
fn appkit_constant(name: &CStr) -> *const AnyObject {
    unsafe extern "C" {
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }
    // SAFETY: dlsym with RTLD_DEFAULT and a NUL-terminated name; AppKit is loaded by then.
    let p = unsafe { dlsym(-2isize as *mut c_void, name.as_ptr()) } as *const *const AnyObject;
    assert!(!p.is_null(), "AppKit has no {name:?}");
    // SAFETY: the symbol is an `NSString *const`.
    unsafe { *p }
}

fn rust_string(s: &AnyObject) -> String {
    // SAFETY: -[NSString UTF8String] returns a NUL-terminated string that lives as long as `s`.
    unsafe {
        let p: *const c_char = msg_send![s, UTF8String];
        if p.is_null() { String::new() } else { CStr::from_ptr(p).to_string_lossy().into_owned() }
    }
}

// The AppKit classes subclassed here. They are looked up by name at run time (AppKit is
// loaded then), and only their NSObject part is declared.
extern_class!(
    // SAFETY: an NSObject subclass, only used as a superclass.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSView"]
    pub struct NSView;
);
extern_class!(
    // SAFETY: an NSObject subclass, only used as a superclass.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSTextView"]
    pub struct NSTextView;
);

define_class!(
    // SAFETY: an NSTextView that draws a placeholder (its ivar) while it is empty.
    #[unsafe(super(NSTextView, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "AppComposerText"]
    #[ivars = String]
    struct PlaceholderText;

    impl PlaceholderText {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, dirty: Rect) {
            // SAFETY: NSTextView, NSDictionary and NSAttributedString messages while the view draws.
            unsafe {
                let _: () = msg_send![super(self), drawRect: dirty];
                let s: Id = msg_send![self, string];
                let len: usize = msg_send![&*s, length];
                let marked: Bool = msg_send![self, hasMarkedText];
                if len > 0 || marked.as_bool() {
                    return;
                }
                let attrs: Id = msg_send![class!(NSMutableDictionary), new];
                let font: Id = msg_send![self, font];
                let color: Id = msg_send![class!(NSColor), placeholderTextColor];
                let _: () = msg_send![&*attrs, setObject: &*font, forKey: appkit_constant(c"NSFontAttributeName")];
                let _: () =
                    msg_send![&*attrs, setObject: &*color, forKey: appkit_constant(c"NSForegroundColorAttributeName")];
                let a: Allocated<AnyObject> = msg_send![class!(NSAttributedString), alloc];
                let a: Id = msg_send![a, initWithString: &*nsstring(self.ivars()), attributes: &*attrs];
                let inset: Size = msg_send![self, textContainerInset];
                let _: () = msg_send![&*a, drawAtPoint: Point { x: inset.w, y: inset.h }];
            }
        }
    }
);

pub struct Parts {
    text: Retained<PlaceholderText>,
    scroll: Id,
    line: Id,
    pill: Id,
    send: Id,
    send_active: Cell<bool>,
    /// The pill's background, around the `pill` button.
    pill_frame: Cell<Rect>,
}

define_class!(
    // SAFETY: an NSView that draws its own background and lays out its subviews; it is the
    // text view's delegate and the send button's target. It adds no Drop.
    #[unsafe(super(NSView, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "AppComposer"]
    #[ivars = Parts]
    pub struct Composer;

    impl Composer {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: Rect) {
            // SAFETY: NSView and NSColor messages while the view draws.
            unsafe {
                let b: Rect = msg_send![self, bounds];
                // Soft glow, the body, then the terracotta frame line.
                fill(Rect { x: 1.0, y: 1.0, w: b.w - 2.0, h: b.h - 2.0 }, 14.0, &accent(0.16));
                let body = Rect { x: INSET, y: INSET, w: b.w - INSET * 2.0, h: b.h - INSET * 2.0 };
                let background: Id = msg_send![class!(NSColor), textBackgroundColor];
                fill(body, 11.0, &background);
                stroke(body, 11.0, 1.5, &accent(1.0));
                let pill = self.ivars().pill_frame.get();
                let quaternary: Id = msg_send![class!(NSColor), quaternaryLabelColor];
                fill(pill, pill.h / 2.0, &quaternary);
                let active = self.ivars().send_active.get();
                fill(send_frame(b), 8.0, &accent(if active { 1.0 } else { 0.45 }));
            }
        }

        #[unsafe(method(resizeSubviewsWithOldSize:))]
        fn resize_subviews(&self, _old: Size) {
            self.layout();
        }

        #[unsafe(method(textDidChange:))]
        fn text_did_change(&self, _note: *mut AnyObject) {
            self.changed();
        }

        #[unsafe(method(textView:doCommandBySelector:))]
        fn do_command(&self, text_view: *mut AnyObject, command: Sel) -> Bool {
            if command != sel!(insertNewline:) {
                return Bool::NO;
            }
            // SAFETY: NSTextView and NSEvent messages on the main thread.
            unsafe {
                // Enter while converting (an input method) commits the text instead.
                let marked: Bool = msg_send![text_view, hasMarkedText];
                if marked.as_bool() {
                    return Bool::NO;
                }
                let flags: usize = msg_send![class!(NSEvent), modifierFlags];
                if flags & SHIFT != 0 {
                    let _: () = msg_send![text_view, insertNewlineIgnoringFieldEditor: std::ptr::null::<AnyObject>()];
                    return Bool::YES;
                }
            }
            self.submit();
            Bool::YES
        }

        #[unsafe(method(send:))]
        fn send(&self, _sender: *mut AnyObject) {
            self.submit();
        }

        #[unsafe(method(chooseAssistant:))]
        fn choose_assistant(&self, _sender: *mut AnyObject) {
            if let Some(link) = faust_link::get() {
                link.choose_assistant();
            }
        }
    }
);

impl Composer {
    /// A field `width` wide that asks Faust for a change with what is written in it.
    pub fn new(mtm: MainThreadMarker, width: f64, placeholder: &str) -> Retained<Composer> {
        let text_w = width - EDGE * 2.0;
        // SAFETY: AppKit's documented messages, sent on the main thread with matching types.
        unsafe {
            let t = PlaceholderText::alloc(mtm).set_ivars(placeholder.to_owned());
            let text: Retained<PlaceholderText> =
                msg_send![super(t), initWithFrame: Rect { x: 0.0, y: 0.0, w: text_w, h: LINE_HEIGHT }];
            let font: Id = msg_send![class!(NSFont), systemFontOfSize: 15.0f64];
            let _: () = msg_send![&*text, setFont: &*font];
            let label: Id = msg_send![class!(NSColor), labelColor];
            let _: () = msg_send![&*text, setTextColor: &*label];
            let _: () = msg_send![&*text, setDrawsBackground: Bool::NO];
            let _: () = msg_send![&*text, setRichText: Bool::NO];
            let _: () = msg_send![&*text, setAllowsUndo: Bool::YES];
            let _: () = msg_send![&*text, setTextContainerInset: Size { w: 0.0, h: 2.0 }];
            let container: Id = msg_send![&*text, textContainer];
            let _: () = msg_send![&*container, setLineFragmentPadding: 0.0f64];
            let _: () = msg_send![&*text, setMinSize: Size { w: 0.0, h: LINE_HEIGHT }];
            let _: () = msg_send![&*text, setMaxSize: Size { w: 1.0e7, h: 1.0e7 }];
            let _: () = msg_send![&*text, setVerticallyResizable: Bool::YES];
            let _: () = msg_send![&*text, setHorizontallyResizable: Bool::NO];
            let _: () = msg_send![&*text, setAutoresizingMask: 2usize]; // width follows the scroll view

            let s: Allocated<AnyObject> = msg_send![class!(NSScrollView), alloc];
            let scroll: Id = msg_send![s, initWithFrame: Rect { x: 0.0, y: 0.0, w: text_w, h: LINE_HEIGHT }];
            let _: () = msg_send![&*scroll, setDocumentView: &*text];
            let _: () = msg_send![&*scroll, setDrawsBackground: Bool::NO];
            let _: () = msg_send![&*scroll, setHasVerticalScroller: Bool::YES];
            let _: () = msg_send![&*scroll, setAutohidesScrollers: Bool::YES];
            let _: () = msg_send![&*scroll, setBorderType: 0usize];

            let l: Allocated<AnyObject> = msg_send![class!(NSBox), alloc];
            let line: Id = msg_send![l, initWithFrame: Rect { x: 0.0, y: 0.0, w: width, h: 1.0 }];
            let _: () = msg_send![&*line, setBoxType: 2usize]; // a separator

            let _: () = msg_send![&*text, setToolTip: &*nsstring("Enter to send, Shift+Enter for a new line")];

            // The buttons' backgrounds are drawn by the box.
            let b: Allocated<AnyObject> = msg_send![class!(NSButton), alloc];
            let pill: Id = msg_send![b, initWithFrame: Rect { x: 0.0, y: 0.0, w: 100.0, h: PILL_H }];
            let _: () = msg_send![&*pill, setBordered: Bool::NO];
            let _: () = msg_send![&*pill, setImagePosition: 3usize]; // the chevron right of the title
            let chevron: Option<Id> = msg_send![class!(NSImage), imageWithSystemSymbolName: &*nsstring("chevron.down"), accessibilityDescription: &*nsstring("Choose")];
            if let Some(chevron) = chevron {
                let config: Id =
                    msg_send![class!(NSImageSymbolConfiguration), configurationWithPointSize: 10.0f64, weight: 0.3f64];
                let chevron: Id = msg_send![&*chevron, imageWithSymbolConfiguration: &*config];
                let _: () = msg_send![&*pill, setImage: &*chevron];
            }
            let secondary: Id = msg_send![class!(NSColor), secondaryLabelColor];
            let _: () = msg_send![&*pill, setContentTintColor: &*secondary];
            let _: () = msg_send![&*pill, setToolTip: &*nsstring("Choose who makes the changes")];

            let b: Allocated<AnyObject> = msg_send![class!(NSButton), alloc];
            let send: Id = msg_send![b, initWithFrame: Rect { x: 0.0, y: 0.0, w: SEND, h: SEND }];
            let _: () = msg_send![&*send, setBordered: Bool::NO];
            let _: () = msg_send![&*send, setImagePosition: 1usize]; // image only
            let arrow: Option<Id> = msg_send![class!(NSImage), imageWithSystemSymbolName: &*nsstring("arrow.up"), accessibilityDescription: &*nsstring("Send")];
            if let Some(arrow) = arrow {
                // 0.3 is NSFontWeightSemibold.
                let config: Id =
                    msg_send![class!(NSImageSymbolConfiguration), configurationWithPointSize: 14.0f64, weight: 0.3f64];
                let arrow: Id = msg_send![&*arrow, imageWithSymbolConfiguration: &*config];
                let _: () = msg_send![&*send, setImage: &*arrow];
            }
            let white: Id = msg_send![class!(NSColor), whiteColor];
            let _: () = msg_send![&*send, setContentTintColor: &*white];
            let _: () = msg_send![&*send, setToolTip: &*nsstring("Send (Enter)")];

            let parts = Parts {
                text,
                scroll,
                line,
                pill,
                send,
                send_active: Cell::new(false),
                pill_frame: Cell::new(Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 }),
            };
            let this = Composer::alloc(mtm).set_ivars(parts);
            let this: Retained<Composer> =
                msg_send![super(this), initWithFrame: Rect { x: 0.0, y: 0.0, w: width, h: height(LINE_HEIGHT) }];
            let p = this.ivars();
            for view in [&*p.scroll, &*p.line, &*p.pill, &*p.send] {
                let _: () = msg_send![&*this, addSubview: view];
            }
            let _: () = msg_send![&*p.text, setDelegate: &*this];
            let _: () = msg_send![&*p.send, setTarget: &*this];
            let _: () = msg_send![&*p.send, setAction: sel!(send:)];
            let _: () = msg_send![&*p.pill, setTarget: &*this];
            let _: () = msg_send![&*p.pill, setAction: sel!(chooseAssistant:)];
            this.set_assistant("");
            this.changed();
            this
        }
    }

    /// Puts the cursor in the field.
    pub fn focus(&self) {
        // SAFETY: NSView and NSWindow messages on the main thread.
        unsafe {
            let window: Option<Id> = msg_send![self, window];
            if let Some(w) = window {
                let _: Bool = msg_send![&*w, makeFirstResponder: &*self.ivars().text];
            }
        }
    }

    /// Shows who makes the next change (Faust says, with `Event::Assistant`).
    pub fn set_assistant(&self, name: &str) {
        let title = if name.is_empty() { "Choose an assistant" } else { name };
        let p = self.ivars();
        // SAFETY: NSDictionary, NSAttributedString and NSButton messages on the main thread.
        unsafe {
            let attrs: Id = msg_send![class!(NSMutableDictionary), new];
            let font: Id = msg_send![class!(NSFont), systemFontOfSize: 13.0f64, weight: 0.23f64]; // medium
            let color: Id = msg_send![class!(NSColor), labelColor];
            let _: () = msg_send![&*attrs, setObject: &*font, forKey: appkit_constant(c"NSFontAttributeName")];
            let _: () =
                msg_send![&*attrs, setObject: &*color, forKey: appkit_constant(c"NSForegroundColorAttributeName")];
            let a: Allocated<AnyObject> = msg_send![class!(NSAttributedString), alloc];
            let a: Id = msg_send![a, initWithString: &*nsstring(&format!("{title} ")), attributes: &*attrs];
            let _: () = msg_send![&*p.pill, setAttributedTitle: &*a];
        }
        self.layout();
        // SAFETY: -[NSView setNeedsDisplay:] on the main thread.
        let _: () = unsafe { msg_send![self, setNeedsDisplay: Bool::YES] };
    }

    fn text(&self) -> String {
        // SAFETY: -[NSTextView string] on the main thread.
        let s: Id = unsafe { msg_send![&*self.ivars().text, string] };
        rust_string(&s)
    }

    /// Sends what is written to Faust, which shows it and asks before anything changes.
    fn submit(&self) {
        let text = self.text();
        let request = text.trim();
        if request.is_empty() {
            return;
        }
        if let Some(link) = faust_link::get() {
            link.request_change(Some(request));
        }
        // SAFETY: -[NSTextView setString:] on the main thread.
        let _: () = unsafe { msg_send![&*self.ivars().text, setString: &*nsstring("")] };
        self.changed();
    }

    fn changed(&self) {
        let p = self.ivars();
        let active = !self.text().trim().is_empty();
        p.send_active.set(active);
        // SAFETY: NSControl and NSView messages on the main thread.
        unsafe {
            let _: () = msg_send![&*p.send, setEnabled: Bool::new(active)];
            let _: () = msg_send![&*p.text, setNeedsDisplay: Bool::YES];
            let _: () = msg_send![self, setNeedsDisplay: Bool::YES];
        }
        self.layout();
    }

    /// The text's height at `width`, from one line to six.
    fn text_height(&self, width: f64) -> f64 {
        let p = self.ivars();
        // SAFETY: NSView, NSTextView and NSLayoutManager messages on the main thread.
        unsafe {
            let s: Rect = msg_send![&*p.scroll, frame];
            if (s.w - width).abs() > 0.5 {
                let _: () = msg_send![&*p.scroll, setFrameSize: Size { w: width, h: s.h }];
            }
            let layout: Id = msg_send![&*p.text, layoutManager];
            let container: Id = msg_send![&*p.text, textContainer];
            let _: () = msg_send![&*layout, ensureLayoutForTextContainer: &*container];
            let used: Rect = msg_send![&*layout, usedRectForTextContainer: &*container];
            (used.h + 4.0).clamp(LINE_HEIGHT, LINE_HEIGHT * MAX_LINES)
        }
    }

    /// Fits the box to the text and lays out the subviews, bottom-up. The box grows upwards:
    /// its superview isn't flipped, so the origin stays where it is.
    fn layout(&self) {
        let p = self.ivars();
        // SAFETY: NSView messages on the main thread.
        unsafe {
            let mut b: Rect = msg_send![self, bounds];
            let text_w = (b.w - EDGE * 2.0).max(LINE_HEIGHT);
            let text_h = self.text_height(text_w);
            if (b.h - height(text_h)).abs() > 0.5 {
                b.h = height(text_h);
                let _: () = msg_send![self, setFrameSize: Size { w: b.w, h: b.h }];
            }
            let send = send_frame(b);
            let _: () = msg_send![&*p.send, setFrame: send];
            let fit: Size = msg_send![&*p.pill, intrinsicContentSize];
            let title_w = fit.w.min(send.x - EDGE - 8.0 - PILL_PAD * 2.0).max(0.0);
            let pill_y = EDGE + (BAR_H - PILL_H) / 2.0;
            let _: () = msg_send![&*p.pill, setFrame: Rect { x: EDGE + PILL_PAD, y: pill_y, w: title_w, h: PILL_H }];
            p.pill_frame.set(Rect { x: EDGE, y: pill_y, w: title_w + PILL_PAD * 2.0, h: PILL_H });
            let line_y = EDGE + BAR_H + PADDING;
            let _: () =
                msg_send![&*p.line, setFrame: Rect { x: INSET + 1.5, y: line_y, w: b.w - (INSET + 1.5) * 2.0, h: 1.0 }];
            let _: () =
                msg_send![&*p.scroll, setFrame: Rect { x: EDGE, y: line_y + 1.0 + PADDING, w: text_w, h: text_h }];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grows_with_the_text() {
        assert!(height(LINE_HEIGHT * 2.0) > height(LINE_HEIGHT));
        let b = Rect { x: 0.0, y: 0.0, w: 520.0, h: height(LINE_HEIGHT) };
        assert_eq!(send_frame(b).x + SEND + EDGE, b.w, "the send button sits in the right corner");
    }
}
