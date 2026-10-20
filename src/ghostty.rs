//! The key bindings the Ghostty preset carries (`presets.rs`).
//!
//! Ghostty 1.3.1 drops the `option` prefix when the layout prints a non-ASCII character: its
//! encoder can only prefix a single byte, and `ы` is two. Upstream fixed this on 2026-09-04
//! ("input: encode non-ASCII alt prefixes as UTF-8"), which no stable release carries yet, so
//! until then a Russian keyboard reaches nerd as plain letters and no shortcut fires.
//!
//! The way out is Ghostty's own: a binding on the *physical* key, which sends the escape
//! sequence itself whatever the layout prints. The bindings are generated from `OPTION_LETTERS`,
//! so they cannot drift from what the program answers to.

use crate::keys::OPTION_LETTERS;

pub const MARK: &str = "# nerd: option shortcuts on the physical key, whatever the layout prints";

/// The W3C code for the key that prints this character on a US keyboard.
fn code(c: char) -> Option<String> {
    Some(match c {
        'a'..='z' => format!("key_{c}"),
        '/' => "slash".into(),
        ',' => "comma".into(),
        '-' => "minus".into(),
        '=' => "equal".into(),
        '\\' => "backslash".into(),
        _ => return None,
    })
}

/// Every letter the program answers to with `option`: the global ones, and the tree's own.
fn letters() -> Vec<char> {
    let mut out = OPTION_LETTERS.to_vec();
    for r in crate::tree::COMMAND.iter().filter(|r| r.alt) {
        if let crossterm::event::KeyCode::Char(c) = r.code
            && !out.contains(&c)
        {
            out.push(c);
        }
    }
    out
}

pub fn conf() -> String {
    let mut s = String::from(MARK);
    s.push_str("\n# Written by nerd. Do not edit: it is rewritten whenever nerd starts.\n");
    // Option and an arrow stay Ghostty's `ESC b` / `ESC f`, the shell's word motion; nerd moves
    // between areas on shift, option and an arrow, spelled the way the terminal protocol spells it.
    for (key, csi) in [("left", "1;4D"), ("right", "1;4C"), ("up", "1;4A"), ("down", "1;4B")] {
        s.push_str(&format!("keybind = shift+alt+{key}=csi:{csi}\n"));
    }
    for c in letters() {
        if let Some(code) = code(c) {
            // `esc:` takes its value as-is (`ghostty.5.md`); doubling a backslash here sent two.
            s.push_str(&format!("keybind = alt+{code}=esc:{c}\n"));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_letter_the_program_answers_to_is_in_the_file() {
        let conf = conf();
        for &c in OPTION_LETTERS {
            let code = code(c).unwrap_or_else(|| panic!("no W3C code for {c:?}"));
            assert!(conf.contains(&format!("alt+{code}=")), "{c:?} is missing from the file");
        }
        // the tree's own letters reach it through the same file, or a Russian layout loses them
        assert!(conf.contains("keybind = alt+key_e=esc:e"), "the tree's edit is missing");
        assert!(conf.contains("keybind = alt+key_x=esc:x"), "the tree's delete is missing");
        assert_eq!(
            conf.lines().filter(|l| l.starts_with("keybind")).count(),
            letters().len() + 4,
            "every letter, and the four arrows that move between areas"
        );
    }

    #[test]
    fn option_and_an_arrow_is_left_to_the_shell() {
        let conf = conf();
        assert!(!conf.contains("keybind = alt+left"), "option+left stays ESC b, the shell's word motion");
        assert!(conf.contains("shift+alt+left=csi:1;4D"), "and the areas move on shift with it");
        assert!(conf.contains("shift+alt+right=csi:1;4C"));
    }

    #[test]
    fn a_backslash_is_sent_once() {
        // `esc:` takes the parameter as-is, so a doubled one arrives as two characters and the
        // second lands in the pane the shortcut just opened.
        assert!(conf().contains("alt+backslash=esc:\\\n"));
    }
}
