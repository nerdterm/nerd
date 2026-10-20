//! User keypresses -> bytes for the process in a pane; encoding is left to terminput, which knows both the ordinary sequences and the kitty protocol.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Turns a keypress into bytes. None means a key with nothing to send.
pub fn encode_key(k: KeyEvent) -> Option<Vec<u8>> {
    use terminput::Encoding;

    // Option and an arrow as Ghostty sends them by default: a shell reads ESC b / ESC f as a word, and prints `;3D` for CSI 1;3D.
    match (k.code, k.modifiers) {
        (KeyCode::Left, KeyModifiers::ALT) => return Some(b"\x1bb".to_vec()),
        (KeyCode::Right, KeyModifiers::ALT) => return Some(b"\x1bf".to_vec()),
        _ => {}
    }
    let k = control_on_its_key(k);
    let ev: terminput::Event = terminput_crossterm::to_terminput(crossterm::event::Event::Key(k)).ok()?;
    let mut buf = [0u8; 32];
    let n = ev.encode(&mut buf, Encoding::Xterm).ok()?;
    // A pane's program reads UTF-8: a lead byte with no continuation is drawn as `<ffffffff>`.
    if n == 0 || std::str::from_utf8(&buf[..n]).is_err() {
        return None;
    }
    Some(buf[..n].to_vec())
}

/// Control has a code only for an ASCII key: a layout's letter takes the Latin one on its key
/// (Control+`С` is `^C`), and a character on no such key goes without Control.
fn control_on_its_key(k: KeyEvent) -> KeyEvent {
    let KeyCode::Char(c) = k.code else { return k };
    if c.is_ascii() || !k.modifiers.contains(KeyModifiers::CONTROL) {
        return k;
    }
    match latin(c) {
        l if l.is_ascii() => KeyEvent { code: KeyCode::Char(l), ..k },
        _ => KeyEvent { modifiers: k.modifiers - KeyModifiers::CONTROL, ..k },
    }
}

/// Plain arrows and Home/End go as SS3 (`ESC O A`) while the program has application cursor mode on.
pub fn cursor_keys(mut b: Vec<u8>, app: bool) -> Vec<u8> {
    if app && b.len() == 3 && b.starts_with(b"\x1b[") && b"ABCDHF".contains(&b[2]) {
        b[1] = b'O';
    }
    b
}

