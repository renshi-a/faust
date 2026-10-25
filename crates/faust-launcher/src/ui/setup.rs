//! The setup window, laid out like the macOS installer: the steps on the left, a page on the
//! right, Back and Continue at the bottom. The first two pages (welcome, what will be
//! installed) are turned here; the controller sees Install, Retry, Start and Quit.

use std::cell::RefCell;
use std::ptr::null;

use objc2::rc::Allocated;
use objc2::runtime::{AnyObject, Bool};
use objc2::{class, msg_send};

use super::{
    Button, Rect, activate_self, add, app, button, color, label, note, on_main, on_main_async, rect, set_frame,
    set_text, show_log_title, stack, view,
};
use crate::launcher::{Event, log, send};
use crate::objc::{Id, nsstring};

/// What the pages say.
#[derive(Debug)]
pub(crate) struct Text {
    /// The welcome page.
    pub(crate) welcome: String,
    /// What will be installed, and where.
    pub(crate) items: String,
    pub(crate) notes: String,
    /// Whether the assistant was found.
    pub(crate) assistant: String,
    /// The last page, after the installation.
    pub(crate) done: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// The welcome page; the Install button is on the next one.
    Consent,
    Running,
    /// Retry, and the log.
    Failed,
    /// Installed: the Start button.
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Page {
    Welcome,
    Items,
    Install,
    Done,
}

const PAGES: [Page; 4] = [Page::Welcome, Page::Items, Page::Install, Page::Done];

fn step_names() -> [String; 4] {
    [
        tr!(en: "Introduction", ja: "はじめに", de: "Einführung", it: "Introduzione"),
        tr!(en: "Components", ja: "インストールするもの", de: "Komponenten", it: "Componenti"),
        tr!(en: "Installation", ja: "インストール", de: "Installation", it: "Installazione"),
        tr!(en: "Summary", ja: "概要", de: "Zusammenfassung", it: "Riepilogo"),
    ]
}

fn continue_title() -> String {
    tr!(en: "Continue", ja: "続ける", de: "Fortfahren", it: "Continua")
}

struct Window {
    window: Id,
    heading: Id,
    /// (dot, label) per step.
    steps: Vec<(Id, Id)>,
    pages: Vec<Id>,
    back: Id,
    next: Id,
    log: Id,
    bar: Id,
    step: Id,
    line: Id,
    page: Page,
    failed: bool,
}

thread_local! {
    static SETUP: RefCell<Option<Window>> = const { RefCell::new(None) };
}

// Window and page geometry.
const WIN_W: f64 = 720.0;
const WIN_H: f64 = 500.0;
const SIDE_W: f64 = 200.0;
const PAGE_X: f64 = SIDE_W + 8.0;
const PAGE_W: f64 = WIN_W - PAGE_X - 24.0;
const PAGE_Y: f64 = 64.0;
const PAGE_H: f64 = WIN_H - PAGE_Y - 64.0;
/// Padding inside a page.
const PAD: f64 = 22.0;
const TEXT_W: f64 = PAGE_W - PAD * 2.0;

/// Shows the window on its welcome page.
pub(crate) fn show(t: Text) {
    on_main(move || {
        let w = build(&t);
        show_page(&w);
        if let Some(path) = crate::launcher::overrides::get("FAUST_TEST_SNAPSHOT") {
            snapshot(w, &path);
        }
        SETUP.with(|s| *s.borrow_mut() = Some(w));
    });
}

#[expect(clippy::too_many_lines, reason = "one window, laid out top to bottom")]
fn build(t: &Text) -> Window {
    let name = super::app_name();
    let title =
        tr!(en: "Install {name}", ja: "{name} のインストール", de: "{name} installieren", it: "Installa {name}");
    // SAFETY: AppKit's documented messages on the main thread, with matching argument types.
    unsafe {
        let win: Allocated<AnyObject> = msg_send![class!(NSWindow), alloc];
        // Titled, closable, miniaturizable; closing asks the delegate (windowShouldClose:).
        let win: Id = msg_send![win, initWithContentRect: rect(0.0, 0.0, WIN_W, WIN_H), styleMask: 1usize | 2 | 4, backing: 2usize, defer: Bool::NO];
        let _: () = msg_send![&*win, setReleasedWhenClosed: Bool::NO];
        let _: () = msg_send![&*win, setTitle: &*nsstring(&title)];
        let _: () = msg_send![&*win, setDelegate: &*super::target()];
        let content: Id = msg_send![&*win, contentView];

        // The sidebar: the steps, and the app's icon at the bottom.
        let mut steps = Vec::new();
        for (i, s) in step_names().iter().enumerate() {
            let y = WIN_H - 112.0 - i as f64 * 34.0;
            let dot = label("●", 11.0, false, false);
            let name = label(s, 14.0, false, false);
            let _: () = msg_send![&*name, setSelectable: Bool::NO];
            let _: () = msg_send![&*dot, setSelectable: Bool::NO];
            set_frame(&dot, rect(32.0, y + 1.0, 16.0, 18.0));
            set_frame(&name, rect(52.0, y, SIDE_W - 56.0, 20.0));
            add(&content, &dot);
            add(&content, &name);
            steps.push((dot, name));
        }
        let icon: *mut AnyObject = msg_send![&*app(), applicationIconImage];
        if !icon.is_null() {
            let iv: Id = msg_send![class!(NSImageView), imageViewWithImage: icon];
            set_frame(&iv, rect((SIDE_W - 112.0) / 2.0, 52.0, 112.0, 112.0));
            add(&content, &iv);
        }

        let heading = label("", 15.0, true, false);
        let _: () = msg_send![&*heading, setSelectable: Bool::NO];
        set_frame(&heading, rect(PAGE_X, PAGE_Y + PAGE_H + 14.0, PAGE_W, 22.0));
        add(&content, &heading);

        // The white, framed page area.
        let b: Allocated<AnyObject> = msg_send![class!(NSBox), alloc];
        let frame: Id = msg_send![b, initWithFrame: rect(PAGE_X, PAGE_Y, PAGE_W, PAGE_H)];
        let _: () = msg_send![&*frame, setBoxType: 4usize]; // NSBoxCustom
        let _: () = msg_send![&*frame, setTitlePosition: 0usize]; // NSNoTitle
        let _: () = msg_send![&*frame, setFillColor: &*color("textBackgroundColor")];
        let _: () = msg_send![&*frame, setBorderColor: &*color("separatorColor")];
        let _: () = msg_send![&*frame, setBorderWidth: 1.0f64];
        let _: () = msg_send![&*frame, setCornerRadius: 4.0f64];
        let _: () = msg_send![&*frame, setContentViewMargins: super::Size { w: 0.0, h: 0.0 }];
        add(&content, &frame);
        let inner: Id = msg_send![&*frame, contentView];
        let page_rect = rect(0.0, 0.0, PAGE_W, PAGE_H);
        let top = PAGE_H - PAD;

        let welcome = view(page_rect);
        let mut y = top;
        stack(&welcome, &label(&t.welcome, 14.0, false, true), PAD, &mut y, TEXT_W, 0.0);

        let items = view(page_rect);
        let mut y = top;
        stack(&items, &label(&t.items, 13.0, false, true), PAD, &mut y, TEXT_W, 16.0);
        stack(&items, &note(&t.notes), PAD, &mut y, TEXT_W, 12.0);
        stack(&items, &note(&t.assistant), PAD, &mut y, TEXT_W, 0.0);

        let install = view(page_rect);
        let step = label("", 13.0, true, false);
        set_frame(&step, rect(PAD, top - 40.0, TEXT_W, 18.0));
        let _: () = msg_send![&*step, setLineBreakMode: 4usize]; // truncate the tail
        add(&install, &step);
        let bar: Allocated<AnyObject> = msg_send![class!(NSProgressIndicator), alloc];
        let bar: Id = msg_send![bar, initWithFrame: rect(PAD, top - 68.0, TEXT_W, 16.0)];
        let _: () = msg_send![&*bar, setIndeterminate: Bool::NO];
        let _: () = msg_send![&*bar, setMinValue: 0.0f64];
        let _: () = msg_send![&*bar, setMaxValue: 1.0f64];
        add(&install, &bar);
        let line = label("", 11.0, false, false);
        let mono: Id = msg_send![class!(NSFont), monospacedSystemFontOfSize: 11.0f64, weight: 0.0f64];
        let _: () = msg_send![&*line, setFont: &*mono];
        let _: () = msg_send![&*line, setTextColor: &*color("secondaryLabelColor")];
        let _: () = msg_send![&*line, setLineBreakMode: 4usize];
        set_frame(&line, rect(PAD, top - 92.0, TEXT_W, 16.0));
        add(&install, &line);
        let mut y = PAD + 40.0;
        let wait = tr!(
            en: "This takes a few minutes. Keep this window open until it's done.",
            ja: "数分かかります。終わるまでこのウインドウは開いたままにしてください。",
            de: "Das dauert ein paar Minuten. Lass dieses Fenster offen, bis alles fertig ist.",
            it: "Ci vorranno alcuni minuti. Tieni aperta questa finestra fino alla fine.",
        );
        stack(&install, &note(&wait), PAD, &mut y, TEXT_W, 0.0);

        let done = view(page_rect);
        let check: Option<Id> = msg_send![class!(NSImage), imageWithSystemSymbolName: &*nsstring("checkmark.circle.fill"), accessibilityDescription: &*nsstring(&step_names()[3])];
        if let Some(check) = check {
            let cfg: Id =
                msg_send![class!(NSImageSymbolConfiguration), configurationWithPointSize: 56.0f64, weight: 0.0f64];
            let check: Id = msg_send![&*check, imageWithSymbolConfiguration: &*cfg];
            let iv: Id = msg_send![class!(NSImageView), imageViewWithImage: &*check];
            let _: () = msg_send![&*iv, setContentTintColor: &*color("systemGreenColor")];
            set_frame(&iv, rect((PAGE_W - 72.0) / 2.0, top - 84.0, 72.0, 72.0));
            add(&done, &iv);
        }
        let mut y = top - 104.0;
        let finished = tr!(en: "The installation is complete.", ja: "インストールが完了しました。", de: "Die Installation ist abgeschlossen.", it: "L'installazione è stata completata.");
        let msg = label(&finished, 16.0, true, true);
        let _: () = msg_send![&*msg, setAlignment: 1isize]; // centered
        stack(&done, &msg, PAD, &mut y, TEXT_W, 12.0);
        let sub = label(&t.done, 13.0, false, true);
        let _: () = msg_send![&*sub, setAlignment: 1isize];
        stack(&done, &sub, PAD, &mut y, TEXT_W, 0.0);

        let pages = vec![welcome, items, install, done];
        for p in &pages {
            add(&inner, p);
        }
        let next = button(&continue_title(), Button::PageNext, rect(WIN_W - 24.0 - 140.0, 16.0, 140.0, 32.0), true);
        let back_title = tr!(en: "Go Back", ja: "戻る", de: "Zurück", it: "Indietro");
        let back =
            button(&back_title, Button::PageBack, rect(WIN_W - 24.0 - 140.0 - 10.0 - 110.0, 16.0, 110.0, 32.0), false);
        let log = button(&show_log_title(), Button::ShowLog, rect(PAGE_X - 6.0, 16.0, 170.0, 32.0), false);
        for b in [&next, &back, &log] {
            add(&content, b);
        }
        let _: () = msg_send![&*win, center];
        let _: () = msg_send![&*win, makeKeyAndOrderFront: null::<AnyObject>()];
        activate_self();
        Window {
            window: win,
            heading,
            steps,
            pages,
            back,
            next,
            log,
            bar,
            step,
            line,
            page: Page::Welcome,
            failed: false,
        }
    }
}

/// `FAUST_TEST_SNAPSHOT=out.png` (with `FAUST_TEST_SNAPSHOT_PAGE=0..3` or `failed`): writes the
/// window's content as an image and exits, for checking the layout.
fn snapshot(mut w: Window, path: &str) -> ! {
    match crate::launcher::overrides::get("FAUST_TEST_SNAPSHOT_PAGE").as_deref() {
        Some("failed") => (w.page, w.failed) = (Page::Install, true),
        Some(n) => w.page = n.parse().ok().and_then(|i: usize| PAGES.get(i).copied()).unwrap_or(Page::Welcome),
        None => {}
    }
    if w.page == Page::Install {
        // SAFETY: setDoubleValue: takes a double.
        let _: () = unsafe { msg_send![&*w.bar, setDoubleValue: 0.42f64] };
        set_text(
            &w.step,
            &tr!(en: "Installing Rust 1.96.1", ja: "Rust 1.96.1 をインストールしています", de: "Rust 1.96.1 wird installiert", it: "Installazione di Rust 1.96.1"),
        );
        set_text(&w.line, "info: installing component 'rust-std'");
    }
    show_page(&w);
    // SAFETY: caching the content view into a bitmap and writing it as PNG, all documented AppKit.
    let ok: Bool = unsafe {
        let content: Id = msg_send![&*w.window, contentView];
        let _: () = msg_send![&*content, layoutSubtreeIfNeeded];
        let bounds: Rect = msg_send![&*content, bounds];
        let rep: Id = msg_send![&*content, bitmapImageRepForCachingDisplayInRect: bounds];
        let _: () = msg_send![&*content, cacheDisplayInRect: bounds, toBitmapImageRep: &*rep];
        let props: Id = msg_send![class!(NSDictionary), dictionary];
        let png: Id = msg_send![&*rep, representationUsingType: 4usize, properties: &*props];
        msg_send![&*png, writeToFile: &*nsstring(path), atomically: Bool::YES]
    };
    log(&format!("snapshot {path}: {}", ok.as_bool()));
    std::process::exit(if ok.as_bool() { 0 } else { 1 });
}

/// Shows `w.page`: the heading, the sidebar, and what the buttons do there.
fn show_page(w: &Window) {
    let name = super::app_name();
    let (heading, next, next_tag, back) = match (w.page, w.failed) {
        (Page::Welcome, _) => (
            tr!(en: "Welcome to the {name} Installer", ja: "{name} インストーラへようこそ", de: "Willkommen beim {name}-Installer", it: "Benvenuto nel programma di installazione di {name}"),
            continue_title(),
            Some(Button::PageNext),
            false,
        ),
        (Page::Items, _) => (
            tr!(en: "What will be installed on this Mac", ja: "この Mac にインストールするもの", de: "Was auf diesem Mac installiert wird", it: "Cosa verrà installato su questo Mac"),
            tr!(en: "Install", ja: "インストール", de: "Installieren", it: "Installa"),
            Some(Button::Install),
            true,
        ),
        (Page::Install, false) => (
            tr!(en: "Installing", ja: "インストールしています", de: "Installation läuft", it: "Installazione in corso"),
            continue_title(),
            None,
            false,
        ),
        (Page::Install, true) => (
            tr!(en: "The installation failed", ja: "インストールできませんでした", de: "Die Installation ist fehlgeschlagen", it: "L'installazione non è riuscita"),
            tr!(en: "Try Again", ja: "もう一度試す", de: "Erneut versuchen", it: "Riprova"),
            Some(Button::Retry),
            false,
        ),
        (Page::Done, _) => (
            tr!(en: "The installation is complete", ja: "インストールが完了しました", de: "Die Installation ist abgeschlossen", it: "L'installazione è stata completata"),
            tr!(en: "Start", ja: "開始", de: "Starten", it: "Avvia"),
            Some(Button::Start),
            false,
        ),
    };
    // SAFETY: AppKit setters on the main thread with matching argument types.
    unsafe {
        set_text(&w.heading, &heading);
        for (i, (dot, name)) in w.steps.iter().enumerate() {
            let (dot_color, name_color) = match PAGES[i].cmp(&w.page) {
                std::cmp::Ordering::Less => ("tertiaryLabelColor", "secondaryLabelColor"),
                std::cmp::Ordering::Equal => ("controlAccentColor", "labelColor"),
                std::cmp::Ordering::Greater => ("quaternaryLabelColor", "tertiaryLabelColor"),
            };
            let _: () = msg_send![&**dot, setTextColor: &*color(dot_color)];
            let _: () = msg_send![&**name, setTextColor: &*color(name_color)];
            let font: Id = if PAGES[i] == w.page {
                msg_send![class!(NSFont), boldSystemFontOfSize: 14.0f64]
            } else {
                msg_send![class!(NSFont), systemFontOfSize: 14.0f64]
            };
            let _: () = msg_send![&**name, setFont: &*font];
        }
        for (i, p) in w.pages.iter().enumerate() {
            let _: () = msg_send![&**p, setHidden: Bool::new(PAGES[i] != w.page)];
        }
        let _: () = msg_send![&*w.next, setTitle: &*nsstring(&next)];
        let _: () = msg_send![&*w.next, setTag: next_tag.map_or(0, |b| b as isize)];
        let _: () = msg_send![&*w.next, setEnabled: Bool::new(next_tag.is_some())];
        let _: () = msg_send![&*w.back, setEnabled: Bool::new(back)];
        let _: () = msg_send![&*w.log, setHidden: Bool::new(w.page != Page::Install)];
        // Closing would stop the installation halfway.
        let close: *mut AnyObject = msg_send![&*w.window, standardWindowButton: 0usize];
        if !close.is_null() {
            let _: () = msg_send![close, setEnabled: Bool::new(w.page != Page::Install || w.failed)];
        }
    }
}

/// Continue and Go Back on the first two pages; true if `b` was one of them.
pub(super) fn turn_page(b: Button) -> bool {
    if b != Button::PageNext && b != Button::PageBack {
        return false;
    }
    SETUP.with(|s| {
        if let Some(w) = s.borrow_mut().as_mut() {
            w.page = match (b, w.page) {
                (Button::PageNext, _) => Page::Items,
                _ => Page::Welcome,
            };
            show_page(w);
        }
    });
    true
}

/// The window's close button: quit, or start once everything is installed.
pub(super) fn close_clicked() {
    let page = SETUP.with(|s| s.borrow().as_ref().map(|w| (w.page, w.failed)));
    match page {
        Some((Page::Done, _)) => send(Event::Button(Button::Start)),
        Some((Page::Install, false)) | None => {}
        Some(_) => send(Event::Button(Button::Quit)),
    }
}

pub(crate) fn set_mode(mode: Mode) {
    on_main_async(move || {
        SETUP.with(|s| {
            let mut s = s.borrow_mut();
            let Some(w) = s.as_mut() else { return };
            (w.page, w.failed) = match mode {
                Mode::Consent => (Page::Welcome, false),
                Mode::Running => (Page::Install, false),
                Mode::Failed => (Page::Install, true),
                Mode::Done => (Page::Done, false),
            };
            show_page(w);
        });
    });
}

/// The overall fraction (0 to 1), the current step, and the last line of its output.
pub(crate) fn progress(fraction: f64, step: &str, line: &str) {
    let (step, line) = (step.to_owned(), line.to_owned());
    on_main_async(move || {
        SETUP.with(|s| {
            let s = s.borrow();
            let Some(w) = s.as_ref() else { return };
            // SAFETY: setDoubleValue: takes a double.
            let _: () = unsafe { msg_send![&*w.bar, setDoubleValue: fraction.clamp(0.0, 1.0)] };
            set_text(&w.step, &step);
            set_text(&w.line, &line);
        });
    });
}

pub(crate) fn close() {
    on_main(|| {
        if let Some(w) = SETUP.with(|s| s.borrow_mut().take()) {
            // SAFETY: orderOut: with a nil sender.
            let _: () = unsafe { msg_send![&*w.window, orderOut: null::<AnyObject>()] };
        }
    });
}
