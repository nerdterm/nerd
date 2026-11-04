use super::*;
use crate::term::{SLICE, Term};
use alacritty_terminal::grid::Dimensions;

/// Seeded xorshift: every run sees the same streams.
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }
    pub(crate) fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    pub(crate) fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Output shaped like what panes print: a shell, vim, an agent's synchronized frames, ncurses REP, a pager;
/// then every recorded file under tests/fixtures/terminal/.
pub(crate) fn corpus() -> Vec<(String, Vec<u8>)> {
    let mut out = vec![
        ("shell".to_string(), shell()),
        ("vim".to_string(), vim()),
        ("agent".to_string(), agent(120)),
        ("agent-wide".to_string(), agent(600)),
        ("rep".to_string(), rep()),
        ("pager".to_string(), pager()),
    ];
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/terminal");
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let mut files: Vec<_> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "bin")).collect();
        files.sort();
        for p in files {
            out.push((p.file_name().unwrap().to_string_lossy().into_owned(), std::fs::read(&p).unwrap()));
        }
    }
    out
}

fn shell() -> Vec<u8> {
    let mut s = String::new();
    for i in 0..200 {
        s += "\x1b[1;32muser@host\x1b[0m:\x1b[34m~/project\x1b[0m$ ls -G\r\n";
        s += &format!("\x1b[1;34mdir{i}\x1b[39;49m\x1b[0m\t\x1b[31mexec{i}\x1b[39;49m\x1b[0m\tplain{i}.txt\t\x1b[35mlink\x1b[39;49m\x1b[0m\r\n");
        s += &format!("* \x1b[33mcommit {i:040x}\x1b[m\r\n|\\  \r\n| * \x1b[1;31m(branch)\x1b[m message é 中文 {i}\r\n");
        s += "\x1b]0;user@host: ~/project\x07";
    }
    s.into_bytes()
}

fn vim() -> Vec<u8> {
    let mut s = String::from("\x1b[22;2t\x1b[22;1t\x1b]2;file.rs - VIM\x07\x1b[?1049h\x1b[22;0;0t\x1b[?1h\x1b=\x1b[H\x1b[2J\x1b[1;39r");
    for frame in 0..30 {
        s += "\x1b[?25l";
        for row in 1..=38 {
            s += &format!("\x1b[{row};1H\x1b[33m{:>3} \x1b[m\x1b[38;5;{}mfn line_{frame}_{row}() {{ é }}\x1b[K", row, (row * 7) % 256);
        }
        s += &format!("\x1b[39;1H\x1b[7m-- INSERT -- {frame}\x1b[m\x1b[1;1H\x1bM\x1b[2;3H\x1b[?25h");
    }
    s += "\x1b[39;1H\x1b[K\x1b[r\x1b[?1l\x1b>\x1b[?1049l\x1b[23;0;0t\x1b[23;2t\x1b[23;1t";
    s.into_bytes()
}

fn agent(cols: usize) -> Vec<u8> {
    let mut s = String::from("\x1b[?2004h\x1b[?1004h\x1b]0;\u{2733} Claude Code\x07");
    for frame in 0..60 {
        s += "\x1b[?2026h\x1b[?25l";
        if frame > 0 {
            s += "\x1b[12A";
        }
        for row in 0..12 {
            let text = format!("\u{25cf} step {frame}.{row} \u{1f600} 中 a\u{301}\u{302} \x1b]8;;https://example.com/{row}\x07link\x1b]8;;\x07");
            s += &format!("\x1b[2K\x1b[38;2;{};{};200m{text}\x1b[39m{}\r\n", row * 20, frame * 4, " ".repeat(cols.saturating_sub(60) % 97));
        }
        s += "\x1b[?25h\x1b[6n\x1b[?2026l";
        s += &format!("\x1b]0;\u{2802} Working {frame}\x07");
    }
    s.into_bytes()
}

fn rep() -> Vec<u8> {
    let mut s = String::new();
    for i in 1..60 {
        s += &format!("x\x1b[{}b\r\n=\x1b[{}b|\r\n", i, 79 - i);
    }
    s.into_bytes()
}

fn pager() -> Vec<u8> {
    let mut s = String::from("\x1b[?1049h\x1b[H\x1b[2J\x1b[3g");
    for c in (1..80).step_by(8) {
        s += &format!("\x1b[1;{c}H\x1bH");
    }
    for i in 0..300 {
        s += &format!("\x1b[24;1H\x1b[S\x1b[23;1Ha\tb\tc {i}\x1b[7m:\x1b[27m\x1b[2T\x1b[3S\x1b[1;1H\x1b[2L\x1b[3M\x1b[4X\x1b[5P\x1b[3@\x1b[2Z\x1b[3I");
    }
    s += "\x1b[?1049l";
    s.into_bytes()
}

#[derive(Debug, PartialEq, Clone)]
enum Call {
    Print(char),
    Exec(u8),
    Hook(Vec<Vec<u16>>, Vec<u8>, bool, char),
    Put(u8),
    Unhook,
    Osc(Vec<Vec<u8>>, bool),
    Csi(Vec<Vec<u16>>, Vec<u8>, bool, char),
    Esc(Vec<u8>, bool, u8),
}