/// Fallback for when the terminput adapter doesn't match the crossterm version.
pub fn encode_key_fallback(k: KeyEvent) -> Option<Vec<u8>> {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    let mut out = Vec::new();
    if alt {
        out.push(0x1b);
    }
    match k.code {
        KeyCode::Char(c) => {
            if ctrl {
                let b = (c.to_ascii_lowercase() as u8).wrapping_sub(b'a').wrapping_add(1);
                if c.is_ascii_alphabetic() {
                    out.push(b);
                } else {
                    return None;
                }
            } else {
                let mut b = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
            }
        }
        KeyCode::Enter => out.push(b'\r'),
        KeyCode::Tab => out.push(b'\t'),
        KeyCode::BackTab => out.extend_from_slice(b"\x1b[Z"),
        KeyCode::Backspace => out.push(0x7f),
        KeyCode::Esc => out.push(0x1b),
        KeyCode::Left => out.extend_from_slice(b"\x1b[D"),
        KeyCode::Right => out.extend_from_slice(b"\x1b[C"),
        KeyCode::Up => out.extend_from_slice(b"\x1b[A"),
        KeyCode::Down => out.extend_from_slice(b"\x1b[B"),
        KeyCode::Home => out.extend_from_slice(b"\x1b[H"),
        KeyCode::End => out.extend_from_slice(b"\x1b[F"),
        KeyCode::PageUp => out.extend_from_slice(b"\x1b[5~"),
        KeyCode::PageDown => out.extend_from_slice(b"\x1b[6~"),
        KeyCode::Delete => out.extend_from_slice(b"\x1b[3~"),
        KeyCode::Insert => out.extend_from_slice(b"\x1b[2~"),
        KeyCode::F(n @ 1..=4) => {
            out.extend_from_slice(&[0x1b, b'O', b'P' + (n - 1)]);
        }
        _ => return None,
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_usage_key_table_lists_every_global_key_and_no_other() {
        let doc: std::collections::BTreeSet<String> = include_str!("../docs/usage.md")
            .lines()
            .filter_map(|l| l.strip_prefix("| `⌥`+`"))
            .filter_map(|l| l.split(" |").next())
            .map(|k| format!("⌥+{}", k.trim_end_matches('`').replace("\\\\", "\\")))
            .filter(|k| k.chars().count() == 3)
            .collect();
        let code: std::collections::BTreeSet<String> = super::GLOBAL.iter().map(|g| g.label.to_string()).collect();
        assert_eq!(doc, code, "docs/usage.md's key table and keys::GLOBAL");
    }

    use super::*;

    fn key(c: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(c, m)
    }

    #[test]
    fn arrows_and_home_end_follow_application_cursor_mode() {
        for (code, csi, ss3) in [
            (KeyCode::Up, b"\x1b[A", b"\x1bOA"),
            (KeyCode::Down, b"\x1b[B", b"\x1bOB"),
            (KeyCode::Right, b"\x1b[C", b"\x1bOC"),
            (KeyCode::Left, b"\x1b[D", b"\x1bOD"),
            (KeyCode::Home, b"\x1b[H", b"\x1bOH"),
            (KeyCode::End, b"\x1b[F", b"\x1bOF"),
        ] {
            let k = key(code, KeyModifiers::NONE);
            for enc in [encode_key(k).unwrap(), encode_key_fallback(k).unwrap()] {
                assert_eq!(cursor_keys(enc.clone(), false), csi, "{code:?} normal");
                assert_eq!(cursor_keys(enc, true), ss3, "{code:?} application");
            }
        }
        let ctrl_up = encode_key(key(KeyCode::Up, KeyModifiers::CONTROL)).unwrap();
        assert_eq!(cursor_keys(ctrl_up, true), b"\x1b[1;5A");
    }

    #[test]
    fn option_and_left_or_right_move_a_word_in_the_shell() {
        let alt = |c| encode_key(key(c, KeyModifiers::ALT)).unwrap();
        assert_eq!(alt(KeyCode::Left), b"\x1bb", "zsh prints ;3D for the CSI form");
        assert_eq!(alt(KeyCode::Right), b"\x1bf");
        assert_eq!(cursor_keys(alt(KeyCode::Left), true), b"\x1bb", "application cursor mode leaves it alone");
        assert_eq!(alt(KeyCode::Up), b"\x1b[1;3A", "up and down keep the modifier form");
        assert_eq!(encode_key(key(KeyCode::Left, KeyModifiers::ALT | KeyModifiers::SHIFT)).unwrap(), b"\x1b[1;4D", "only plain Option");
    }

    #[test]
    fn plain_letter_goes_as_is() {
        let b = encode_key_fallback(key(KeyCode::Char('x'), KeyModifiers::NONE)).unwrap();
        assert_eq!(b, b"x");
    }

    #[test]
    fn ctrl_letter_becomes_control_code() {
        let b = encode_key_fallback(key(KeyCode::Char('c'), KeyModifiers::CONTROL)).unwrap();
        assert_eq!(b, vec![0x03]);
    }

    #[test]
    fn alt_prefixes_escape() {
        let b = encode_key_fallback(key(KeyCode::Char('f'), KeyModifiers::ALT)).unwrap();
        assert_eq!(b, vec![0x1b, b'f']);
    }

    #[test]
    fn arrows_and_enter() {
        assert_eq!(
            encode_key_fallback(key(KeyCode::Up, KeyModifiers::NONE)).unwrap(),
            b"\x1b[A"
        );
        assert_eq!(
            encode_key_fallback(key(KeyCode::Enter, KeyModifiers::NONE)).unwrap(),
            b"\r"
        );
    }

    #[test]
    fn cyrillic_survives_as_utf8() {
        let b = encode_key_fallback(key(KeyCode::Char('ж'), KeyModifiers::NONE)).unwrap();
        assert_eq!(b, "ж".as_bytes());
    }
}

/// The letter on the key, whatever the layout prints on it. With `option` held, macOS sends the
/// character the layout produced - physical T types `е` on a Russian keyboard - so a shortcut
/// named after a word would only work on a Latin layout without this.
pub fn latin(c: char) -> char {
    // The Russian layout has no letter on the slash key: it prints a full stop there, so that
    // one is mapped by the character rather than by a letter.
    const RU: [(char, char); 34] = [
        ('й', 'q'), ('ц', 'w'), ('у', 'e'), ('к', 'r'), ('е', 't'), ('н', 'y'),
        ('г', 'u'), ('ш', 'i'), ('щ', 'o'), ('з', 'p'), ('х', '['), ('ъ', ']'),
        ('ф', 'a'), ('ы', 's'), ('в', 'd'), ('а', 'f'), ('п', 'g'), ('р', 'h'),
        ('о', 'j'), ('л', 'k'), ('д', 'l'), ('ж', ';'), ('э', '\''),
        ('я', 'z'), ('ч', 'x'), ('с', 'c'), ('м', 'v'), ('и', 'b'), ('т', 'n'),
        ('ь', 'm'), ('б', ','), ('ю', '.'), ('ё', '`'), ('.', '/'),
    ];
    let lower = c.to_lowercase().next().unwrap_or(c);
    RU.iter().find(|(ru, _)| *ru == lower).map_or(lower, |(_, en)| *en)
}

/// The layouts whose Option layer is read back, by input source id, for the client to look the current one up in.
pub const OPTION_LAYOUTS: [&str; 6] = [
    "com.apple.keylayout.US",
    "com.apple.keylayout.ABC",
    "com.apple.keylayout.USInternational-PC",
    "com.apple.keylayout.Russian",
    "com.apple.keylayout.RussianWin",
    "com.apple.keylayout.Russian-Phonetic",
];

/// A character a layout types with Option, and the key it is typed on.
type Layer = [(char, char)];

/// What a layout prints for Option and a key, read back as Alt and that key (its Latin letter, as `latin` reads it),
/// so a terminal that types the printed character (iTerm2 and kitty by default) still reaches the Option shortcuts.
/// Taken from the layouts themselves (UCKeyTranslate): U.S. and ABC are the same, U.S. International PC has the same
/// Option layer. Left out: dead keys (U.S. Option and e, u, i, n, `), what the layout also types without Option, a
/// character two keys print, and the Shift layer where it repeats the plain one.
/// Also read back: a bare accent typed as dead key and space (´ ¨ ˆ ˜); and —, ∏, ‘ arrive as Alt and _, P, ], which the server takes for the start of a terminal reply, as it does in Ghostty.
pub fn option_layer(layout: &str, k: KeyEvent) -> KeyEvent {
    let (plain, shifted): (&Layer, &Layer) = match layout {
        "com.apple.keylayout.US" | "com.apple.keylayout.ABC" | "com.apple.keylayout.USInternational-PC" => (&US_PLAIN, &US_SHIFTED),
        "com.apple.keylayout.Russian" => (&RU_PLAIN, &RU_SHIFTED),
        "com.apple.keylayout.RussianWin" => (&RU_WIN_PLAIN, &RU_WIN_SHIFTED),
        "com.apple.keylayout.Russian-Phonetic" => (&RU_PHONETIC_PLAIN, &RU_PHONETIC_SHIFTED),
        _ => return k,
    };
    let KeyCode::Char(c) = k.code else { return k };
    if !k.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
        return k;
    }
    let read = |table: &[(char, char)], modifiers| table.iter().find(|(o, _)| *o == c).map(|(_, key)| KeyEvent { code: KeyCode::Char(*key), modifiers, ..k });
    read(plain, KeyModifiers::ALT).or_else(|| read(shifted, KeyModifiers::ALT | KeyModifiers::SHIFT)).unwrap_or(k)
}

