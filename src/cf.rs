//! CoreFoundation, declared by hand for the few calls nerd makes: the battery and the clock.

use std::ffi::{c_char, c_void};

pub type CFRef = *const c_void;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    pub static kCFRunLoopDefaultMode: CFRef;
    pub static kCFPreferencesAnyApplication: CFRef;
    pub fn CFArrayGetCount(array: CFRef) -> isize;
    pub fn CFArrayGetValueAtIndex(array: CFRef, i: isize) -> CFRef;
    pub fn CFDictionaryGetValue(dict: CFRef, key: CFRef) -> CFRef;
    // C `Boolean` is a byte: read as u8, never as Rust `bool`
    pub fn CFNumberGetValue(number: CFRef, kind: isize, out: *mut c_void) -> u8;
    pub fn CFBooleanGetValue(boolean: CFRef) -> u8;
    pub fn CFGetTypeID(r: CFRef) -> usize;
    pub fn CFNumberGetTypeID() -> usize;
    pub fn CFBooleanGetTypeID() -> usize;
    pub fn CFStringGetTypeID() -> usize;
    pub fn CFStringCreateWithCString(alloc: CFRef, s: *const c_char, encoding: u32) -> CFRef;
    pub fn CFStringGetCString(s: CFRef, buf: *mut c_char, len: isize, encoding: u32) -> u8;
    pub fn CFStringGetLength(s: CFRef) -> isize;
    pub fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
    pub fn CFEqual(a: CFRef, b: CFRef) -> u8;
    pub fn CFRelease(r: CFRef);
    pub fn CFRunLoopGetCurrent() -> CFRef;
    pub fn CFRunLoopAddSource(run_loop: CFRef, source: CFRef, mode: CFRef);
    pub fn CFRunLoopRun();
    pub fn CFPreferencesAppSynchronize(app: CFRef) -> u8;
    pub fn CFPreferencesCopyAppValue(key: CFRef, app: CFRef) -> CFRef;
    pub fn CFLocaleCreate(alloc: CFRef, id: CFRef) -> CFRef;
    pub fn CFLocaleCopyCurrent() -> CFRef;
    pub fn CFDateFormatterCreateDateFormatFromTemplate(alloc: CFRef, template: CFRef, options: usize, locale: CFRef) -> CFRef;
    pub fn CFDateFormatterCreate(alloc: CFRef, locale: CFRef, date: isize, time: isize) -> CFRef;
    pub fn CFDateFormatterSetFormat(formatter: CFRef, format: CFRef);
    pub fn CFDateFormatterCreateStringWithAbsoluteTime(alloc: CFRef, formatter: CFRef, at: f64) -> CFRef;
    pub fn CFAbsoluteTimeGetCurrent() -> f64;
    pub fn CFTimeZoneResetSystem();
}

pub const UTF8: u32 = 0x0800_0100;

/// A CF object nerd created or copied, released once when it goes.
pub struct Owned(pub CFRef);

impl Owned {
    pub fn string(s: &str) -> Owned {
        let c = std::ffi::CString::new(s).unwrap_or_default();
        // SAFETY: a NUL-terminated UTF-8 string; the result is owned by the Owned.
        Owned(unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), UTF8) })
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: Owned holds only Create/Copy results, released here once.
            unsafe { CFRelease(self.0) };
        }
    }
}

/// A CFString's text, or empty.
pub fn text(s: CFRef) -> String {
    if s.is_null() {
        return String::new();
    }
    // SAFETY: the buffer is as long as the string can take in UTF-8, plus its NUL.
    let size = unsafe { CFStringGetMaximumSizeForEncoding(CFStringGetLength(s), UTF8) }.max(0) as usize + 1;
    let mut buf = vec![0 as c_char; size];
    if unsafe { CFStringGetCString(s, buf.as_mut_ptr(), size as isize, UTF8) } == 0 {
        return String::new();
    }
    // SAFETY: CFStringGetCString wrote a NUL-terminated string into buf.
    unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned()
}
