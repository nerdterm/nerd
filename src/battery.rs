//! The battery charge for the top row, read from IOKit when macOS says the power changed.

use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Power {
    Battery,
    Charging,
    /// On the charger but not charging: full, or held back by optimized charging.
    Plugged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Charge {
    pub percent: u8,
    pub power: Power,
    /// macOS Low Power Mode is on.
    pub low_power: bool,
    /// To empty on battery, to full while charging; None while macOS estimates.
    pub minutes: Option<u16>,
    /// The power adapter's rating, when one is plugged in.
    pub watts: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Fine,
    /// At or under `low` on battery; macOS warns at 20%.
    Low,
    /// At or under `critical` on battery.
    Critical,
}

impl Charge {
    pub fn level(self, low: u8, critical: u8) -> Level {
        match self.power {
            // 0 turns a threshold off
            Power::Battery if critical > 0 && self.percent <= critical => Level::Critical,
            Power::Battery if low > 0 && self.percent <= low => Level::Low,
            _ => Level::Fine,
        }
    }

    /// `ϟ 71%` on the charger, `46%` on battery, `46% L` in Low Power Mode.
    pub fn text(self) -> String {
        self.with(&format!("{}%", self.percent))
    }

    /// `4:10` to empty, `ϟ 1:20` to full, `ϟ full`, `ϟ not charging`, or `estimating`.
    pub fn time_text(self) -> String {
        self.with(&self.time())
    }

    pub fn time(self) -> String {
        match (self.power, self.minutes) {
            (Power::Plugged, _) if self.percent >= 100 => "full".into(),
            (Power::Plugged, _) => match self.watts {
                Some(w) => format!("not charging {w}W"),
                None => "not charging".into(),
            },
            (_, Some(m)) => format!("{}:{:02}", m / 60, m % 60),
            (_, None) => "estimating".into(),
        }
    }

    /// The hover line: `87%, charging, 0:41 to full, 96W adapter, Low Power Mode`.
    pub fn details(self) -> String {
        let mut parts = vec![format!("{}%", self.percent)];
        parts.push(match self.power {
            Power::Battery => "on battery".into(),
            Power::Charging => "charging".into(),
            Power::Plugged if self.percent >= 100 => "plugged in, full".into(),
            Power::Plugged => "plugged in, not charging".into(),
        });
        match (self.power, self.minutes) {
            (Power::Battery, Some(m)) => parts.push(format!("{}:{:02} left", m / 60, m % 60)),
            (Power::Charging, Some(m)) => parts.push(format!("{}:{:02} to full", m / 60, m % 60)),
            (Power::Plugged, _) => {}
            (_, None) => parts.push("estimating the time".into()),
        }
        if let Some(w) = self.watts {
            parts.push(format!("{w}W adapter"));
        }
        if self.low_power {
            parts.push("Low Power Mode".into());
        }
        parts.join(", ")
    }

    fn with(self, body: &str) -> String {
        let bolt = if self.power == Power::Battery { "" } else { "\u{3df} " };
        let low = if self.low_power { " L" } else { "" };
        format!("{bolt}{body}{low}")
    }
}

static NOW: Mutex<Option<Charge>> = Mutex::new(None);

/// How long the details stay after the pointer last moved over the battery.
pub const HOVER: std::time::Duration = std::time::Duration::from_secs(3);

pub fn now() -> Option<Charge> {
    *NOW.lock().unwrap_or_else(|e| e.into_inner())
}

/// What a power source says, as IOKit names it.
pub fn charge(on_ac: bool, charging: bool, low_power: bool, current: i32, max: i32, minutes: i32) -> Option<Charge> {
    let percent = (current.max(0) as i64 * 100 / max.max(1) as i64).min(100) as u8;
    let power = match (on_ac, charging) {
        (false, _) => Power::Battery,
        (true, true) => Power::Charging,
        (true, false) => Power::Plugged,
    };
    // IOKit says -1 while it estimates, and 0 for the direction it is not going
    let minutes = u16::try_from(minutes).ok().filter(|m| *m > 0);
    (max > 0).then_some(Charge { percent, power, low_power, minutes, watts: None })
}

/// Reads now, then sleeps until macOS says the power changed: no polling, no process.
pub fn start() {
    let _ = std::thread::Builder::new().name("battery".into()).spawn(|| {
        extern "C" fn changed(_: *mut c_void) {
            *NOW.lock().unwrap_or_else(|e| e.into_inner()) = read();
        }
        changed(std::ptr::null_mut());
        // SAFETY: the source and the run loop belong to this thread, which never returns.
        unsafe {
            let source = IOPSNotificationCreateRunLoopSource(changed, std::ptr::null_mut());
            if source.is_null() {
                return;
            }
            CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopDefaultMode);
            CFRunLoopRun();
        }
    });
}