/// vte's own parser, recording each call with the offset of the byte that made it.
#[derive(Default)]
struct Rec {
    calls: Vec<(usize, Call)>,
    at: usize,
}

impl alacritty_terminal::vte::Perform for Rec {
    fn print(&mut self, c: char) {
        self.calls.push((self.at, Call::Print(c)));
    }
    fn execute(&mut self, b: u8) {
        self.calls.push((self.at, Call::Exec(b)));
    }
    fn hook(&mut self, p: &alacritty_terminal::vte::Params, i: &[u8], ignore: bool, a: char) {
        self.calls.push((self.at, Call::Hook(p.iter().map(<[u16]>::to_vec).collect(), i.to_vec(), ignore, a)));
    }
    fn put(&mut self, b: u8) {
        self.calls.push((self.at, Call::Put(b)));
    }
    fn unhook(&mut self) {
        self.calls.push((self.at, Call::Unhook));
    }
    fn osc_dispatch(&mut self, p: &[&[u8]], bell: bool) {
        self.calls.push((self.at, Call::Osc(p.iter().map(|x| x.to_vec()).collect(), bell)));
    }
    fn csi_dispatch(&mut self, p: &alacritty_terminal::vte::Params, i: &[u8], ignore: bool, a: char) {
        self.calls.push((self.at, Call::Csi(p.iter().map(<[u16]>::to_vec).collect(), i.to_vec(), ignore, a)));
    }
    fn esc_dispatch(&mut self, i: &[u8], ignore: bool, b: u8) {
        self.calls.push((self.at, Call::Esc(i.to_vec(), ignore, b)));
    }
}

/// vte's calls for `bytes`, fed one byte at a time.
fn oracle(bytes: &[u8]) -> Vec<(usize, Call)> {
    let mut parser = alacritty_terminal::vte::Parser::new();
    let mut rec = Rec::default();
    for (i, b) in bytes.iter().enumerate() {
        rec.at = i + 1;
        parser.advance(&mut rec, std::slice::from_ref(b));
    }
    rec.calls
}

fn filter(bytes: &[u8], cols: u16) -> (Vec<u8>, Vec<(usize, bool)>, u8) {
    let (mut out, mut marks, mut hits) = (Vec::new(), Vec::new(), 0);
    Filter::default().pass(bytes, cols, &mut hits, &mut out, &mut marks);
    (out, marks, hits)
}

/// What the caps promise of vte's calls on the filter's output.
fn assert_bounded(raw: &[u8], cols: u16, name: &str) {
    let (out, marks, _) = filter(raw, cols);
    let calls = oracle(&out);
    let mut want = Vec::new();
    for (at, c) in &calls {
        match c {
            Call::Osc(p, _) => assert!(p.iter().map(Vec::len).sum::<usize>() <= OSC_BYTES as usize, "{name}: an OSC of {} bytes", p.iter().map(Vec::len).sum::<usize>()),
            Call::Csi(p, i, false, 'b') if i.is_empty() => {
                let n = p.first().and_then(|x| x.first()).copied().unwrap_or(0).max(1);
                assert!(n <= cols, "{name}: REP {n} at {cols} columns");
            }
            Call::Csi(p, i, false, a @ ('h' | 'l')) if i == b"?" && p.iter().any(|x| x[0] == SYNC) => want.push((*at, *a == 'h')),
            _ => {}
        }
    }
    assert_eq!(marks, want, "{name}: the marks are vte's own begins and ends");
}

const ALPHABET: &[&[u8]] = &[
    b"\x1b", b"[", b"]", b"P", b"_", b"X", b"#", b"8", b"\\", b"\x07", b"\x18", b"\x1a", b"\x9c", b"\x7f", b"\x80", b"\xbf", b"\xc3",
    b"\xa9", b"\xf0", b";", b":", b"?", b">", b" ", b"0", b"1", b"5", b"9", b"2026", b"b", b"Z", b"h", b"l", b"t", b"a", b"\n",
];

/// A stream of the alphabet with, unless `tame`, the long and over-cap pieces mixed in.
fn stream(rng: &mut Rng, tame: bool) -> Vec<u8> {
    let mut s = Vec::new();
    for _ in 0..rng.below(400) {
        let pick = rng.below(if tame { 110 } else { 120 });
        match pick {
            0..100 => {
                let t = ALPHABET[rng.below(ALPHABET.len())];
                if !(tame && t == b"b") {
                    s.extend_from_slice(t);
                }
            }
            100 => s.extend_from_slice(b"\x1b[?2026h"),
            101 => s.extend_from_slice(b"\x1b[?2026l"),
            102 => s.extend_from_slice(b"\x1b[?1049;2026h"),
            103 => s.extend_from_slice(format!("\x1b[{}b", 1 + rng.below(80)).as_bytes()),
            104 => s.extend_from_slice(b"\x1b]0;title\x07"),
            105 => s.extend_from_slice("é中\u{301}".as_bytes()),
            106 => s.extend_from_slice(b"\x1b[?2026:1h"),
            107 => s.extend_from_slice(b"\x1b[>2026h"),
            108 => s.extend_from_slice(b"\x1b[\x05?2026l"),
            109 => s.extend_from_slice(&[b';'; 33][..rng.below(34)]),
            110 => s.extend_from_slice(format!("\x1b[{}b", 81 + rng.below(70000)).as_bytes()),
            111 => s.extend(std::iter::repeat_n(b'7', rng.below(40))),
            112 => s.extend("\x1b]0;".bytes().chain("é".repeat(4000 + rng.below(200)).bytes())),
            113 => s.extend(b"\x1b]".iter().copied().chain(std::iter::repeat_n(0x80, 16300 + rng.below(200)))),
            114 => s.extend(b"\x1b]".iter().copied().chain(std::iter::repeat_n(0x9c, 16300 + rng.below(200)))),
            115 => s.extend(b"\x1b]0;\xf0".iter().copied().chain(std::iter::repeat_n(0xbf, 17000))),
            116 => s.extend("\x1b]2;".bytes().chain("\u{1f600}".repeat(4090 + rng.below(10)).bytes())),
            117 => s.extend_from_slice(b"a\x1b[65535b"),
            118 => s.extend_from_slice(b"\x1b[0065535;9b"),
            _ => s.extend_from_slice(b"\x1b[3g\x1b[80G\x1b[65535Z"),
        }
    }
    s.push(0x07);
    s
}

