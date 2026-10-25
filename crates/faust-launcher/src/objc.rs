//! Small helpers for talking to Foundation and AppKit through Objective-C messages.

use std::ffi::{CStr, CString, c_char};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};

/// An owned Objective-C object of any class.
pub(crate) type Id = Retained<AnyObject>;

/// An `NSString` with the text of `s` (NUL characters dropped).
pub(crate) fn nsstring(s: &str) -> Id {
    let c = CString::new(s.replace('\0', "")).expect("NUL characters were removed");
    // SAFETY: stringWithUTF8String: copies the NUL-terminated UTF-8 bytes it is given.
    unsafe { msg_send![class!(NSString), stringWithUTF8String: c.as_ptr()] }
}

/// The text of an `NSString` (empty for nil).
pub(crate) fn rust_string(s: *mut AnyObject) -> String {
    if s.is_null() {
        return String::new();
    }
    // SAFETY: `s` is a non-nil NSString; UTF8String returns a NUL-terminated buffer that
    // lives at least as long as the string, and it is copied right away.
    unsafe {
        let p: *const c_char = msg_send![s, UTF8String];
        if p.is_null() { String::new() } else { CStr::from_ptr(p).to_string_lossy().into_owned() }
    }
}
