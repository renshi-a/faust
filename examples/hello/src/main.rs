//! Hello: a small macOS app that its users can change by asking.
//!
//! It opens one window with a ball of water that ripples as you type (`water.rs`), a field at
//! the bottom for asking for a change, and "Choose a Version…" in its app menu. Those talk to
//! Faust through `faust_link`, which also tells Faust when the window is up and hears when to
//! quit. AppKit is loaded at run time and reached
//! through Objective-C messages, so the build needs no SDK and no Xcode.

mod composer;
mod water;

use std::cell::OnceCell;
use std::ptr::null;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, Bool, NSObject, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, class, define_class, msg_send, sel};

type Id = Retained<AnyObject>;

/// The water's colour, as sRGB red, green and blue. Changing it is a good first request.
const WATER: (f64, f64, f64) = (0.20, 0.55, 0.95);

fn main() {
    // `--selftest`: Faust runs this before it uses a new version. Exit 0 means good.
    if std::env::args().any(|a| a == "--selftest") {
        if let Err(e) = water::selftest() {
            eprintln!("selftest: {e}");
            std::process::exit(1);
        }
        println!("selftest: ok");
        return;
    }
    load_appkit();
    let mtm = MainThreadMarker::new().expect("main runs on the main thread");
    let target: Retained<Target> = {
        let this = Target::alloc(mtm).set_ivars(OnceCell::new());
        // SAFETY: -[NSObject init] on a freshly allocated object.
        unsafe { msg_send![super(this), init] }
    };
    // SAFETY: AppKit's documented messages, sent on the main thread with matching types.
    unsafe {
        let app: Id = msg_send![class!(NSApplication), sharedApplication];
        let _: Bool = msg_send![&*app, setActivationPolicy: 0isize]; // regular: a Dock icon
        let _: () = msg_send![&*app, setMainMenu: &*menu_bar(&target)];
        let window = window(mtm, &target);
        let _: () = msg_send![&*window, makeKeyAndOrderFront: null::<AnyObject>()];
        let _: () = msg_send![&*app, activateIgnoringOtherApps: Bool::YES];
        if let Some(link) = faust_link::get() {
            link.ready();
        }
        // Check for messages from Faust twice a second.
        let _: () =
            msg_send![&*target, performSelector: sel!(tick:), withObject: null::<AnyObject>(), afterDelay: 0.5f64];
        std::mem::forget(window);
        let _: () = msg_send![&*app, run];
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and this class adds no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "AppTarget"]
    #[ivars = OnceCell<Retained<composer::Composer>>] // the field, to show who makes changes
    struct Target;

    impl Target {
        #[unsafe(method(chooseVersion:))]
        fn choose_version(&self, _sender: *mut AnyObject) {
            if let Some(link) = faust_link::get() {
                link.choose_version();
            }
        }

        #[unsafe(method(tick:))]
        fn tick(&self, _arg: *mut AnyObject) {
            let mut quit = false;
            for event in faust_link::get().map(faust_link::Link::events).unwrap_or_default() {
                match event {
                    faust_link::Event::Quit => quit = true,
                    faust_link::Event::Assistant(name) => {
                        if let Some(field) = self.ivars().get() {
                            field.set_assistant(&name);
                        }
                    }
                    _ => {}
                }
            }
            // SAFETY: NSApplication and NSObject messages on the main thread.
            unsafe {
                if quit {
                    let app: Id = msg_send![class!(NSApplication), sharedApplication];
                    let _: () = msg_send![&*app, terminate: null::<AnyObject>()];
                } else {
                    let _: () = msg_send![self, performSelector: sel!(tick:), withObject: null::<AnyObject>(), afterDelay: 0.5f64];
                }
            }
        }
    }
);

/// AppKit is part of every Mac; loading it at run time keeps it off the link line.
fn load_appkit() {
    unsafe extern "C" {
        fn dlopen(path: *const std::ffi::c_char, mode: i32) -> *mut std::ffi::c_void;
    }
    // SAFETY: dlopen takes a NUL-terminated path.
    let handle = unsafe { dlopen(c"/System/Library/Frameworks/AppKit.framework/AppKit".as_ptr(), 2) };
    assert!(!handle.is_null(), "AppKit could not be loaded");
}

fn nsstring(s: &str) -> Id {
    let c = std::ffi::CString::new(s).expect("no NUL in UI text");
    // SAFETY: stringWithUTF8String: copies the NUL-terminated UTF-8 bytes.
    unsafe { msg_send![class!(NSString), stringWithUTF8String: c.as_ptr()] }
}