#[test]
fn tripwire_the_bounds_mirror_vte_0_15_and_alacritty_0_26() {
    let lock = include_str!("../../../Cargo.lock");
    let why = "Filter mirrors vte 0.15, NoSync relies on its Processor never buffering without a pending timeout, and Guard forwards its 71 Handler methods and keeps alacritty's title stack empty (unsafe with kitty_keyboard, alacritty term/mod.rs:1296): re-check all four, then update this test";
    assert!(lock.contains("name = \"vte\"\nversion = \"0.15.0\"") && lock.matches("name = \"vte\"\n").count() == 1, "{why}");
    assert!(lock.contains("name = \"alacritty_terminal\"\nversion = \"0.26.0\"") && lock.matches("name = \"alacritty_terminal\"\n").count() == 1, "{why}");
    assert!(lock.contains("name = \"unicode-width\"\nversion = \"0.2.2\"") && lock.matches("name = \"unicode-width\"\n").count() == 1, "Guard counts zero-width chars with alacritty's own width table: {why}");
    assert!(!alacritty_terminal::term::Config::default().kitty_keyboard, "{why}");
}

#[test]
fn the_guard_answers_every_handler_method_of_vte_0_15() {
    // vte 0.15 ansi.rs:495-732, in order.
    const HANDLER: &[&str] = &[
        "set_title", "set_cursor_style", "set_cursor_shape", "input", "goto", "goto_line", "goto_col", "insert_blank", "move_up", "move_down",
        "identify_terminal", "device_status", "move_forward", "move_backward", "move_down_and_cr", "move_up_and_cr", "put_tab", "backspace",
        "carriage_return", "linefeed", "bell", "substitute", "newline", "set_horizontal_tabstop", "scroll_up", "scroll_down",
        "insert_blank_lines", "delete_lines", "erase_chars", "delete_chars", "move_backward_tabs", "move_forward_tabs",
        "save_cursor_position", "restore_cursor_position", "clear_line", "clear_screen", "clear_tabs", "set_tabs", "reset_state",
        "reverse_index", "terminal_attribute", "set_mode", "unset_mode", "report_mode", "set_private_mode", "unset_private_mode",
        "report_private_mode", "set_scrolling_region", "set_keypad_application_mode", "unset_keypad_application_mode",
        "set_active_charset", "configure_charset", "set_color", "dynamic_color_sequence", "reset_color", "clipboard_store",
        "clipboard_load", "decaln", "push_title", "pop_title", "text_area_size_pixels", "text_area_size_chars", "set_hyperlink",
        "set_mouse_cursor_icon", "report_keyboard_mode", "push_keyboard_mode", "pop_keyboard_modes", "set_keyboard_mode",
        "set_modify_other_keys", "report_modify_other_keys", "set_scp",
    ];
    assert_eq!(HANDLER.len(), 71);
    let mut ours: Vec<&str> = FORWARDED.iter().chain(OWN).copied().collect();
    let mut theirs = HANDLER.to_vec();
    ours.sort_unstable();
    theirs.sort_unstable();
    assert_eq!(ours, theirs);
}

#[test]
fn vte_dispatches_no_osc_or_repeat_past_the_caps_and_the_marks_are_its_own() {
    let mut rng = Rng::new(7);
    for i in 0..2000 {
        let raw = stream(&mut rng, false);
        assert_bounded(&raw, 80, &format!("stream {i}"));
    }
    for (name, raw) in corpus() {
        assert_bounded(&raw, 80, &name);
    }
}

#[test]
fn output_under_the_caps_reaches_vte_unchanged() {
    let mut rng = Rng::new(11);
    for i in 0..2000 {
        let raw = stream(&mut rng, true);
        let (out, _, hits) = filter(&raw, 80);
        assert_eq!(hits, 0, "stream {i}");
        assert_eq!(oracle(&out), oracle(&raw), "stream {i}");
    }
    for (name, raw) in corpus() {
        let (out, _, hits) = filter(&raw, 80);
        assert_eq!((hits, out == raw), (0, true), "{name}");
    }
}