const US_PLAIN: [(char, char); 42] = [
    ('¡', '1'), ('™', '2'), ('£', '3'), ('¢', '4'), ('∞', '5'), ('§', '6'), ('¶', '7'), ('•', '8'),
    ('ª', '9'), ('º', '0'), ('–', '-'), ('≠', '='), ('œ', 'q'), ('∑', 'w'), ('®', 'r'), ('†', 't'),
    ('¥', 'y'), ('ø', 'o'), ('π', 'p'), ('“', '['), ('‘', ']'), ('«', '\\'), ('å', 'a'), ('ß', 's'),
    ('∂', 'd'), ('ƒ', 'f'), ('©', 'g'), ('˙', 'h'), ('∆', 'j'), ('˚', 'k'), ('¬', 'l'), ('…', ';'),
    ('æ', '\''), ('Ω', 'z'), ('≈', 'x'), ('ç', 'c'), ('√', 'v'), ('∫', 'b'), ('µ', 'm'), ('≤', ','),
    ('≥', '.'), ('÷', '/'),
];
const US_SHIFTED: [(char, char); 46] = [
    ('⁄', '!'), ('€', '@'), ('‹', '#'), ('›', '$'), ('ﬁ', '%'), ('ﬂ', '^'), ('‡', '&'), ('°', '*'),
    ('·', '('), ('‚', ')'), ('—', '_'), ('±', '+'), ('Œ', 'Q'), ('„', 'W'), ('´', 'E'), ('‰', 'R'),
    ('ˇ', 'T'), ('Á', 'Y'), ('¨', 'U'), ('ˆ', 'I'), ('Ø', 'O'), ('∏', 'P'), ('”', '{'), ('’', '}'),
    ('»', '|'), ('Å', 'A'), ('Í', 'S'), ('Î', 'D'), ('Ï', 'F'), ('˝', 'G'), ('Ó', 'H'), ('Ô', 'J'),
    ('\u{f8ff}', 'K'), ('Ò', 'L'), ('Ú', ':'), ('Æ', '"'), ('¸', 'Z'), ('˛', 'X'), ('Ç', 'C'), ('◊', 'V'),
    ('ı', 'B'), ('˜', 'N'), ('Â', 'M'), ('¯', '<'), ('˘', '>'), ('¿', '?'),
];
const RU_PLAIN: [(char, char); 37] = [
    ('@', '2'), ('#', '3'), ('$', '4'), ('^', '6'), ('&', '7'), ('*', '8'), ('{', '9'), ('}', '0'),
    ('–', '-'), ('»', '='), ('ј', 'q'), ('џ', 'w'), ('ќ', 'e'), ('®', 'r'), ('†', 't'), ('њ', 'y'),
    ('ѓ', 'u'), ('ѕ', 'i'), ('ў', 'o'), ('‘', 'p'), ('ƒ', 'a'), ('ћ', 'd'), ('÷', 'f'), ('©', 'g'),
    ('₽', 'h'), ('°', 'j'), ('љ', 'k'), ('∆', 'l'), ('…', ';'), ('ђ', 'z'), ('≈', 'x'), ('≠', 'c'),
    ('µ', 'v'), ('™', 'n'), ('~', 'm'), ('≤', ','), ('≥', '.'),
];
const RU_SHIFTED: [(char, char); 27] = [
    ('|', '!'), ('£', '#'), ('€', '$'), ('∞', '%'), ('¬', '^'), ('¶', '&'), ('√', '*'), ('\'', '('),
    ('`', ')'), ('—', '_'), ('«', '+'), ('Ј', 'Q'), ('Џ', 'W'), ('Ќ', 'E'), ('Њ', 'Y'), ('Ѓ', 'U'),
    ('Ѕ', 'I'), ('Ў', 'O'), ('’', 'P'), ('”', '{'), ('Ћ', 'D'), ('•', 'J'), ('Љ', 'K'), ('Ђ', 'Z'),
    ('<', '<'), ('>', '>'), ('„', '?'),
];
const RU_WIN_PLAIN: [(char, char); 41] = [
    ('[', '`'), ('@', '2'), ('#', '3'), ('$', '4'), ('©', '5'), ('^', '6'), ('&', '7'), ('₽', '8'),
    ('–', '-'), ('≠', '='), ('ј', 'q'), ('џ', 'w'), ('ў', 'e'), ('ќ', 'r'), ('†', 't'), ('њ', 'y'),
    ('ѓ', 'u'), ('ѕ', 'i'), ('\'', 'o'), ('‘', 'p'), ('“', '['), ('d', 'a'), ('z', 's'), ('ћ', 'd'),
    ('÷', 'f'), ('…', 'g'), ('•', 'h'), ('∆', 'j'), ('љ', 'k'), ('l', 'l'), ('є', '\''), ('ђ', 'z'),
    ('x', 'x'), ('c', 'c'), ('v', 'v'), ('і', 'b'), ('ƒ', 'n'), ('m', 'm'), ('≤', ','), ('≥', '.'),
    ('ї', '/'),
];
const RU_WIN_SHIFTED: [(char, char); 42] = [
    (']', '~'), ('™', '!'), ('§', '@'), ('£', '#'), ('€', '$'), ('®', '%'), ('¬', '^'), ('¶', '&'),
    ('°', '*'), ('{', '('), ('}', ')'), ('—', '_'), ('≈', '+'), ('Ј', 'Q'), ('Џ', 'W'), ('Ў', 'E'),
    ('Ќ', 'R'), ('Њ', 'Y'), ('Ѓ', 'U'), ('Ѕ', 'I'), ('„', 'O'), ('’', 'P'), ('”', '{'), ('|', '|'),
    ('Z', 'S'), ('Ћ', 'D'), ('±', 'F'), ('√', 'G'), ('∞', 'H'), ('µ', 'J'), ('Љ', 'K'), ('L', 'L'),
    ('Є', '"'), ('Ђ', 'Z'), ('X', 'X'), ('C', 'C'), ('V', 'V'), ('І', 'B'), ('M', 'M'), ('<', '<'),
    ('>', '>'), ('Ї', '?'),
];
const RU_PHONETIC_PLAIN: [(char, char); 31] = [
    ('∞', '5'), ('§', '6'), ('¶', '7'), ('•', '8'), ('[', '9'), (']', '0'), ('-', '-'), ('=', '='),
    ('¬', 'q'), ('₽', 'r'), ('ћ', 't'), ('ў', 'u'), ('і', 'i'), ('«', 'o'), ('»', 'p'), ('“', '['),
    ('ѕ', 's'), ('ђ', 'd'), ('ƒ', 'f'), ('ѓ', 'g'), ('ј', 'j'), ('ќ', 'k'), ('љ', 'l'), ('„', ';'),
    ('≈', 'x'), ('©', 'c'), ('√', 'v'), ('њ', 'n'), ('µ', 'm'), ('≤', ','), ('≥', '.'),
];
const RU_PHONETIC_SHIFTED: [(char, char); 22] = [
    ('°', '*'), ('{', '('), ('}', ')'), ('—', '_'), ('+', '+'), ('±', 'Q'), ('®', 'R'), ('Ћ', 'T'),
    ('Ў', 'U'), ('І', 'I'), ('‘', 'O'), ('”', '{'), ('Ѕ', 'S'), ('Ђ', 'D'), ('∆', 'F'), ('Ѓ', 'G'),
    ('Ј', 'J'), ('Ќ', 'K'), ('Љ', 'L'), ('Њ', 'N'), ('…', '>'), ('\\', '?'),
];