use crate::cf::*;
use std::ffi::c_void;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOPSCopyPowerSourcesInfo() -> CFRef;
    fn IOPSCopyPowerSourcesList(blob: CFRef) -> CFRef;
    fn IOPSGetPowerSourceDescription(blob: CFRef, source: CFRef) -> CFRef;
    fn IOPSNotificationCreateRunLoopSource(callback: extern "C" fn(*mut c_void), context: *mut c_void) -> CFRef;
    fn IOPSCopyExternalPowerAdapterDetails() -> CFRef;
}

/// The internal battery's charge, None on a Mac with none.
fn read() -> Option<Charge> {
    // SAFETY: every Copy/Create below is released; Get results are borrowed from `blob` while it lives.
    unsafe {
        let key = |s: &std::ffi::CStr| CFStringCreateWithCString(std::ptr::null(), s.as_ptr(), 0x0800_0100);
        let blob = IOPSCopyPowerSourcesInfo();
        if blob.is_null() {
            return None;
        }
        // CF halts on a NULL argument, and any app may publish a power source with missing keys
        let same = |a: CFRef, b: CFRef| !a.is_null() && !b.is_null() && CFEqual(a, b) != 0;
        let flag = |v: CFRef| !v.is_null() && CFGetTypeID(v) == CFBooleanGetTypeID() && CFBooleanGetValue(v) != 0;
        let list = IOPSCopyPowerSourcesList(blob);
        let keys = [key(c"Type"), key(c"InternalBattery"), key(c"Current Capacity"), key(c"Max Capacity"), key(c"Is Charging"), key(c"Power Source State"), key(c"AC Power"), key(c"LPM Active"), key(c"Time to Empty"), key(c"Time to Full Charge"), key(c"Watts")];
        let [kind, internal, current, max, charging, state, ac, lpm, to_empty, to_full, watts] = keys;
        let int = |d: CFRef, k: CFRef| {
            let mut n = 0i32;
            let v = CFDictionaryGetValue(d, k);
            // 9: kCFNumberIntType
            (!v.is_null() && CFGetTypeID(v) == CFNumberGetTypeID() && CFNumberGetValue(v, 9, (&raw mut n).cast()) != 0).then_some(n)
        };
        let mut found = None;
        for i in 0..if list.is_null() { 0 } else { CFArrayGetCount(list) } {
            let d = IOPSGetPowerSourceDescription(blob, CFArrayGetValueAtIndex(list, i));
            if d.is_null() || !same(CFDictionaryGetValue(d, kind), internal) {
                continue;
            }
            let st = CFDictionaryGetValue(d, state);
            // a boolean on this Mac; a number is taken too
            let low = flag(CFDictionaryGetValue(d, lpm)) || int(d, lpm).is_some_and(|n| n != 0);
            let (on_ac, is_charging) = (same(st, ac), flag(CFDictionaryGetValue(d, charging)));
            let minutes = int(d, if is_charging { to_full } else { to_empty }).unwrap_or(-1);
            found = charge(on_ac, is_charging, low, int(d, current).unwrap_or(0), int(d, max).unwrap_or(0), minutes);
            if let Some(c) = found.as_mut().filter(|c| c.power != Power::Battery) {
                let adapter = IOPSCopyExternalPowerAdapterDetails();
                if !adapter.is_null() {
                    c.watts = int(adapter, watts).and_then(|w| u16::try_from(w).ok());
                    CFRelease(adapter);
                }
            }
            break;
        }
        for k in keys.into_iter().filter(|k| !k.is_null()) {
            CFRelease(k);
        }
        if !list.is_null() {
            CFRelease(list);
        }
        CFRelease(blob);
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iokit_says_the_charge_and_whether_it_charges() {
        assert_eq!(charge(false, false, false, 46, 100, -1), Some(Charge { percent: 46, power: Power::Battery, low_power: false, minutes: None, watts: None }));
        assert_eq!(charge(true, true, false, 71, 100, -1).map(|c| c.power), Some(Power::Charging));
        assert_eq!(charge(true, false, false, 80, 100, -1).map(|c| c.power), Some(Power::Plugged), "held back: on the charger, not charging");
        assert_eq!(charge(true, true, false, 3500, 5000, -1).map(|c| c.percent), Some(70), "older Macs give mAh");
        assert_eq!(charge(true, false, false, 0, 0, -1), None, "no capacity: no battery");
        assert_eq!(charge(true, true, false, 71, 100, -1).map(Charge::text), Some("\u{3df} 71%".into()));
        assert_eq!(charge(false, false, true, 46, 100, -1).map(Charge::text), Some("46% L".into()), "Low Power Mode");
        assert_eq!(charge(true, true, true, 71, 100, -1).map(Charge::text), Some("\u{3df} 71% L".into()));
        let at = |percent, power| Charge { percent, power, low_power: false, minutes: None, watts: None }.level(20, 10);
        assert_eq!([at(21, Power::Battery), at(20, Power::Battery), at(11, Power::Battery), at(10, Power::Battery)], [Level::Fine, Level::Low, Level::Low, Level::Critical]);
        assert_eq!(at(5, Power::Charging), Level::Fine, "on the charger it is not running out");
        let b = Charge { percent: 30, power: Power::Battery, low_power: false, minutes: None, watts: None };
        assert_eq!((b.level(35, 15), b.level(40, 30), b.level(0, 0)), (Level::Low, Level::Critical, Level::Fine), "the thresholds are the config's");
        assert_eq!(Charge { percent: 0, power: Power::Battery, low_power: false, minutes: None, watts: None }.level(0, 0), Level::Fine, "0 is off, even at 0%");
        let time = |on_ac, charging, percent, minutes| charge(on_ac, charging, false, percent, 100, minutes).map(Charge::time_text);
        assert_eq!(time(false, false, 46, 250), Some("4:10".into()), "to empty on battery");
        assert_eq!(time(true, true, 71, 80), Some("\u{3df} 1:20".into()), "to full on the charger");
        assert_eq!(time(true, true, 71, -1), Some("\u{3df} estimating".into()));
        assert_eq!(time(false, false, 46, 0), Some("estimating".into()), "0 is not an answer");
        assert_eq!(time(true, false, 80, 0), Some("\u{3df} not charging".into()));
        assert_eq!(time(true, false, 100, 0), Some("\u{3df} full".into()));
        let mut c = charge(true, true, true, 87, 100, 41).unwrap();
        c.watts = Some(96);
        assert_eq!(c.details(), "87%, charging, 0:41 to full, 96W adapter, Low Power Mode");
        assert_eq!(charge(false, false, false, 46, 100, 250).unwrap().details(), "46%, on battery, 4:10 left");
        let mut weak = charge(true, false, false, 60, 100, 0).unwrap();
        weak.watts = Some(30);
        assert_eq!((weak.time_text(), weak.details()), ("\u{3df} not charging 30W".into(), "60%, plugged in, not charging, 30W adapter".into()), "a weak adapter shows why");
    }

    #[test]
    fn reading_the_battery_does_not_crash_with_or_without_one() {
        let _ = read();
    }
}