#[test]
fn a_stream_split_anywhere_filters_as_it_does_whole() {
    let mut rng = Rng::new(3);
    let mut raw = Vec::new();
    while raw.len() < 2048 {
        raw.extend(stream(&mut rng, false).into_iter().take(300));
    }
    raw.truncate(2048);
    let whole = filter(&raw, 80);
    for at in 0..=raw.len() {
        let (mut out, mut marks, mut hits) = (Vec::new(), Vec::new(), 0);
        let mut f = Filter::default();
        f.pass(&raw[..at], 80, &mut hits, &mut out, &mut marks);
        f.pass(&raw[at..], 80, &mut hits, &mut out, &mut marks);
        assert_eq!((out, marks, hits), whole, "split at {at}");
    }
}

#[test]
fn a_repeat_wider_than_the_pane_is_cut_to_its_width() {
    for v in [&b"x\x1b[65535b"[..], b"\x1b[6\n5535b", b"\x1b[0065535b", b"\x1b[65535:1b", b"\x1b[65535;9b", b"\x1b[999999b"] {
        let (out, _, hits) = filter(v, 80);
        let mut want = v[..v.len() - 1].to_vec();
        want.extend_from_slice(b"\x18\x1b[80b");
        assert_eq!((String::from_utf8_lossy(&out), hits), (String::from_utf8_lossy(&want), HIT_REP), "{v:?}");
    }
    let semis = format!("\x1b[99999{}b", ";".repeat(32));
    for v in [&b"\x1b[?65535b"[..], b"\x1b[ 65535b", b"\x1b[>65535b", semis.as_bytes(), b"\x1b[80b", b"\x1b[0b", b"\x1b[3g\x1b[80G\x1b[65535Z"] {
        assert_eq!(filter(v, 80), (v.to_vec(), vec![], 0), "{v:?}");
    }
}

fn osc_payload(out: &[u8]) -> Vec<Vec<u8>> {
    oracle(out).into_iter().filter_map(|(_, c)| if let Call::Osc(p, _) = c { Some(p) } else { None }).next().unwrap_or_default()
}

#[test]
fn an_osc_keeps_its_first_4096_characters_and_never_more_than_16_kib() {
    for pre in [&b"\x1b\x7f]"[..], b"\x1b\n]", b"\x1b\x1b]", b"\x1b\xc3]"] {
        let raw: Vec<u8> = pre.iter().copied().chain(b"0;".iter().copied()).chain(std::iter::repeat_n(b'x', 5000)).chain([7]).collect();
        let (out, _, hits) = filter(&raw, 80);
        assert_eq!(hits, HIT_OSC, "{pre:?} opens an OSC");
        assert_eq!(osc_payload(&out)[1].len(), 4094, "{pre:?}");
    }
    let title: Vec<u8> = b"\x1b]0;".iter().copied().chain("é".repeat(1 << 19).into_bytes()).chain([7]).collect();
    let p = osc_payload(&filter(&title, 80).0);
    assert_eq!(String::from_utf8(p[1].clone()).unwrap().chars().count(), 4094, "`0;` counts toward the 4096");
    let c1 = b"\x1b]0;a\x9cb\x07";
    assert_eq!(osc_payload(&filter(c1, 80).0), vec![b"0".to_vec(), b"a\x9cb".to_vec()], "0x9C does not end it");
    for body in [vec![0x80; 1 << 20], [vec![0xf0], vec![0x80; 1 << 20]].concat(), vec![0x9c; 1 << 20]] {
        for end in [&[][..], &[7]] {
            let raw: Vec<u8> = b"\x1b]".iter().chain(&body).chain(end).copied().collect();
            let (out, _, hits) = filter(&raw, 80);
            assert_eq!(hits, HIT_OSC);
            assert!(out.len() <= 2 + OSC_BYTES as usize + 1, "{} bytes passed", out.len());
            if !end.is_empty() {
                assert!(osc_payload(&out).iter().map(Vec::len).sum::<usize>() <= OSC_BYTES as usize);
            }
        }
    }
    let wide: Vec<u8> = b"\x1b]".iter().copied().chain("\u{1f600}".repeat(4096).into_bytes()).chain([7]).collect();
    assert_eq!(filter(&wide, 80), (wide.clone(), vec![], 0), "4096 four-byte characters pass whole");
}

#[test]
fn a_synchronized_update_is_marked_where_vte_begins_and_ends_it() {
    let semis = |n: usize| format!("\x1b[?2026{}h", ";".repeat(n));
    let begins = [b"\x1b[?2026h".to_vec(), b"\x1b[?1049;2026h".to_vec(), b"\x1b[?2026:1h".to_vec(), b"\x1b]0;t\x1b[?2026h".to_vec(), b"\x1b[\x05?2026h".to_vec(), semis(31).into_bytes()];
    for v in begins {
        assert_eq!(filter(&v, 80).1, vec![(v.len(), true)], "{:?}", String::from_utf8_lossy(&v));
    }
    for v in [&b"\x1b[?2026l"[..], b"\x1b[?1;2026l"] {
        assert_eq!(filter(v, 80).1, vec![(v.len(), false)], "{v:?}");
    }
    for v in [b"\x1b[2026h".to_vec(), b"\x1b[>2026h".to_vec(), b"\x1b[? 2026h".to_vec(), b"\x1b[?20260h".to_vec(), b"\x1b[?67562h".to_vec(), semis(32).into_bytes(), b"\x1bP?2026h\x1b\\".to_vec()] {
        assert_eq!(filter(&v, 80).1, vec![], "{:?}", String::from_utf8_lossy(&v));
    }
}