/// The same key event with its character read off the key rather than off the layout, so a
/// command spelled `g` is `g` on any keyboard. Case survives: the tree keeps `shift`+`R` and
/// `shift`+`D` deliberately. Call it only where the key is a command - while a query is being
/// typed the character must stay exactly as it was printed.
pub fn normalize(k: KeyEvent) -> KeyEvent {
    let KeyCode::Char(c) = k.code else { return k };
    let mapped = latin(c);
    let mapped = if c.is_uppercase() { mapped.to_ascii_uppercase() } else { mapped };
    KeyEvent { code: KeyCode::Char(mapped), ..k }
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    #[test]
    fn no_key_sends_a_pane_broken_utf8() {
        let mut broken = 0;
        for c in (0x80..=0x10FFFF).filter_map(char::from_u32) {
            for bits in 0..16u8 {
                let mut m = KeyModifiers::NONE;
                for (i, f) in [KeyModifiers::CONTROL, KeyModifiers::SHIFT, KeyModifiers::ALT, KeyModifiers::SUPER].into_iter().enumerate() {
                    if bits & (1 << i) != 0 {
                        m |= f;
                    }
                }
                if encode_key(KeyEvent::new(KeyCode::Char(c), m)).is_some_and(|b| std::str::from_utf8(&b).is_err()) {
                    broken += 1;
                }
            }
        }
        assert_eq!(broken, 0);
    }

    #[test]
    fn control_with_a_russian_letter_is_its_keys_control_code() {
        let ctrl = |c| encode_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
        assert_eq!(ctrl('с'), Some(vec![0x03]), "physical C");
        assert_eq!(ctrl('С'), Some(vec![0x03]), "with Shift the layout prints a capital");
        assert_eq!(ctrl('в'), Some(vec![0x04]), "physical D");
        assert_eq!(ctrl('я'), Some(vec![0x1a]), "physical Z");
        for c in "йцукенгшщзфывапролдячсмить".chars() {
            assert_eq!(ctrl(c), ctrl(latin(c)), "{c} is Control on its key");
        }
        assert_eq!(ctrl('§'), Some("§".as_bytes().to_vec()), "on no letter key: the character itself, without Control");
    }

    #[test]
    fn a_shortcut_finds_its_letter_whatever_the_layout_prints() {
        // pairs a real Russian-layout keyboard sends
        assert_eq!(latin('с'), 'c', "physical C");
        assert_eq!(latin('м'), 'v', "physical V");
        assert_eq!(latin('е'), 't', "physical T");
        assert_eq!(latin('ь'), 'm', "physical M");
        assert_eq!(latin('ы'), 's', "physical S");
        assert_eq!(latin('л'), 'k', "physical K");
        assert_eq!(latin('т'), 'n', "physical N");
        assert_eq!(latin('.'), '/', "physical slash, which a Russian layout prints as a full stop");

        assert_eq!(latin('t'), 't', "a Latin layout passes through");
        assert_eq!(latin('T'), 't', "and case does not matter");
        assert_eq!(latin('Е'), 't', "nor does it in Cyrillic");
        assert_eq!(latin('1'), '1', "digits sit on the same keys everywhere");
        assert!(latin('ы').is_ascii(), "every mapping lands on ascii");
    }

    fn us(k: KeyEvent) -> KeyEvent {
        option_layer("com.apple.keylayout.US", k)
    }

    #[test]
    fn a_us_option_character_is_read_back_as_option_and_its_key() {
        let ev = |c, m| KeyEvent::new(KeyCode::Char(c), m);
        let alt = |c| ev(c, KeyModifiers::ALT);
        assert_eq!(us(ev('†', KeyModifiers::NONE)), alt('t'));
        assert_eq!(us(ev('ç', KeyModifiers::NONE)), alt('c'));
        assert_eq!(us(ev('∑', KeyModifiers::NONE)), alt('w'));
        assert_eq!(us(ev('÷', KeyModifiers::NONE)), alt('/'));
        for (c, digit) in "¡™£¢∞§¶•ª".chars().zip('1'..='9') {
            assert_eq!(us(ev(c, KeyModifiers::NONE)), alt(digit), "Option and {digit}");
        }
        assert_eq!(us(ev('ˇ', KeyModifiers::SHIFT)), ev('T', KeyModifiers::ALT | KeyModifiers::SHIFT), "the shifted layer");
        assert_eq!(us(ev('¿', KeyModifiers::NONE)), ev('?', KeyModifiers::ALT | KeyModifiers::SHIFT));
        assert_eq!(option_layer("com.apple.keylayout.ABC", ev('†', KeyModifiers::NONE)), alt('t'), "ABC is the same layout");
    }

    #[test]
    fn a_russian_option_character_is_read_back_as_option_and_the_key_under_it() {
        let ev = |c, m| KeyEvent::new(KeyCode::Char(c), m);
        let alt = |c| ev(c, KeyModifiers::ALT);
        let win = |k| option_layer("com.apple.keylayout.RussianWin", k);
        assert_eq!(win(ev('ѓ', KeyModifiers::NONE)), alt('u'), "Option and U on Russian - PC");
        assert_eq!(win(ev('Ѓ', KeyModifiers::SHIFT)), ev('U', KeyModifiers::ALT | KeyModifiers::SHIFT));
        assert_eq!(win(ev('†', KeyModifiers::NONE)), alt('t'));
        assert_eq!(win(ev('@', KeyModifiers::NONE)), alt('2'), "the cost: Option and 2 is how this layout types @");
        assert_eq!(option_layer("com.apple.keylayout.Russian", ev('ѓ', KeyModifiers::NONE)), alt('u'));
        assert_eq!(option_layer("com.apple.keylayout.Russian-Phonetic", ev('ѓ', KeyModifiers::NONE)), alt('g'), "phonetic: on the G key");
    }

    #[test]
    fn typing_is_never_read_as_option() {
        let ev = |c, m| KeyEvent::new(KeyCode::Char(c), m);
        let typed = (' '..='~').chain("йцукенЖЁśłąüéñ".chars());
        for c in typed {
            for m in [KeyModifiers::NONE, KeyModifiers::SHIFT] {
                assert_eq!(us(ev(c, m)), ev(c, m), "{c:?} is typed as it is");
            }
        }
        let russian = "йцукенгшщзхъфывапролджэячсмитьбюёЙЦУКЕНГШЩЗХЪФЫВАПРОЛДЖЭЯЧСМИТЬБЮЁ0123456789";
        for layout in ["com.apple.keylayout.Russian", "com.apple.keylayout.RussianWin", "com.apple.keylayout.Russian-Phonetic"] {
            for c in russian.chars() {
                assert_eq!(option_layer(layout, ev(c, KeyModifiers::NONE)), ev(c, KeyModifiers::NONE), "{c:?} on {layout}");
            }
        }
        assert_eq!(option_layer("com.apple.keylayout.German", ev('†', KeyModifiers::NONE)), ev('†', KeyModifiers::NONE), "a layout not read back");
        for m in [KeyModifiers::CONTROL, KeyModifiers::ALT, KeyModifiers::SUPER] {
            assert_eq!(us(ev('†', m)), ev('†', m), "another modifier: not the Option layer");
        }
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(us(enter), enter);
    }

    #[test]
    fn every_option_key_is_its_own_and_dead_keys_are_left_out() {
        let ev = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let counts = [88, 88, 88, 64, 83, 53];
        for (layout, count) in OPTION_LAYOUTS.into_iter().zip(counts) {
            let mut seen = std::collections::HashSet::new();
            for c in (0x21..0x10000u32).filter_map(char::from_u32) {
                let k = option_layer(layout, ev(c));
                if k != ev(c) {
                    let KeyCode::Char(base) = k.code else { panic!("{c:?} became {k:?}") };
                    assert!(base.is_ascii_graphic(), "{c:?} lands on a key, {base:?}");
                    assert!(seen.insert((base, k.modifiers)), "{base:?} {:?} comes from two characters on {layout}", k.modifiers);
                }
            }
            assert_eq!(seen.len(), count, "{layout}");
            if layout.contains(".US") || layout.ends_with(".ABC") {
                for dead in ['e', 'u', 'i', 'n', '`'] {
                    assert!(!seen.contains(&(dead, KeyModifiers::ALT)), "Option and {dead} is a dead key on {layout}");
                }
            }
        }
    }

    #[test]
    fn a_command_key_is_read_off_the_key_and_keeps_its_case() {
        let ev = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        assert_eq!(normalize(ev('п')).code, KeyCode::Char('g'), "physical G");
        assert_eq!(normalize(ev('П')).code, KeyCode::Char('G'), "and shifted, for the keys that need it");
        assert_eq!(normalize(ev('.')).code, KeyCode::Char('/'), "the slash key, which prints a full stop");
        assert_eq!(normalize(ev('r')).code, KeyCode::Char('r'), "a Latin layout is left alone");
        assert_eq!(normalize(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).code, KeyCode::Enter);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    NewTab,
    ClosePane,
    SplitDown,
    SplitSide,
    Zoom,
    Sidebar,
    Update,
    Detach,
}

/// What a global shortcut does: perform something, or open one of the six windows `fkey` knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bind {
    Do(KeyAction),
    Open(u8),
}

