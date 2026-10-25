//! The language of Faust's windows: the first of the Mac's preferred languages that Faust
//! speaks (English, Japanese, German, Italian), else English. `FAUST_LANG=en|ja|de|it`
//! overrides it, and the app is told which one was picked (`FAUST_LANG`) so it can follow.

use std::ffi::{CStr, c_char};
use std::sync::OnceLock;

use objc2::runtime::AnyObject;
use objc2::{class, msg_send};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lang {
    En,
    Ja,
    De,
    It,
}

impl Lang {
    const ALL: [Lang; 4] = [Lang::En, Lang::Ja, Lang::De, Lang::It];

    pub(crate) fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Ja => "ja",
            Lang::De => "de",
            Lang::It => "it",
        }
    }

    /// "de-AT", "it_IT", "ja": by the language subtag alone.
    fn from_code(s: &str) -> Option<Lang> {
        let primary = s.split(['-', '_']).next()?.to_ascii_lowercase();
        Lang::ALL.into_iter().find(|l| l.code() == primary)
    }
}

pub(crate) fn current() -> Lang {
    static LANG: OnceLock<Lang> = OnceLock::new();
    *LANG.get_or_init(detect)
}

/// One of four, in the order English, Japanese, German, Italian.
pub(crate) fn pick<T>(choices: [T; 4]) -> T {
    let [en, ja, de, it] = choices;
    match current() {
        Lang::En => en,
        Lang::Ja => ja,
        Lang::De => de,
        Lang::It => it,
    }
}

fn detect() -> Lang {
    // Tests compare with the English strings.
    if cfg!(test) {
        return Lang::En;
    }
    if let Some(l) = std::env::var("FAUST_LANG").ok().as_deref().and_then(Lang::from_code) {
        return l;
    }
    preferred().iter().find_map(|s| Lang::from_code(s)).unwrap_or(Lang::En)
}

/// `[NSLocale preferredLanguages]`: the order in System Settings > General > Language & Region.
fn preferred() -> Vec<String> {
    // SAFETY: NSLocale and NSString are Foundation classes (linked by objc2); every
    // message here is sent with the argument and return types its documentation gives.
    unsafe {
        let list: *const AnyObject = msg_send![class!(NSLocale), preferredLanguages];
        if list.is_null() {
            return Vec::new();
        }
        let n: usize = msg_send![list, count];
        (0..n)
            .filter_map(|i| {
                let s: *const AnyObject = msg_send![list, objectAtIndex: i];
                let p: *const c_char = msg_send![s, UTF8String];
                (!p.is_null()).then(|| CStr::from_ptr(p).to_string_lossy().into_owned())
            })
            .collect()
    }
}

/// Text in the current language. Each one is a format string, so `{name}` takes `name`
/// from the caller.
macro_rules! tr {
    (en: $en:literal, ja: $ja:literal, de: $de:literal, it: $it:literal $(,)?) => {
        match $crate::i18n::current() {
            $crate::i18n::Lang::En => format!($en),
            $crate::i18n::Lang::Ja => format!($ja),
            $crate::i18n::Lang::De => format!($de),
            $crate::i18n::Lang::It => format!($it),
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes() {
        assert_eq!(Lang::from_code("de-AT"), Some(Lang::De));
        assert_eq!(Lang::from_code("it_IT"), Some(Lang::It));
        assert_eq!(Lang::from_code("JA"), Some(Lang::Ja));
        assert_eq!(Lang::from_code("en-GB"), Some(Lang::En));
        assert_eq!(Lang::from_code("fr-FR"), None);
        assert_eq!(Lang::from_code(""), None);
    }

    #[test]
    fn tests_read_english() {
        let name = "Hello";
        assert_eq!(tr!(en: "Open {name}", ja: "{name} を開く", de: "{name} öffnen", it: "Apri {name}"), "Open Hello");
        assert_eq!(pick(["Yes", "はい", "Ja", "Sì"]), "Yes");
    }
}
