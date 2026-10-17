//! The mouse pointer's shape (OSC 22): a hand over what a click acts on, a resize arrow over what drags.

/// Terminals take CSS cursor names; these are the ones nerd sends.
pub const DEFAULT: &str = "default";
pub const POINTER: &str = "pointer";
pub const TEXT: &str = "text";
pub const EW: &str = "ew-resize";
pub const NS: &str = "ns-resize";

pub fn osc(shape: &str) -> Vec<u8> {
    format!("\x1b]22;{shape}\x1b\\").into_bytes()
}

/// The last OSC 22 in a pane's output: `Some(None)` resets it, `Some(Some(name))` sets it. Only a CSS name passes,
/// so a pane cannot put anything else into the client's terminal through it.
pub fn requested(bytes: &[u8]) -> Option<Option<String>> {
    let mut last = None;
    let mut rest = bytes;
    while let Some(at) = rest.windows(5).position(|w| w == b"\x1b]22;") {
        let body = &rest[at + 5..];
        let Some(end) = body.iter().position(|b| *b == 0x07 || *b == 0x1b) else { break };
        let name = &body[..end];
        if name.is_empty() {
            last = Some(None);
        } else if name.len() <= 32 && name.iter().all(|b| b.is_ascii_lowercase() || *b == b'-') {
            last = Some(Some(String::from_utf8_lossy(name).into_owned()));
        }
        rest = &body[end..];
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pane_sets_and_resets_its_pointer_and_nothing_else_passes() {
        assert_eq!(requested(b"x\x1b]22;pointer\x1b\\y"), Some(Some("pointer".into())));
        assert_eq!(requested(b"\x1b]22;grab\x07\x1b]22;\x07"), Some(None), "the last one wins; empty resets");
        assert_eq!(requested(b"\x1b]22;x\x1b[31m;\x07"), Some(Some("x".into())), "the name ends at ESC");
        assert_eq!(requested(b"\x1b]22;evil\x1b]52;c;AA\x07"), Some(Some("evil".into())));
        assert_eq!(requested(b"\x1b]22;a b\x07"), None, "not a CSS name: ignored");
        assert_eq!(requested(b"\x1b]22;pointer"), None, "unterminated: waits");
        assert_eq!(requested(b"plain"), None);
        assert_eq!(osc(EW), b"\x1b]22;ew-resize\x1b\\");
    }
}