fn marks_on(t: &Term, row: i32, col: usize) -> usize {
    t.term.grid()[alacritty_terminal::index::Line(row)][Column(col)].zerowidth().map_or(0, <[char]>::len)
}

#[test]
fn a_cell_keeps_at_most_30_zero_width_characters() {
    let m = "\u{301}";
    let cases = [
        (format!("a{}", m.repeat(1000)), 0),
        (format!("中{}", m.repeat(1000)), 0),
        (format!("\x1b[80Ga{}", m.repeat(1000)), 79),
        (format!("a{}", format!("{}\x1b[2G", m.repeat(100)).repeat(10)), 0),
        (format!("a{m}\x1b[65535b"), 0),
        (format!("a{m}\x1b[m\x1b[65535b"), 0),
    ];
    for (bytes, col) in cases {
        let mut t = Term::new(80, 5, 10);
        t.feed(bytes.as_bytes());
        assert_eq!(marks_on(&t, 0, col), ZERO_WIDTH, "{:?}", bytes.chars().take(8).collect::<String>());
        assert_eq!(t.line(0).unwrap().chars().filter(|c| *c != ' ').count(), 31);
        assert_eq!(t.take_hits() & HIT_MARKS, HIT_MARKS);
    }
}

#[test]
fn a_character_cut_between_chunks_loses_nothing_after_it() {
    for (chunks, want) in [(&[&b"\xd1"[..], b"\x82 \xd0\xbc"][..], "т м"), (&[&b"\xf0"[..], b"\x9f", b"\x98\x80 x\xd0\xbc"][..], "\u{1f600} xм")] {
        let mut t = Term::new(80, 5, 10);
        for c in chunks {
            t.take(c);
            t.parse(None);
        }
        assert_eq!(t.line(0).unwrap().trim_end(), want);
    }
}

#[test]
fn titles_nest_eight_deep_dropping_the_oldest() {
    let mut t = Term::new(80, 5, 10);
    for c in 'A'..='T' {
        t.feed(format!("\x1b]2;{c}\x07\x1b[22t").as_bytes());
    }
    assert_eq!(t.titles.depth(), 8);
    let mut seen = Vec::new();
    for _ in 0..20 {
        t.feed(b"\x1b[23t");
        seen.push(t.title().unwrap().to_string());
    }
    let want: Vec<String> = ('M'..='T').rev().map(String::from).chain(std::iter::repeat_n("M".to_string(), 12)).collect();
    assert_eq!(seen, want);
    t.feed(b"\x1b]2;x\x07\x1b[22t\x1b[22t\x1bc");
    assert_eq!(t.titles.depth(), 0, "a reset empties the stack");
    let before = t.title().map(str::to_string);
    t.feed(b"\x1b[23t");
    assert_eq!(t.title().map(str::to_string), before, "and a pop after it changes nothing");
}

#[test]
fn links_are_not_kept_in_cells() {
    let mut t = Term::new(80, 24, 100);
    let link = format!("\x1b]8;;{}\x07x", "u".repeat(4096));
    for _ in 0..1000 {
        t.feed(link.as_bytes());
    }
    let g = t.term.grid();
    for l in -(g.history_size() as i32)..g.screen_lines() as i32 {
        for c in 0..g.columns() {
            assert!(g[alacritty_terminal::index::Line(l)][Column(c)].hyperlink().is_none());
        }
    }
}

#[test]
fn vims_title_save_and_restore_is_as_before() {
    let mut a = Term::new(120, 40, 100);
    let mut b = Term::unbounded(120, 40, 100);
    for t in [&mut a, &mut b] {
        t.feed(b"\x1b]2;shell\x07");
        t.feed(&vim());
    }
    assert_eq!(a.title(), b.title());
    assert_eq!(a.title(), Some("shell"));
}

#[test]
fn a_back_tab_scans_the_row_once() {
    let back_tabs = |bytes: &[u8]| {
        let (mut a, mut b) = (Term::new(600, 5, 0), Term::unbounded(600, 5, 0));
        BACK_TABS.with(|n| n.set(0));
        a.feed(bytes);
        b.feed(bytes);
        assert_eq!(a.full_state(), b.full_state(), "{bytes:?}");
        (BACK_TABS.with(|n| n.get()), a.screen().cursor_position().1)
    };
    let (passes, _) = back_tabs(b"\x1b[3g\x1b[600G\x1b[65535Z");
    assert!(passes <= 2, "{passes} passes with no stop to the left");
    let (passes, col) = back_tabs(b"\x1b[600G\x1b[65535Z");
    assert!(passes <= 76 && col == 0, "{passes} passes, column {col}");
}

/// Where two `full_state`s part: the row and the cell, or the line after the grid.
fn first_difference(a: &str, b: &str) -> String {
    for (row, (x, y)) in a.lines().zip(b.lines()).enumerate() {
        if x != y {
            let cell = x.split('|').zip(y.split('|')).enumerate().find(|(_, (p, q))| p != q);
            return format!("row {row}: {cell:?}");
        }
    }
    "none".into()
}

