//! The launcher's few windows, through Objective-C messages to AppKit, which is loaded with
//! dlopen so that linking needs no SDK: alerts, the setup window and the progress panel.
//!
//! AppKit objects stay on the main thread. The controller runs on its own thread and calls
//! these functions, which hop to the main queue with `dispatch_sync_f`/`dispatch_async_f`.
//! Clicks come back as [`Event::Button`].
//!
//! For tests without clicking, `FAUST_TEST_ANSWERS="0|text:…|1"` answers the next dialogs
//! in order.

pub(crate) mod progress;
pub(crate) mod setup;

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::ffi::{CString, c_char, c_int, c_void};
use std::path::Path;
use std::ptr::null;
use std::sync::{Mutex, OnceLock};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, Bool, NSObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, class, define_class, msg_send, sel};

use crate::launcher::{Event, log, send};
use crate::objc::{Id, nsstring, rust_string};

/// Button tags, sent back as [`Event::Button`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Button {
    Install = 1,
    Quit = 2,
    Retry = 3,
    ShowLog = 4,
    Start = 5,
    Cancel = 10,
    /// Turning the setup window's first pages (handled here, never sent).
    PageNext = 20,
    PageBack = 21,
}

impl Button {
    fn from_tag(tag: isize) -> Option<Button> {
        [
            Button::Install,
            Button::Quit,
            Button::Retry,
            Button::ShowLog,
            Button::Start,
            Button::Cancel,
            Button::PageNext,
            Button::PageBack,
        ]
        .into_iter()
        .find(|b| *b as isize == tag)
    }
}

unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    static _dispatch_main_q: c_void;
    fn dispatch_sync_f(queue: *const c_void, context: *mut c_void, work: extern "C" fn(*mut c_void));
    fn dispatch_async_f(queue: *const c_void, context: *mut c_void, work: extern "C" fn(*mut c_void));
    fn pthread_main_np() -> c_int;
}

/// Loads AppKit into the process. Call once, before anything else here.
pub(crate) fn load_appkit() {
    let path = c"/System/Library/Frameworks/AppKit.framework/AppKit";
    // SAFETY: dlopen takes a NUL-terminated path; AppKit is part of every macOS.
    let handle = unsafe { dlopen(path.as_ptr(), 2) };
    assert!(!handle.is_null(), "AppKit could not be loaded");
}

fn main_queue() -> *const c_void {
    // The main dispatch queue is a static object libdispatch provides for the whole life of
    // the process; only its address is taken.
    &raw const _dispatch_main_q
}

/// Runs `f` on the main thread and waits for its result.
pub(crate) fn on_main<R: Send, F: FnOnce() -> R + Send>(f: F) -> R {
    // SAFETY: pthread_main_np has no preconditions.
    if unsafe { pthread_main_np() } != 0 {
        return f();
    }
    extern "C" fn trampoline<R, F: FnOnce() -> R>(ctx: *mut c_void) {
        // SAFETY: `ctx` is the `slot` below, alive until dispatch_sync_f returns.
        let slot = unsafe { &mut *ctx.cast::<(Option<F>, Option<R>)>() };
        slot.1 = Some((slot.0.take().expect("called once"))());
    }
    let mut slot: (Option<F>, Option<R>) = (Some(f), None);
    // SAFETY: dispatch_sync_f runs the trampoline on the main queue and returns after it.
    unsafe { dispatch_sync_f(main_queue(), (&raw mut slot).cast(), trampoline::<R, F>) };
    slot.1.expect("the main queue ran the closure")
}

/// Runs `f` on the main thread later.
pub(crate) fn on_main_async(f: impl FnOnce() + Send + 'static) {
    extern "C" fn trampoline(ctx: *mut c_void) {
        // SAFETY: `ctx` is the box leaked below, handed over exactly once.
        let f = unsafe { Box::from_raw(ctx.cast::<Box<dyn FnOnce() + Send>>()) };
        f();
    }
    let b: Box<Box<dyn FnOnce() + Send>> = Box::new(Box::new(f));
    // SAFETY: the trampoline takes ownership of the leaked box when the queue runs it.
    unsafe { dispatch_async_f(main_queue(), Box::into_raw(b).cast(), trampoline) };
}