/// Every `option` shortcut, once. The dispatcher looks the letter up here, the menus take the
/// label beside their item from here, and the keys screen prints its global section from here,
/// so a shortcut is added by adding a row and nothing else.
pub struct Global {
    pub letter: char,
    pub label: &'static str,
    pub what: &'static str,
    pub bind: Bind,
}

pub const GLOBAL: &[Global] = &[
    Global { letter: 'k', label: "⌥+K", what: "keys and mouse", bind: Bind::Open(1) },
    Global { letter: 'm', label: "⌥+M", what: "main menu", bind: Bind::Open(2) },
    // the key that already means "find" in the tree and the lists
    Global { letter: '/', label: "⌥+/", what: "search in the pane; again to close", bind: Bind::Open(3) },
    Global { letter: 't', label: "⌥+T", what: "show or hide the file tree", bind: Bind::Open(4) },
    Global { letter: 'n', label: "⌥+N", what: "navigator", bind: Bind::Open(5) },
    // settings, where macOS keeps preferences
    Global { letter: ',', label: "⌥+,", what: "settings", bind: Bind::Open(6) },
    Global { letter: 'c', label: "⌥+C", what: "new tab", bind: Bind::Do(KeyAction::NewTab) },
    Global { letter: 'w', label: "⌥+W", what: "close the pane, asking first", bind: Bind::Do(KeyAction::ClosePane) },
    Global { letter: '-', label: "⌥+-", what: "split the pane down", bind: Bind::Do(KeyAction::SplitDown) },
    Global { letter: '\\', label: "⌥+\\", what: "split the pane right", bind: Bind::Do(KeyAction::SplitSide) },
    Global { letter: 'z', label: "⌥+Z", what: "full screen for the pane; again to leave", bind: Bind::Do(KeyAction::Zoom) },
    Global { letter: 's', label: "⌥+S", what: "collapse or expand the sidebar", bind: Bind::Do(KeyAction::Sidebar) },
    Global { letter: 'u', label: "⌥+U", what: "update to the installed build; panes keep running", bind: Bind::Do(KeyAction::Update) },
    Global { letter: 'q', label: "⌥+Q", what: "detach, asking first; the server keeps running", bind: Bind::Do(KeyAction::Detach) },
];