/// Both Terms fed `raw`, the bounded one in `pieces` and parsed in slices of `slice`.
fn differ(raw: &[u8], cols: u16, rows: u16, cuts: &[usize], slice: usize) -> (String, String) {
    let mut a = Term::new(cols, rows, 1000);
    let mut b = Term::unbounded(cols, rows, 1000);
    let mut from = 0;
    for &to in cuts.iter().chain([&raw.len()]) {
        a.take(&raw[from..to]);
        while !a.parse_in(None, slice) {}
        from = to;
    }
    b.feed(raw);
    a.finish();
    b.finish();
    (a.full_state(), b.full_state())
}

#[test]
fn every_corpus_stream_leaves_the_same_state_whole_split_and_sliced() {
    for (name, raw) in corpus() {
        for (cols, rows) in [(120, 40), (80, 24)] {
            let (a, b) = differ(&raw, cols, rows, &[], SLICE);
            assert!(a == b, "{name} at {cols}x{rows}, whole");
            let mut rng = Rng::new(raw.len() as u64);
            let mut cuts: Vec<usize> = (0..64).map(|_| rng.below(raw.len())).collect();
            cuts.sort_unstable();
            let (a, b) = differ(&raw, cols, rows, &cuts, SLICE);
            assert!(a == b, "{name} at {cols}x{rows}, split at 64 points");
            for slice in [1, 7] {
                let (a, b) = differ(&raw, cols, rows, &[], slice);
                assert!(a == b, "{name} at {cols}x{rows}, slices of {slice}: {}", first_difference(&a, &b));
            }
        }
    }
}

#[test]
fn each_handler_method_leaves_the_same_state() {
    let pre = b"\x1b[1;1Habcdefghij\r\nabcdefghij\r\nabcdefghij\r\n\x1b[2;3H";
    let equal: &[&[u8]] = &[
        b"\x1b]2;t\x07", b"\x1b[5 q", b"\x1b]50;CursorShape=1\x07", "xé中".as_bytes(), b"\x1b[5;7H", b"\x1b[9d", b"\x1b[9G", b"\x1b[1G\x1b[2@",
        b"\x1b[2A", b"\x1b[2B", b"\x1b[c", b"\x1b[6n", b"\x1b[3C", b"\x1b[2D", b"\x1b[2E", b"\x1b[1F", b"\t", b"\x08", b"\r", b"\n", b"\x07",
        b"\x1a", b"\x1b[5G\x1bH", b"\x1b[2S", b"\x1b[2T", b"\x1b[2L", b"\x1b[2M", b"\x1b[3X", b"\x1b[3P", b"\x1b[9G\x1b[2Z",
        b"\x1b[3g\x1b[9G\x1b[65535Z", b"\x1b[9G\x1b[65535Z", b"\x1b[2I", b"\x1b7", b"\x1b7\x1b[5;5H\x1b8", b"\x1b[2K", b"\x1b[2J", b"\x1b[3J",
        b"\x1b[3g\t", b"\x1b[?5W", b"\x1b]2;t\x07\x1b[22t\x1bc", b"\x1b[1;1H\x1bM", b"\x1b[1;3;31;48;5;200;58:2::1:2:3mx", b"\x1b[4hx",
        b"\x1b[4h\x1b[4lx", b"\x1b[4$p", b"\x1b[?1000h\x1b[?1006h", b"\x1b[?2026hx\x1b[?2026l", b"\x1b[?25l", b"\x1b[?2026l",
        b"\x1b[?1049$p", b"\x1b[?2026$p", b"\x1b[2;3r", b"\x1b=", b"\x1b=\x1b>", b"\x1b)0\x0eq\x0fq", b"\x1b(0q", b"\x1b]4;1;rgb:12/34/56\x07",
        b"\x1b]11;?\x07", b"\x1b]4;1;rgb:12/34/56\x07\x1b]104;1\x07", b"\x1b]52;c;aGk=\x07", b"\x1b]52;c;?\x07", b"\x1b#8",
        b"\x1b]2;a\x07\x1b[22t", b"\x1b]2;a\x07\x1b[22t\x1b]2;b\x07\x1b[23t", b"\x1b[14t", b"\x1b[18t", b"\x1b]22;pointer\x07", b"\x1b[?u",
        b"\x1b[>1u", b"\x1b[<u", b"\x1b[=1;1u", b"\x1b[>4;2m", b"\x1b[?4m", b"\x1b[0;0 k",
    ];
    let run = |bytes: &[u8]| {
        let (mut a, mut b) = (Term::new(80, 24, 100), Term::unbounded(80, 24, 100));
        for t in [&mut a, &mut b] {
            t.feed(pre);
            t.feed(bytes);
            t.finish();
        }
        (a, b)
    };
    for bytes in equal {
        let (a, b) = run(bytes);
        assert!(a.full_state() == b.full_state(), "{:?}", String::from_utf8_lossy(bytes));
    }
    // newline: vte 0.15 never calls it, so it is called on both directly.
    let (mut a, mut b) = run(b"");
    Guard { term: &mut a.term, titles: &mut a.titles, hits: &mut a.hits }.newline();
    b.term.newline();
    assert!(a.full_state() == b.full_state(), "newline");
    // The three that differ, by design.
    let (a, b) = run(format!("a{}", "\u{301}".repeat(31)).as_bytes());
    assert_eq!((marks_on(&a, 1, 2), marks_on(&b, 1, 2)), (30, 31));
    let pushes: String = (1..=9).map(|i| format!("\x1b]2;{i}\x07\x1b[22t")).collect();
    let (a, b) = run(format!("{pushes}{}", "\x1b[23t".repeat(9)).as_bytes());
    assert_eq!((a.titles.depth(), a.title(), b.title()), (0, Some("2"), Some("1")), "8 kept against 9");
    let (a, b) = run(b"\x1b]8;;http://x\x07y");
    let link = |t: &Term| t.term.grid()[alacritty_terminal::index::Line(1)][Column(2)].hyperlink().is_some();
    assert_eq!((link(&a), link(&b)), (false, true));
}