fn mtm() -> MainThreadMarker {
    MainThreadMarker::new().expect("AppKit is used on the main thread only")
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct Rect {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) w: f64,
    pub(crate) h: f64,
}

// SAFETY: the layout matches CGRect: two CGPoint/CGSize structs of two doubles each.
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
#[derive(Clone, Copy, Debug)]
pub(crate) struct Size {
    pub(crate) w: f64,
    pub(crate) h: f64,
}

// SAFETY: the layout matches CGSize: two doubles.
unsafe impl objc2::Encode for Size {
    const ENCODING: objc2::Encoding = objc2::Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
}

pub(crate) fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect { x, y, w, h }
}

// ---------------------------------------------------------------------------
// The target for buttons, the setup window's delegate and the app's delegate.

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and this class adds no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FaustLauncherTarget"]
    struct Target;

    impl Target {
        #[unsafe(method(clicked:))]
        fn clicked(&self, sender: *mut AnyObject) {
            // SAFETY: only NSButtons made by `button` send this action.
            let tag: isize = unsafe { msg_send![sender, tag] };
            let Some(b) = Button::from_tag(tag) else { return };
            if !setup::turn_page(b) {
                send(Event::Button(b));
            }
        }

        /// The setup window's close button. It never closes by itself: the controller closes it.
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, _window: *mut AnyObject) -> Bool {
            setup::close_clicked();
            Bool::NO
        }

        /// The user opened the app again while it runs.
        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn reopen(&self, _app: *mut AnyObject, _visible: Bool) -> Bool {
            send(Event::Reopen);
            Bool::YES
        }
    }
);

thread_local! {
    static TARGET: RefCell<Option<Retained<Target>>> = const { RefCell::new(None) };
}

fn target() -> Retained<Target> {
    TARGET.with(|t| t.borrow().clone()).expect("ui::init runs first")
}

fn app() -> Id {
    // SAFETY: sharedApplication creates or returns the one NSApplication.
    unsafe { msg_send![class!(NSApplication), sharedApplication] }
}

static APP_NAME: OnceLock<String> = OnceLock::new();

/// The app's name, for titles.
pub(crate) fn app_name() -> &'static str {
    APP_NAME.get().map_or("App", String::as_str)
}