pub fn global_of(c: char) -> Option<&'static Global> {
    GLOBAL.iter().find(|g| g.letter == c)
}

/// The label a menu item shows beside its name, taken from the same row the key dispatches on.
pub fn global_label(a: KeyActionName) -> &'static str {
    GLOBAL
        .iter()
        .find(|g| match (g.bind, a) {
            (Bind::Do(d), KeyActionName::Act(want)) => d == want,
            (Bind::Open(n), KeyActionName::Surface(want)) => n == want,
            _ => false,
        })
        .map_or("", |g| g.label)
}

/// Which row a caller wants the label of: an action, or one of the numbered surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyActionName {
    Act(KeyAction),
    Surface(u8),
}

/// Every letter `option` claims, for the terminal that has to be told about them.
pub const OPTION_LETTERS: &[char] = &['k', 'm', '/', 't', 'n', ',', 'c', 'w', '-', '\\', 'z', 's', 'u', 'q'];

#[cfg(test)]
mod global_tests {
    use super::*;

    #[test]
    fn the_list_of_letters_and_the_table_cannot_drift() {
        for &c in OPTION_LETTERS {
            assert!(global_of(c).is_some(), "{c:?} is listed as ours but the table has no row for it");
        }
        for g in GLOBAL {
            assert!(OPTION_LETTERS.contains(&g.letter), "{:?} is in the table but not in the list", g.letter);
        }
        assert_eq!(GLOBAL.len(), OPTION_LETTERS.len());
    }

