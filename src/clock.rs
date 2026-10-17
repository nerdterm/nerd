//! The top row's clock as the menu bar clock shows it: its settings, the user's locale, the system's formatter.

use crate::cf::*;

/// The menu bar clock settings that change what it shows; None is the system's default.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Settings {
    pub day_of_week: Option<bool>,
    pub hours24: Option<bool>,
    pub am_pm: Option<bool>,
    pub seconds: Option<bool>,
}

/// ICU templates for the date and the time; the formatter orders them for the locale.
pub fn templates(s: Settings) -> (String, String) {
    let date = format!("{}dMMM", if s.day_of_week.unwrap_or(true) { "EEE" } else { "" });
    let hours = match (s.hours24, s.am_pm) {
        (Some(true), _) => "HH",
        (Some(false), _) => "h",
        // the locale's hours; J leaves out AM/PM when the menu bar hides it
        (None, Some(false)) => "J",
        (None, _) => "j",
    };
    let secs = if s.seconds == Some(true) { "ss" } else { "" };
    let ampm = if s.hours24 == Some(false) && s.am_pm.unwrap_or(true) { "a" } else { "" };
    (date, format!("{hours}mm{secs}{ampm}"))
}

/// `01:07`, or with the date `Sun 4. 10.  01:07`, as the menu bar has it now, or by the user's own patterns
/// (empty: the menu bar's); and whether it ticks by the second.
pub fn now(date: bool, time_pattern: &str, date_pattern: &str) -> (String, bool) {
    // CoreFoundation keeps the time zone it started with; a laptop that crossed zones must follow the menu bar
    // SAFETY: no preconditions.
    unsafe { CFTimeZoneResetSystem() };
    let locale = current_locale();
    let (d, t) = templates(settings());
    // a NUL cannot reach CoreFoundation: such a pattern counts as none
    let own = |p: &str| (!p.is_empty() && !p.contains('\0')).then(|| p.to_string());
    let time = own(time_pattern).unwrap_or_else(|| pattern(&t, &locale));
    let day = own(date_pattern).unwrap_or_else(|| pattern(&d, &locale));
    // SAFETY: CFAbsoluteTimeGetCurrent has no preconditions.
    let at = unsafe { CFAbsoluteTimeGetCurrent() };
    let text = if date { format!("{}  {}", format(&day, &locale, at), format(&time, &locale, at)) } else { format(&time, &locale, at) };
    (text, seconds(&time))
}

/// Whether a pattern shows seconds: an `s` outside its quoted text.
pub fn seconds(pattern: &str) -> bool {
    pattern.split('\'').step_by(2).any(|plain| plain.contains('s'))
}

/// Read fresh each time, so a change in System Settings shows at the next tick.
fn settings() -> Settings {
    let app = Owned::string("com.apple.menuextra.clock");
    // SAFETY: Synchronize drops the cached copy; CopyAppValue returns an owned value or NULL.
    unsafe { CFPreferencesAppSynchronize(app.0) };
    let flag = |key: &str| -> Option<bool> {
        let v = Owned(unsafe { CFPreferencesCopyAppValue(Owned::string(key).0, app.0) });
        number(v.0).map(|n| n != 0)
    };
    Settings { day_of_week: flag("ShowDayOfWeek"), hours24: flag("Show24Hour"), am_pm: flag("ShowAMPM"), seconds: flag("ShowSeconds") }
}

/// The user's locale as set now; CFLocaleCopyCurrent keeps the one the process started with.
fn current_locale() -> Owned {
    // SAFETY: as in settings(); a missing AppleLocale falls back to the current locale.
    unsafe {
        CFPreferencesAppSynchronize(kCFPreferencesAnyApplication);
        let id = Owned(CFPreferencesCopyAppValue(Owned::string("AppleLocale").0, kCFPreferencesAnyApplication));
        if id.0.is_null() || CFGetTypeID(id.0) != CFStringGetTypeID() {
            return Owned(CFLocaleCopyCurrent());
        }
        Owned(CFLocaleCreate(std::ptr::null(), id.0))
    }
}

/// A CFNumber or a CFBoolean as an integer.
fn number(v: CFRef) -> Option<i32> {
    if v.is_null() {
        return None;
    }
    // SAFETY: the type is checked before each read.
    unsafe {
        if CFGetTypeID(v) == CFBooleanGetTypeID() {
            return Some(CFBooleanGetValue(v) as i32);
        }
        let mut n = 0i32;
        // 9: kCFNumberIntType
        (CFGetTypeID(v) == CFNumberGetTypeID() && CFNumberGetValue(v, 9, (&raw mut n).cast()) != 0).then_some(n)
    }
}

/// The locale's pattern for a template: `jmm` is `HH:mm` in en_CZ and `h:mm a` in en_US.
fn pattern(template: &str, locale: &Owned) -> String {
    // SAFETY: the Create result is an Owned and released.
    text(Owned(unsafe { CFDateFormatterCreateDateFormatFromTemplate(std::ptr::null(), Owned::string(template).0, 0, locale.0) }).0)
}

/// `pattern` for `locale` at `at` (seconds since 2001, CFAbsoluteTime).
fn format(pattern: &str, locale: &Owned, at: f64) -> String {
    // SAFETY: every Create result is an Owned and released; the formatter outlives its use.
    unsafe {
        let f = Owned(CFDateFormatterCreate(std::ptr::null(), locale.0, 0, 0));
        if f.0.is_null() {
            return String::new();
        }
        CFDateFormatterSetFormat(f.0, Owned::string(pattern).0);
        text(Owned(CFDateFormatterCreateStringWithAbsoluteTime(std::ptr::null(), f.0, at)).0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_menu_bar_settings_make_the_templates() {
        assert_eq!(templates(Settings::default()), ("EEEdMMM".into(), "jmm".into()), "unset: the locale decides 12 or 24 hours");
        let s = Settings { day_of_week: Some(false), hours24: Some(false), am_pm: Some(true), seconds: Some(true) };
        assert_eq!(templates(s), ("dMMM".into(), "hmmssa".into()));
        assert_eq!(templates(Settings { hours24: Some(true), ..s }).1, "HHmmss", "no AM/PM in 24 hours");
        assert_eq!(templates(Settings { am_pm: Some(false), ..Settings::default() }).1, "Jmm", "the locale's hours without AM/PM");
    }

    #[test]
    fn the_system_formatter_writes_it_as_the_menu_bar_does() {
        // 2026-10-04 12:00 UTC, as seconds since 2001: October in every time zone
        let at = 812_808_000.0;
        let locale = Owned(unsafe { CFLocaleCreate(std::ptr::null(), Owned::string("en_CZ").0) });
        let (d, t) = templates(Settings { hours24: Some(true), ..Settings::default() });
        assert_eq!(pattern(&t, &locale), "HH:mm");
        assert_eq!(pattern(&d, &locale), "EEE d. M.", "the menu bar's own order and dots, from the locale");
        assert_eq!(format("MMMM yyyy", &locale, at), "October 2026", "a pattern of the user's own, as written; the date itself depends on the zone");
        let long = format!("'{}' HH", "x".repeat(400));
        assert_eq!(format(&long, &locale, at).len(), 403, "no limit on the length of what it writes");
    }

    #[test]
    fn seconds_are_an_s_outside_quotes() {
        assert!(seconds("HH:mm:ss") && !seconds("HH:mm") && !seconds("HH:mm 'Uhrs'") && seconds("'at' HH:mm:ss"));
    }
}