/// NSApplication without a Dock icon (the app the launcher starts has one), with an Edit
/// menu so copy and paste work in the alerts' text fields.
pub(crate) fn init(app_name: &str) {
    let _ = APP_NAME.set(app_name.to_owned());
    let mtm = mtm();
    // SAFETY: allocating and initializing our NSObject subclass on the main thread.
    let t: Retained<Target> = unsafe { msg_send![super(Target::alloc(mtm).set_ivars(())), init] };
    TARGET.with(|slot| *slot.borrow_mut() = Some(t.clone()));
    let quit = tr!(en: "Quit {app_name}", ja: "{app_name} を終了", de: "{app_name} beenden", it: "Esci da {app_name}");
    // SAFETY: documented NSApplication and NSMenu messages with matching argument types.
    unsafe {
        let app = app();
        let _: Bool = msg_send![&*app, setActivationPolicy: 1isize];
        let _: () = msg_send![&*app, setDelegate: &*t];
        let bar: Id = msg_send![class!(NSMenu), new];
        let app_item: Id = msg_send![class!(NSMenuItem), new];
        let _: () = msg_send![&*bar, addItem: &*app_item];
        let app_menu: Id = msg_send![class!(NSMenu), new];
        let _: () = msg_send![&*app_menu, addItem: &*menu_item(&quit, sel!(terminate:), "q")];
        let _: () = msg_send![&*app_item, setSubmenu: &*app_menu];
        let edit_item: Id = msg_send![class!(NSMenuItem), new];
        let _: () = msg_send![&*bar, addItem: &*edit_item];
        let edit: Allocated<AnyObject> = msg_send![class!(NSMenu), alloc];
        let edit: Id = msg_send![edit, initWithTitle: &*nsstring(&tr!(en: "Edit", ja: "編集", de: "Bearbeiten", it: "Composizione"))];
        for (title, action, key) in [
            (tr!(en: "Undo", ja: "取り消す", de: "Widerrufen", it: "Annulla"), sel!(undo:), "z"),
            (tr!(en: "Redo", ja: "やり直す", de: "Wiederholen", it: "Ripristina"), sel!(redo:), "Z"),
            (tr!(en: "Cut", ja: "カット", de: "Ausschneiden", it: "Taglia"), sel!(cut:), "x"),
            (tr!(en: "Copy", ja: "コピー", de: "Kopieren", it: "Copia"), sel!(copy:), "c"),
            (tr!(en: "Paste", ja: "ペースト", de: "Einsetzen", it: "Incolla"), sel!(paste:), "v"),
            (
                tr!(en: "Select All", ja: "すべてを選択", de: "Alles auswählen", it: "Seleziona tutto"),
                sel!(selectAll:),
                "a",
            ),
        ] {
            let _: () = msg_send![&*edit, addItem: &*menu_item(&title, action, key)];
        }
        let _: () = msg_send![&*edit_item, setSubmenu: &*edit];
        let _: () = msg_send![&*app, setMainMenu: &*bar];
    }
}

fn menu_item(title: &str, action: Sel, key: &str) -> Id {
    // SAFETY: initWithTitle:action:keyEquivalent: takes two NSStrings and a selector.
    unsafe {
        let item: Allocated<AnyObject> = msg_send![class!(NSMenuItem), alloc];
        msg_send![item, initWithTitle: &*nsstring(title), action: action, keyEquivalent: &*nsstring(key)]
    }
}

/// Runs the AppKit event loop; returns when the app terminates.
pub(crate) fn run() {
    // SAFETY: -[NSApplication run] on the main thread.
    unsafe { msg_send![&*app(), run] }
}

pub(crate) fn quit() {
    // SAFETY: -[NSApplication terminate:] on the main thread, with a nil sender.
    on_main_async(|| unsafe { msg_send![&*app(), terminate: null::<AnyObject>()] });
}

/// A Dock icon while the setup window is up; none otherwise.
pub(crate) fn set_regular(regular: bool) {
    on_main(move || {
        // SAFETY: setActivationPolicy: takes an NSApplicationActivationPolicy.
        let _: Bool = unsafe { msg_send![&*app(), setActivationPolicy: if regular { 0isize } else { 1isize }] };
    });
}

fn activate_self() {
    // SAFETY: activateIgnoringOtherApps: takes a BOOL.
    unsafe { msg_send![&*app(), activateIgnoringOtherApps: Bool::YES] }
}

/// Brings the app with this pid to the front.
pub(crate) fn activate_pid(pid: i32) {
    on_main_async(move || activate(pid));
}

/// [`activate_pid`], waiting until it is done.
pub(crate) fn activate_pid_now(pid: i32) {
    on_main(move || activate(pid));
}

fn activate(pid: i32) {
    // SAFETY: NSRunningApplication's documented messages; a missing app is nil.
    unsafe {
        let running: Option<Id> = msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier: pid];
        if let Some(running) = running {
            let _: Bool = msg_send![&*running, activateWithOptions: 3usize];
        }
    }
}

/// Is the Option key down? Checked once at launch: choose a version first.
pub(crate) fn option_key_down() -> bool {
    on_main(|| {
        // SAFETY: +[NSEvent modifierFlags] has no preconditions.
        let flags: usize = unsafe { msg_send![class!(NSEvent), modifierFlags] };
        flags & (1 << 19) != 0
    })
}