    #[test]
    fn a_menu_item_takes_its_key_from_the_row_that_dispatches_it() {
        assert_eq!(global_label(KeyActionName::Act(KeyAction::Sidebar)), "⌥+S");
        assert_eq!(global_label(KeyActionName::Surface(3)), "⌥+/");
        assert_eq!(global_label(KeyActionName::Surface(1)), "⌥+K");
    }

    #[test]
    fn every_action_letter_is_the_one_the_chord_used_to_carry() {
        use KeyAction::*;
        let bind = |c: char| global_of(c).map(|g| g.bind);
        for (c, want) in [('c', NewTab), ('w', ClosePane), ('-', SplitDown), ('\\', SplitSide),
                          ('z', Zoom), ('s', Sidebar), ('u', Update), ('q', Detach)] {
            assert_eq!(bind(c), Some(Bind::Do(want)), "⌥+{c:?}");
        }
        // the same physical keys on a Russian layout
        for (c, want) in [('с', NewTab), ('ц', ClosePane), ('я', Zoom), ('ы', Sidebar), ('г', Update), ('й', Detach)] {
            assert_eq!(bind(crate::keys::latin(c)), Some(Bind::Do(want)), "⌥+{c:?}");
        }
        assert!(global_of('y').is_none(), "a letter bound to nothing stays unbound");
    }
}
