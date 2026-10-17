//! The key file nerd writes for Ghostty, and the one line that includes it.
//!
//! Ghostty 1.3.1 drops the `option` prefix when the layout prints a non-ASCII character: its
//! encoder can only prefix a single byte, and `ы` is two. Upstream fixed this on 2026-09-04
//! ("input: encode non-ASCII alt prefixes as UTF-8"), which no stable release carries yet, so
//! until then a Russian keyboard reaches nerd as plain letters and no shortcut fires.
//!
//! The way out is Ghostty's own: a binding on the *physical* key, which sends the escape
//! sequence itself whatever the layout prints. The file is generated from `OPTION_LETTERS`,
//! so it cannot drift from what the program answers to.

use std::path::{Path, PathBuf};

use crate::keys::OPTION_LETTERS;

const MARK: &str = "# nerd: option shortcuts on the physical key, whatever the layout prints";

pub fn keys_path() -> PathBuf {
    crate::paths::config_path().with_file_name("ghostty-keys.conf")
}

pub fn ghostty_config() -> PathBuf {
    crate::config::home_dir().join(".config/ghostty/config")
}

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
/// `shift` is part of the key for the two the tree spells with it.
fn letters() -> Vec<(char, bool)> {
    let mut out: Vec<(char, bool)> = OPTION_LETTERS.iter().map(|c| (*c, false)).collect();
    for r in crate::tree::COMMAND.iter().filter(|r| r.alt) {
        if let crossterm::event::KeyCode::Char(c) = r.code {
            let shift = c.is_uppercase();
            let c = c.to_ascii_lowercase();
            if !out.contains(&(c, shift)) {
                out.push((c, shift));
            }
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
    for (c, shift) in letters() {
        if let Some(code) = code(c) {
            // `esc:` takes its value as-is (`ghostty.5.md`); doubling a backslash here sent two.
            let (mods, text) = if shift { ("alt+shift+", c.to_ascii_uppercase()) } else { ("alt+", c) };
            s.push_str(&format!("keybind = {mods}{code}=esc:{text}\n"));
        }
    }
    s
}

/// Writes the key file, and says whether its contents changed.
pub fn refresh() -> std::io::Result<bool> {
    let path = keys_path();
    let want = conf();
    if std::fs::read_to_string(&path).is_ok_and(|have| have == want) {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, want)?;
    Ok(true)
}

/// Whether Ghostty's own config pulls our file in.
pub fn is_linked() -> bool {
    let p = keys_path();
    std::fs::read_to_string(ghostty_config()).is_ok_and(|s| includes(&s, &p))
}

fn includes(config: &str, keys: &Path) -> bool {
    let keys = keys.to_string_lossy();
    config
        .lines()
        .any(|l| !l.trim_start().starts_with('#') && l.contains("config-file") && l.contains(&*keys))
}

/// Adds the include line once. Ghostty's own config is otherwise left alone.
pub fn link() -> std::io::Result<()> {
    refresh()?;
    let path = ghostty_config();
    let mut s = std::fs::read_to_string(&path).unwrap_or_default();
    if includes(&s, &keys_path()) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if !s.is_empty() && !s.ends_with('\n') {
        s.push('\n');
    }
    s.push_str(&format!("\n{MARK}\nconfig-file = {}\n", keys_path().display()));
    std::fs::write(&path, s)
}

/// Takes the include line and our marker back out, and removes the file.
pub fn unlink() -> std::io::Result<()> {
    let path = ghostty_config();
    if let Ok(s) = std::fs::read_to_string(&path) {
        let keys = keys_path();
        let kept: Vec<&str> = s
            .lines()
            .filter(|l| *l != MARK && !(l.contains("config-file") && l.contains(&*keys.to_string_lossy())))
            .collect();
        std::fs::write(&path, format!("{}\n", kept.join("\n").trim_end()))?;
    }
    let _ = std::fs::remove_file(keys_path());
    Ok(())
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
        assert!(conf.contains("keybind = alt+shift+key_d=esc:D"), "the tree's delete is missing");
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

    #[test]
    fn the_include_is_recognised_only_when_it_is_live() {
        let keys = Path::new("/tmp/x/ghostty-keys.conf");
        assert!(includes("config-file = /tmp/x/ghostty-keys.conf", keys));
        assert!(!includes("# config-file = /tmp/x/ghostty-keys.conf", keys), "a comment is not a link");
        assert!(!includes("config-file = /somewhere/else.conf", keys));
    }
}