/// Moves a file or folder to the Trash (it can be put back from there).
pub(crate) fn trash(path: &Path) -> Result<(), String> {
    let path = path.to_owned();
    on_main(move || {
        // SAFETY: NSFileManager's trashItemAtURL:resultingItemURL:error: with valid out-pointers.
        unsafe {
            let url: Id = msg_send![class!(NSURL), fileURLWithPath: &*nsstring(&path.to_string_lossy())];
            let fm: *mut AnyObject = msg_send![class!(NSFileManager), defaultManager];
            let mut err: *mut AnyObject = std::ptr::null_mut();
            let ok: Bool = msg_send![fm, trashItemAtURL: &*url, resultingItemURL: std::ptr::null_mut::<*mut AnyObject>(), error: &mut err];
            if ok.as_bool() {
                Ok(())
            } else if err.is_null() {
                Err(
                    tr!(en: "Couldn't move it to the Trash", ja: "ゴミ箱に入れられません", de: "Konnte nicht in den Papierkorb gelegt werden", it: "Impossibile spostarlo nel Cestino"),
                )
            } else {
                Err(rust_string(msg_send![err, localizedDescription]))
            }
        }
    })
}

/// Opens a text file in TextEdit.
pub(crate) fn open_text(path: &Path) {
    let _ = std::process::Command::new("/usr/bin/open").arg("-e").arg(path).spawn();
}

// ---------------------------------------------------------------------------
// Building blocks

pub(crate) fn label(text: &str, size: f64, bold: bool, wrap: bool) -> Id {
    // SAFETY: NSTextField and NSFont class methods with NSString and CGFloat arguments.
    unsafe {
        let l: Id = if wrap {
            msg_send![class!(NSTextField), wrappingLabelWithString: &*nsstring(text)]
        } else {
            msg_send![class!(NSTextField), labelWithString: &*nsstring(text)]
        };
        let font: Id = if bold {
            msg_send![class!(NSFont), boldSystemFontOfSize: size]
        } else {
            msg_send![class!(NSFont), systemFontOfSize: size]
        };
        let _: () = msg_send![&*l, setFont: &*font];
        let _: () = msg_send![&*l, setSelectable: Bool::YES];
        l
    }
}

pub(crate) fn button(title: &str, tag: Button, frame: Rect, default: bool) -> Id {
    // SAFETY: buttonWithTitle:target:action: with our target object; setters take plain values.
    unsafe {
        let t = target();
        let b: Id =
            msg_send![class!(NSButton), buttonWithTitle: &*nsstring(title), target: &*t, action: sel!(clicked:)];
        let _: () = msg_send![&*b, setTag: tag as isize];
        let _: () = msg_send![&*b, setFrame: frame];
        if default {
            let _: () = msg_send![&*b, setKeyEquivalent: &*nsstring("\r")];
        }
        b
    }
}

/// A system color by its NSColor class method name, such as `secondaryLabelColor`.
pub(crate) fn color(name: &str) -> Id {
    let s = Sel::register(&CString::new(name).expect("color names have no NUL"));
    // SAFETY: only NSColor class methods without arguments are named here.
    unsafe { msg_send![class!(NSColor), performSelector: s] }
}

pub(crate) fn view(frame: Rect) -> Id {
    // SAFETY: -[NSView initWithFrame:] with a CGRect.
    unsafe {
        let v: Allocated<AnyObject> = msg_send![class!(NSView), alloc];
        msg_send![v, initWithFrame: frame]
    }
}

pub(crate) fn add(parent: &Id, child: &Id) {
    // SAFETY: addSubview: takes an NSView.
    let _: () = unsafe { msg_send![&**parent, addSubview: &**child] };
}

pub(crate) fn set_frame(v: &Id, frame: Rect) {
    // SAFETY: setFrame: takes a CGRect.
    let _: () = unsafe { msg_send![&**v, setFrame: frame] };
}