fn menu_item(title: &str, action: Sel, key: &str, target: Option<&Target>) -> Id {
    // SAFETY: NSMenuItem's designated initializer and setTarget:, on the main thread.
    unsafe {
        let item: Allocated<AnyObject> = msg_send![class!(NSMenuItem), alloc];
        let item: Id =
            msg_send![item, initWithTitle: &*nsstring(title), action: action, keyEquivalent: &*nsstring(key)];
        if let Some(t) = target {
            let _: () = msg_send![&*item, setTarget: t];
        }
        item
    }
}

fn menu_bar(target: &Target) -> Id {
    // SAFETY: NSMenu and NSMenuItem messages on the main thread.
    unsafe {
        let bar: Id = msg_send![class!(NSMenu), new];
        let app_item: Id = msg_send![class!(NSMenuItem), new];
        let _: () = msg_send![&*bar, addItem: &*app_item];
        let menu: Id = msg_send![class!(NSMenu), new];
        if faust_link::get().is_some() {
            let _: () =
                msg_send![&*menu, addItem: &*menu_item("Choose a Version…", sel!(chooseVersion:), "", Some(target))];
            let separator: Id = msg_send![class!(NSMenuItem), separatorItem];
            let _: () = msg_send![&*menu, addItem: &*separator];
        }
        let _: () = msg_send![&*menu, addItem: &*menu_item("Quit Hello", sel!(terminate:), "q", None)];
        let _: () = msg_send![&*app_item, setSubmenu: &*menu];
        bar
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

// SAFETY: the layout matches CGRect (two pairs of doubles).
unsafe impl objc2::Encode for Rect {
    const ENCODING: objc2::Encoding = objc2::Encoding::Struct(
        "CGRect",
        &[
            objc2::Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]),
            objc2::Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]),
        ],
    );
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Point {
    x: f64,
    y: f64,
}

// SAFETY: the layout matches CGPoint (two doubles).
unsafe impl objc2::Encode for Point {
    const ENCODING: objc2::Encoding = objc2::Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Size {
    w: f64,
    h: f64,
}

// SAFETY: the layout matches CGSize (two doubles).
unsafe impl objc2::Encode for Size {
    const ENCODING: objc2::Encoding = objc2::Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
}

fn window(mtm: MainThreadMarker, target: &Target) -> Id {
    // SAFETY: NSWindow and NSView messages on the main thread.
    unsafe {
        let (width, height) = (560.0, 440.0);
        let w: Allocated<AnyObject> = msg_send![class!(NSWindow), alloc];
        // Titled, closable, miniaturizable, resizable.
        let w: Id = msg_send![w, initWithContentRect: Rect { x: 0.0, y: 0.0, w: width, h: height }, styleMask: 15usize, backing: 2usize, defer: Bool::NO];
        let _: () = msg_send![&*w, setTitle: &*nsstring("Hello")];
        let _: () = msg_send![&*w, setReleasedWhenClosed: Bool::NO];
        let content: Id = msg_send![&*w, contentView];
        // The water fills what the field leaves above it.
        let mut bottom = 0.0;
        let field = faust_link::get().map(|_| composer::Composer::new(mtm, width - 40.0, "Ask Hello for a change"));
        if let Some(f) = &field {
            let r: Rect = msg_send![&**f, frame];
            bottom = 20.0 + r.h;
        }
        let water = water::WaterView::new(mtm, Rect { x: 0.0, y: bottom, w: width, h: height - bottom });
        let _: () = msg_send![&*water, setAutoresizingMask: 18usize]; // grows with the window
        let _: () = msg_send![&*content, addSubview: &*water];
        if let Some(field) = field {
            let _: () = msg_send![&*field, setFrameOrigin: Point { x: 20.0, y: 20.0 }];
            let _: () = msg_send![&*field, setAutoresizingMask: 34usize]; // stays at the bottom, as wide as the window
            let _: () = msg_send![&*content, addSubview: &*field];
            field.focus();
            let _ = target.ivars().set(field);
        } else {
            let _: Bool = msg_send![&*w, makeFirstResponder: &*water];
        }
        let _: () = msg_send![&*w, center];
        w
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn water_is_a_colour() {
        let (r, g, b) = WATER;
        assert!([r, g, b].iter().all(|c| (0.0..=1.0).contains(c)));
    }
}