#[test]
fn a_dump_from_an_older_build_is_capped_as_it_is_restored() {
    let mut t = Term::new(80, 24, 100);
    t.restore(format!("a{}\r\nnext", "\u{301}".repeat(5000)).as_bytes(), 0);
    assert_eq!(t.line(0).unwrap().trim_end().chars().count(), 31);
    assert!(t.line(1).unwrap().starts_with("next"));
    let once = t.dump();
    let mut u = Term::new(80, 24, 100);
    u.restore(&once, 0);
    assert_eq!(u.dump(), once);
}

#[test]
fn a_huge_title_saved_many_times_stays_small() {
    let mut t = Term::new(80, 24, 100);
    t.feed(&[&b"\x1b]0;"[..], &vec![b'x'; 1 << 20], b"\x07"].concat());
    for _ in 0..64 {
        t.feed(b"\x1b[22t");
    }
    assert!(t.title().unwrap().len() <= OSC_BYTES as usize);
    assert!(t.titles.depth() <= TITLES);
}

#[test]
fn an_update_holds_at_most_2_mib_and_vte_never_buffers() {
    let mut t = Term::new(80, 24, 30_000);
    t.take(b"\x1b[?2026h");
    let total = (2 << 20) + (16 << 10);
    for _ in 0..total / 8192 {
        t.take(&[b'x'; 8192]);
        assert!(t.held.len() <= crate::term::HOLD_CAP);
        while !t.parse(None) {}
        assert_eq!(t.parser.sync_bytes_count(), 0);
    }
    t.finish();
    let xs: usize = t.all_lines().iter().map(|l| l.matches('x').count()).sum();
    assert_eq!(xs, total, "every x is on screen or in history");
}

#[test]
fn an_update_shows_at_its_end_or_150_ms_after_it_began() {
    let now = std::time::Instant::now();
    let mut t = Term::new(80, 24, 100);
    t.feed(b"before\x1b[?2026hafter");
    let text = t.screen().contents();
    assert!(text.contains("before") && !text.contains("after"), "{text}");
    assert!(!t.release_stale(now));
    let since = t.held_since;
    t.feed(b"\x1b[?2026hmore");
    assert_eq!(t.held_since, since, "a second begin does not re-arm it");
    assert!(t.release_stale(now + std::time::Duration::from_millis(151)));
    t.parse(None);
    assert_eq!(t.parser.sync_bytes_count(), 0, "vte never buffers an update itself");
    assert!(t.screen().contents().contains("aftermore"));

    let mut t = Term::new(80, 24, 100);
    t.feed(b"\x1b[?2026hx\x1b[?2026l");
    assert!(t.screen().contents().contains('x'), "shown at its end");

    let mut t = Term::new(80, 24, 100);
    t.feed(b"\x1b[?1000h\x1b[?2026h");
    t.mouse_off();
    assert!(t.wants_mouse(), "nerd's bytes wait in the update, in their place");
    t.release_hold();
    t.parse(None);
    assert!(!t.wants_mouse());

    let mut t = Term::new(80, 24, 100);
    t.feed(b"\x1b[?1000h\x1b[?2026hy");
    t.modes_off();
    assert!(!t.queued() && t.held.is_empty() && !t.wants_mouse());
    assert!(!t.screen().contents().contains('y'));
}

#[test]
fn the_caps_that_fired_are_told_once() {
    let mut t = Term::new(80, 24, 100);
    for _ in 0..5 {
        t.feed(&[&b"\x1b]0;"[..], &vec![b'x'; 5000], b"\x07x\x1b[500b"].concat());
    }
    let hits = t.take_hits();
    assert_eq!(hits, HIT_OSC | HIT_REP);
    assert_eq!(t.take_hits(), 0);
    assert_eq!(describe(hits), "an OSC over 4096 characters, a repeat wider than the pane");
    for (name, raw) in corpus() {
        let mut t = Term::new(120, 40, 100);
        t.feed(&raw);
        assert_eq!(t.take_hits(), 0, "{name}");
    }
}

/// The slowest of three runs of one parse of `bytes` in a single slice, after `setup`, in ms.
fn slice_ms(cols: u16, rows: u16, setup: &[u8], bytes: &[u8]) -> f64 {
    (0..3)
        .map(|_| {
            let mut t = Term::new(cols, rows, 1000);
            t.feed(setup);
            t.take(bytes);
            let start = std::time::Instant::now();
            assert!(t.parse_in(None, bytes.len().max(1)));
            start.elapsed().as_secs_f64() * 1e3
        })
        .fold(0.0, f64::max)
}