pub(crate) fn set_text(v: &Id, text: &str) {
    // SAFETY: setStringValue: takes an NSString.
    let _: () = unsafe { msg_send![&**v, setStringValue: &*nsstring(text)] };
}

/// A gray label for notes.
pub(crate) fn note(text: &str) -> Id {
    let l = label(text, 12.0, false, true);
    // SAFETY: setTextColor: takes an NSColor.
    let _: () = unsafe { msg_send![&*l, setTextColor: &*color("secondaryLabelColor")] };
    l
}

/// Places `view` at `x`, at the top of the remaining space (y goes down from `y`).
pub(crate) fn stack(content: &Id, view: &Id, x: f64, y: &mut f64, width: f64, gap: f64) {
    // SAFETY: NSTextField layout messages with CGFloat and CGSize arguments.
    unsafe {
        let _: () = msg_send![&**view, setPreferredMaxLayoutWidth: width];
        let s: Size = msg_send![&**view, sizeThatFits: Size { w: width, h: 10_000.0 }];
        *y -= s.h.ceil();
        let _: () = msg_send![&**view, setFrame: rect(x, *y, width, s.h.ceil())];
    }
    add(content, view);
    *y -= gap;
}

// ---------------------------------------------------------------------------
// Alerts: modal, they wait for the answer

fn new_alert(title: &str, text: &str, buttons: &[String], critical: bool) -> Id {
    // SAFETY: NSAlert's documented setters with NSString and NSAlertStyle arguments.
    unsafe {
        let a: Id = msg_send![class!(NSAlert), new];
        let _: () = msg_send![&*a, setMessageText: &*nsstring(title)];
        let _: () = msg_send![&*a, setInformativeText: &*nsstring(text)];
        let _: () = msg_send![&*a, setAlertStyle: if critical { 2usize } else { 1usize }];
        for b in buttons {
            let _: Id = msg_send![&*a, addButtonWithTitle: &*nsstring(b)];
        }
        a
    }
}

/// The next answer from `FAUST_TEST_ANSWERS`, if any.
pub(crate) fn test_answer() -> Option<String> {
    static ANSWERS: Mutex<Option<VecDeque<String>>> = Mutex::new(None);
    let mut answers = ANSWERS.lock().expect("test answers poisoned");
    let queue = answers.get_or_insert_with(|| {
        crate::launcher::overrides::get("FAUST_TEST_ANSWERS")
            .unwrap_or_default()
            .split('|')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    });
    let answer = queue.pop_front();
    if let Some(a) = &answer {
        log(&format!("(test answer: {a})"));
    }
    answer
}

fn test_button() -> Option<usize> {
    test_answer().and_then(|a| a.parse().ok())
}

/// NSAlertFirstButtonReturn.
const FIRST_BUTTON: isize = 1000;

/// What an alert returns when it was closed without a button being pressed. It matches no
/// button, so every caller takes it as "not this", like Cancel.
pub(crate) const DISMISSED: usize = usize::MAX;

/// Runs the alert and returns the button pressed, or [`DISMISSED`].
fn run_alert(a: &Id, title: &str, buttons: usize) -> usize {
    if let Some(b) = test_button() {
        log(&format!("dialog \"{title}\" -> {b}"));
        return b;
    }
    activate_self();
    // SAFETY: runModal on the main thread.
    let r: isize = unsafe { msg_send![&**a, runModal] };
    usize::try_from(r - FIRST_BUTTON).ok().filter(|b| *b < buttons).unwrap_or(DISMISSED)
}

fn owned(buttons: &[&str]) -> Vec<String> {
    buttons.iter().map(|s| (*s).to_owned()).collect()
}

/// Asks with `buttons` (the first is the default) and returns which one was pressed.
pub(crate) fn alert(title: &str, text: &str, buttons: &[&str]) -> usize {
    let (title, text, buttons) = (title.to_owned(), text.to_owned(), owned(buttons));
    on_main(move || run_alert(&new_alert(&title, &text, &buttons, false), &title, buttons.len()))
}