fn cut(pattern: &[u8], n: usize) -> Vec<u8> {
    pattern.iter().copied().cycle().take(n).collect()
}

/// S6: `cargo test --release --bin nerd -- --ignored --nocapture measure_slices`.
#[test]
#[ignore]
fn measure_slices() {
    let sizes = [(200u16, 50u16), (600, 170), (1000, 280)];
    for (cols, rows) in sizes {
        let row: Vec<String> = (8..=13).map(|p| format!("{}={:.2}", 1 << p, slice_ms(cols, rows, b"", &cut(b"\x1b#8", 1 << p)))).collect();
        println!("DECALN {cols}x{rows}: {}", row.join(" "));
    }
    for (cols, rows) in sizes {
        let rep = filter(&cut(b"a\x1b[65535b", SLICE), cols).0;
        let cbt = cut(b"\x1b[65535Z", SLICE);
        let plain = cut(b"plain text of a line, 0123456789\r\n", SLICE);
        let osc = filter(&[&b"\x1b]0;"[..], &vec![b'x'; 1 << 20], b"\x07"].concat(), cols).0[..SLICE].to_vec();
        let marks = "\u{301}".repeat(SLICE / 2).into_bytes();
        let full = format!("a{}", "\u{301}".repeat(30));
        println!(
            "SLICE={SLICE} {cols}x{rows}: rep={:.4} cbt={:.4} plain={:.4} osc={:.4} marks-on-full={:.4}",
            slice_ms(cols, rows, b"", &rep),
            slice_ms(cols, rows, format!("\x1b[3g\x1b[{cols}G").as_bytes(), &cbt),
            slice_ms(cols, rows, b"", &plain),
            slice_ms(cols, rows, b"", &osc),
            slice_ms(cols, rows, full.as_bytes(), &marks),
        );
    }
    // The largest synchronized frame of the corpus, released at its end, parsed through `parse` in slices.
    for (name, raw) in corpus() {
        let (mut best, mut from) = ((0, 0), None);
        for i in 0..raw.len().saturating_sub(7) {
            match &raw[i..i + 8] {
                b"\x1b[?2026h" if from.is_none() => from = Some(i),
                b"\x1b[?2026l" => {
                    if let Some(f) = from.take()
                        && i + 8 - f > best.1 - best.0
                    {
                        best = (f, i + 8);
                    }
                }
                _ => {}
            }
        }
        if best.1 == 0 {
            continue;
        }
        let frame = &raw[best.0..best.1];
        let mut t = Term::new(600, 170, 1000);
        t.take(frame);
        let start = std::time::Instant::now();
        t.parse(None);
        println!("frame {name}: {} bytes parsed in {:.2} ms at 600x170", frame.len(), start.elapsed().as_secs_f64() * 1e3);
    }
    // Every cell of a default-Scrollback pane holding 30 marks, read as search reads it.
    let mut t = Term::new(80, 24, 10_000);
    t.feed(&b"\r\n".repeat(10_100));
    let mut proto = alacritty_terminal::term::cell::Cell { c: 'a', ..Default::default() };
    for _ in 0..30 {
        proto.push_zerowidth('\u{301}');
    }
    let g = t.term.grid_mut();
    for l in -(g.history_size() as i32)..g.screen_lines() as i32 {
        for c in 0..g.columns() {
            g[alacritty_terminal::index::Line(l)][Column(c)] = proto.clone();
        }
    }
    let start = std::time::Instant::now();
    let lines = t.all_lines();
    println!("all_lines 80x24 + {} history, every cell 30 marks: {:.1} ms", lines.len() - 24, start.elapsed().as_secs_f64() * 1e3);
    // Plain throughput: 64 MiB of the corpus in 8 KiB chunks, today's path against the bounded one.
    let all: Vec<u8> = corpus().into_iter().flat_map(|(_, raw)| raw).collect();
    let all: Vec<u8> = all.iter().copied().cycle().take(64 << 20).collect();
    for (label, mut t) in [("before", Term::unbounded(120, 40, 10_000)), ("after", Term::new(120, 40, 10_000))] {
        let start = std::time::Instant::now();
        for c in all.chunks(8192) {
            t.take(c);
            while !t.parse(Some(std::time::Instant::now())) {}
        }
        println!("throughput {label}: {:.1} MB/s", all.len() as f64 / start.elapsed().as_secs_f64() / 1e6);
    }
    let mut t = Term::new(120, 40, 10_000);
    let start = std::time::Instant::now();
    for c in all.chunks(8192) {
        t.take(c);
        t.parse(None);
    }
    println!("throughput after, no deadline: {:.1} MB/s", all.len() as f64 / start.elapsed().as_secs_f64() / 1e6);
    let (mut f, mut out, mut marks, mut hits) = (Filter::default(), Vec::new(), Vec::new(), 0);
    let start = std::time::Instant::now();
    for c in all.chunks(8192) {
        out.clear();
        marks.clear();
        f.pass(c, 120, &mut hits, &mut out, &mut marks);
    }
    println!("filter alone: {:.1} MB/s", all.len() as f64 / start.elapsed().as_secs_f64() / 1e6);
}