/// Like [`alert`], with the warning style.
pub(crate) fn alert_critical(title: &str, text: &str, buttons: &[&str]) -> usize {
    let (title, text, buttons) = (title.to_owned(), text.to_owned(), owned(buttons));
    on_main(move || run_alert(&new_alert(&title, &text, &buttons, true), &title, buttons.len()))
}

// ---- Confirmations a trusted device may answer too (launcher::devices)

/// NSModalResponseAbort: the dialog was closed by abortModal.
const MODAL_ABORT: isize = -1001;

#[derive(Default)]
struct Remote {
    /// The confirmation whose dialog is up.
    active: Option<String>,
    /// Verified answers from devices, by confirmation id.
    answers: HashMap<String, String>,
}

static REMOTE: Mutex<Option<Remote>> = Mutex::new(None);

fn with_remote<R>(f: impl FnOnce(&mut Remote) -> R) -> R {
    let mut guard = REMOTE.lock().expect("remote answers poisoned");
    f(guard.get_or_insert_with(Remote::default))
}

/// A device answered confirmation `id` (its signature was checked). The dialog closes if it
/// is up; otherwise the answer waits for [`alert_remote`].
pub(crate) fn remote_answer(id: &str, choice: &str) {
    let close = with_remote(|r| {
        r.answers.insert(id.to_owned(), choice.to_owned());
        r.active.as_deref() == Some(id)
    });
    if close {
        let id = id.to_owned();
        on_main_async(move || {
            // The user may have answered on the Mac meanwhile, and another dialog be up now.
            if with_remote(|r| r.active.as_deref() != Some(id.as_str())) {
                return;
            }
            // SAFETY: modalWindow and abortModal on the main thread.
            unsafe {
                let app = app();
                let modal: *mut AnyObject = msg_send![&*app, modalWindow];
                if !modal.is_null() {
                    let _: () = msg_send![&*app, abortModal];
                }
            }
        });
    }
}

/// Like [`alert`], for confirmation `id` that a trusted device may answer too: `choices[i]`
/// is what a device sends for button i (`None`: that button exists on the Mac only).
/// Whichever answer comes first counts.
pub(crate) fn alert_remote(id: &str, title: &str, text: &str, buttons: &[&str], choices: &[Option<&str>]) -> usize {
    let (id, title, text, buttons) = (id.to_owned(), title.to_owned(), text.to_owned(), owned(buttons));
    let choices: Vec<Option<String>> = choices.iter().map(|c| c.map(str::to_owned)).collect();
    on_main(move || {
        let from_device = |r: &mut Remote| {
            let c = r.answers.remove(&id)?;
            choices.iter().position(|x| x.as_deref() == Some(c.as_str()))
        };
        // Answered on a device before the dialog came up.
        let early = with_remote(|r| {
            let i = from_device(r);
            if i.is_none() {
                r.active = Some(id.clone());
            }
            i
        });
        if let Some(i) = early {
            return i;
        }
        let a = new_alert(&title, &text, &buttons, false);
        let r: isize = match test_button() {
            Some(t) => {
                log(&format!("dialog \"{title}\" -> {t}"));
                isize::try_from(t).unwrap_or(0) + FIRST_BUTTON
            }
            None => {
                activate_self();
                // SAFETY: runModal on the main thread.
                unsafe { msg_send![&*a, runModal] }
            }
        };
        let device = with_remote(|rem| {
            rem.active = None;
            from_device(rem)
        });
        match (r, device) {
            (MODAL_ABORT, Some(i)) => i,
            // Closed without an answer we know.
            (MODAL_ABORT, None) => DISMISSED,
            _ => usize::try_from(r - FIRST_BUTTON).ok().filter(|b| *b < buttons.len()).unwrap_or(DISMISSED),
        }
    })
}

/// Asks for a few lines of text, starting from `initial`. `None` when the second button
/// (cancel) was pressed.
pub(crate) fn ask_text(title: &str, text: &str, initial: &str, ok: &str, cancel: &str) -> Option<String> {
    let (title, text, buttons) = (title.to_owned(), text.to_owned(), vec![ok.to_owned(), cancel.to_owned()]);
    let initial = initial.to_owned();
    on_main(move || {
        let a = new_alert(&title, &text, &buttons, false);
        // SAFETY: an NSScrollView holding an NSTextView, set as the alert's accessory view.
        let tv: Id = unsafe {
            let sv: Allocated<AnyObject> = msg_send![class!(NSScrollView), alloc];
            let sv: Id = msg_send![sv, initWithFrame: rect(0.0, 0.0, 440.0, 150.0)];
            let _: () = msg_send![&*sv, setHasVerticalScroller: Bool::YES];
            let _: () = msg_send![&*sv, setBorderType: 2usize];
            let tv: Allocated<AnyObject> = msg_send![class!(NSTextView), alloc];
            let tv: Id = msg_send![tv, initWithFrame: rect(0.0, 0.0, 440.0, 150.0)];
            let _: () = msg_send![&*tv, setRichText: Bool::NO];
            let _: () = msg_send![&*tv, setAutoresizingMask: 2usize];
            let font: Id = msg_send![class!(NSFont), systemFontOfSize: 13.0f64];
            let _: () = msg_send![&*tv, setFont: &*font];
            let _: () = msg_send![&*tv, setString: &*nsstring(&initial)];
            let _: () = msg_send![&*sv, setDocumentView: &*tv];
            let _: () = msg_send![&*a, setAccessoryView: &*sv];
            let _: () = msg_send![&*a, layout];
            let w: Id = msg_send![&*a, window];
            let _: () = msg_send![&*w, setInitialFirstResponder: &*tv];
            tv
        };
        if let Some(t) = test_answer() {
            return t.strip_prefix("text:").map(str::to_owned);
        }
        if run_alert(&a, &title, buttons.len()) != 0 {
            return None;
        }
        // SAFETY: -[NSTextView string] returns the text view's NSString.
        let s: *mut AnyObject = unsafe { msg_send![&*tv, string] };
        Some(rust_string(s).trim().to_owned())
    })
}

/// Lets the user pick one of `items`. Returns (button, item).
pub(crate) fn pick(title: &str, text: &str, items: &[String], buttons: &[&str]) -> (usize, usize) {
    let (title, text, items, buttons) = (title.to_owned(), text.to_owned(), items.to_vec(), owned(buttons));
    on_main(move || {
        let a = new_alert(&title, &text, &buttons, false);
        // SAFETY: an NSPopUpButton filled with NSStrings, set as the alert's accessory view.
        let popup: Id = unsafe {
            let p: Allocated<AnyObject> = msg_send![class!(NSPopUpButton), alloc];
            let p: Id = msg_send![p, initWithFrame: rect(0.0, 0.0, 440.0, 28.0), pullsDown: Bool::NO];
            for it in &items {
                let _: () = msg_send![&*p, addItemWithTitle: &*nsstring(it)];
            }
            let _: () = msg_send![&*a, setAccessoryView: &*p];
            p
        };
        // A test answer is "button:item".
        if let Some((b, i)) = test_answer().and_then(|t| {
            let (b, i) = t.split_once(':')?;
            Some((b.parse().ok()?, i.parse().ok()?))
        }) {
            return (b, i);
        }
        let b = run_alert(&a, &title, buttons.len());
        // SAFETY: indexOfSelectedItem returns an NSInteger.
        let i: isize = unsafe { msg_send![&*popup, indexOfSelectedItem] };
        (b, usize::try_from(i).unwrap_or(0))
    })
}

/// "Show Log", on the setup window and in alerts.
pub(crate) fn show_log_title() -> String {
    tr!(en: "Show Log", ja: "ログを表示", de: "Protokoll anzeigen", it: "Mostra registro")
}
