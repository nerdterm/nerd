//! The public built-in previews: CSV as a table, JSON laid out, Markdown rendered. They answer a preview request
//! exactly as a plugin would, and their answer goes through the same ingest.

use std::io::Read;
use std::path::Path;

use crate::proto::{PluginMsg, TextSpan};
use crate::slots::{cells, clean, fit, LINE_CELLS, PREVIEW_LINES};

pub const EXTENSIONS: &[&str] = &["csv", "json", "md", "markdown"];

/// As much as the plain preview reads.
const LIMIT: u64 = crate::preview::PREVIEW_LIMIT;
/// Widest a table cell is drawn; the rest of it is cut with an ellipsis.
const CELL: usize = 40;

/// Lines as runs of text with their role names.
type Lines = Vec<Vec<(String, &'static str)>>;

/// The file's extension as a preview is claimed by: lowercase, without the dot.
pub fn extension(path: &Path) -> Option<String> {
    path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase)
}

/// The header's word for what drew the view.
pub fn kind(ext: &str) -> &'static str {
    match ext {
        "csv" => "csv table",
        "json" => "json",
        _ => "markdown",
    }
}

/// The answer to preview request `req`; `None` leaves the file as the plain preview shows it.
pub fn answer(req: u64, path: &Path, cols: u16) -> Option<PluginMsg> {
    let ext = extension(path)?;
    let (text, cut) = read(path)?;
    let lines = match ext.as_str() {
        "csv" => table(&records(&text, cut)),
        "json" if !cut => json(&text)?,
        "md" | "markdown" => markdown(&text, cols),
        _ => return None,
    };
    let (lines, capped) = finish(lines);
    Some(PluginMsg::Preview { req, lines, cut: cut || capped, modes: vec![], mode: String::new() })
}

/// The file's text, and whether it was longer than read; `None` for what is not a readable text file.
fn read(path: &Path) -> Option<(String, bool)> {
    // A FIFO or a device blocks `open` itself, and the one preview worker with it.
    let md = std::fs::metadata(path).ok().filter(|m| m.is_file())?;
    let mut bytes = Vec::new();
    std::fs::File::open(path).ok()?.take(LIMIT).read_to_end(&mut bytes).ok()?;
    let cut = md.len() > LIMIT;
    if bytes.contains(&0) {
        return None;
    }
    Some((String::from_utf8_lossy(&bytes).into_owned(), cut))
}

/// Under the caps a plugin's answer must keep: whole lines dropped past the last, a wide line cut.
fn finish(lines: Lines) -> (Vec<Vec<TextSpan>>, bool) {
    let mut cut = lines.len() > PREVIEW_LINES;
    let out = lines
        .into_iter()
        .take(PREVIEW_LINES)
        .map(|l| {
            let mut room = LINE_CELLS;
            let mut spans = Vec::new();
            for (text, role) in l {
                let text = clean(&text);
                if room == 0 {
                    cut = true;
                    break;
                }
                let text = if cells(&text) > room {
                    cut = true;
                    fit(&text, room)
                } else {
                    text
                };
                room -= cells(&text);
                spans.push(TextSpan { text, role: role.into() });
            }
            spans
        })
        .collect();
    (out, cut)
}

/// RFC 4180 records: quoted fields may hold commas, quotes as `""` and line breaks. A file cut short loses its last record.
fn records(text: &str, cut: bool) -> Vec<Vec<String>> {
    let (mut out, mut rec, mut field) = (Vec::new(), Vec::new(), String::new());
    let (mut quoted, mut was_quoted) = (false, false);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' if field.is_empty() && !was_quoted => (quoted, was_quoted) = (true, true),
            ',' => {
                rec.push(std::mem::take(&mut field));
                was_quoted = false;
            }
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' => {
                rec.push(std::mem::take(&mut field));
                out.push(std::mem::take(&mut rec));
                was_quoted = false;
            }
            _ => field.push(c),
        }
    }
    if cut {
        return out;
    }
    if !field.is_empty() || !rec.is_empty() || was_quoted {
        rec.push(field);
        out.push(rec);
    }
    out
}

/// Columns aligned to their widest cell, the first row as the header, a line break in a cell shown as `↵`.
fn table(rows: &[Vec<String>]) -> Lines {
    let shown: Vec<Vec<String>> = rows.iter().map(|r| r.iter().map(|f| fit(&clean(&f.replace("\r\n", "\n").replace('\n', "\u{21b5}")), CELL)).collect()).collect();
    let n = shown.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..n).map(|j| shown.iter().filter_map(|r| r.get(j)).map(|c| cells(c)).max().unwrap_or(0)).collect();
    let row = |r: &[String], role: &'static str| {
        let mut l = Vec::new();
        for (j, w) in widths.iter().enumerate() {
            if j > 0 {
                l.push((" \u{2502} ".to_string(), "faint"));
            }
            let c = r.get(j).map(String::as_str).unwrap_or("");
            let pad = if j + 1 == n { String::new() } else { " ".repeat(w - cells(c)) };
            l.push((format!("{c}{pad}"), role));
        }
        l
    };
    shown.iter().enumerate().map(|(i, r)| row(r, if i == 0 { "accent" } else { "text" })).collect()
}

/// Valid JSON laid out two spaces a level, its text kept as written, coloured as the plain preview colours JSON.
fn json(text: &str) -> Option<Lines> {
    serde_json::from_str::<serde::de::IgnoredAny>(text).ok()?;
    let b: Vec<char> = text.chars().collect();
    let mut lines = vec![String::new()];
    let mut depth = 0usize;
    let mut i = 0;
    while i < b.len() {
        let last = lines.last_mut().expect("never empty");
        match b[i] {
            '"' => {
                let start = i;
                i += 1;
                while i < b.len() && b[i] != '"' {
                    i += if b[i] == '\\' { 2 } else { 1 };
                }
                last.extend(&b[start..(i + 1).min(b.len())]);
            }
            c @ ('{' | '[') => {
                let next = (i + 1..b.len()).find(|&j| !b[j].is_whitespace());
                if let Some(j) = next.filter(|&j| matches!(b[j], '}' | ']')) {
                    last.push(c);
                    last.push(b[j]);
                    i = j;
                } else {
                    last.push(c);
                    depth += 1;
                    lines.push("  ".repeat(depth));
                }
            }
            c @ ('}' | ']') => {
                depth = depth.saturating_sub(1);
                lines.push("  ".repeat(depth) + &c.to_string());
            }
            ',' => {
                last.push(',');
                lines.push("  ".repeat(depth));
            }
            ':' => last.push_str(": "),
            c if c.is_whitespace() => {}
            c => last.push(c),
        }
        i += 1;
    }
    Some(lines.iter().map(|l| crate::preview::roles(l, crate::preview::Lang::Json).into_iter().map(|(s, r)| (s.to_string(), r)).collect()).collect())
}

/// Headings without their `#`, bullets, quotes behind a bar, links and code spans picked out; code blocks as they are.
fn markdown(text: &str, cols: u16) -> Lines {
    let mut out = Vec::new();
    let mut fence = false;
    for raw in text.lines() {
        let line = raw.replace('\t', "    ");
        let t = line.trim_start();
        let indent = line[..line.len() - t.len()].to_string();
        if t.starts_with("```") || t.starts_with("~~~") {
            fence = !fence;
            out.push(vec![(line.clone(), "faint")]);
            continue;
        }
        if fence {
            out.push(vec![(line.clone(), "text")]);
            continue;
        }
        let hashes = t.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && t[hashes..].chars().next().is_none_or(|c| c == ' ') {
            out.push(vec![(t[hashes..].trim().to_string(), "accent")]);
            continue;
        }
        let bare: String = t.chars().filter(|c| *c != ' ').collect();
        if bare.len() >= 3 && ['-', '*', '_'].iter().any(|m| bare.chars().all(|c| c == *m)) {
            out.push(vec![("\u{2500}".repeat((cols as usize).clamp(3, 80)), "faint")]);
            continue;
        }
        let mut l = Vec::new();
        let rest = if let Some(q) = t.strip_prefix('>') {
            l.push((format!("{indent}\u{2502} "), "faint"));
            inline(&mut l, q.trim_start(), "muted");
            out.push(l);
            continue;
        } else if let Some(r) = ["- ", "* ", "+ "].iter().find_map(|m| t.strip_prefix(m)) {
            l.push((format!("{indent}\u{2022} "), "faint"));
            r
        } else if let Some(d) = t.find(['.', ')']).filter(|&d| d > 0 && d < 10 && t[..d].bytes().all(|b| b.is_ascii_digit()) && t[d + 1..].starts_with(' ')) {
            l.push((format!("{indent}{} ", &t[..=d]), "faint"));
            &t[d + 2..]
        } else {
            l.push((indent, "text"));
            t
        };
        inline(&mut l, rest, "text");
        out.push(l);
    }
    out
}

/// Code spans in teal without their backticks, a link's text in teal with its target after it, emphasis muted without its marks.
fn inline(l: &mut Vec<(String, &'static str)>, s: &str, base: &'static str) {
    let mut rest = s;
    while !rest.is_empty() {
        let code = rest.find('`');
        let link = rest.find('[').filter(|&i| rest[i..].find("](").is_some_and(|c| rest[i + c..].contains(')')));
        match (code, link) {
            (Some(c), l2) if l2.is_none_or(|k| c < k) && rest[c + 1..].contains('`') => {
                let end = c + 1 + rest[c + 1..].find('`').expect("checked");
                l.push((rest[..c].to_string(), base));
                l.push((rest[c + 1..end].to_string(), "teal"));
                rest = &rest[end + 1..];
            }
            (_, Some(k)) => {
                let mid = k + rest[k..].find("](").expect("checked");
                let end = mid + rest[mid..].find(')').expect("checked");
                l.push((rest[..k].to_string(), base));
                l.push((rest[k + 1..mid].to_string(), "teal"));
                l.push((format!(" ({})", &rest[mid + 2..end]), "faint"));
                rest = &rest[end + 1..];
            }
            _ => {
                l.push((rest.to_string(), base));
                rest = "";
            }
        }
    }
    let spans = std::mem::take(l);
    for (t, role) in spans {
        if role == base { emphasis(l, &t, base) } else { l.push((t, role)) }
    }
    l.retain(|(t, _)| !t.is_empty());
}

/// `*x*`, `_x_`, `**x**` and `__x__` at word edges: the text muted, the marks gone; `snake_case` and `a * b` stay.
fn emphasis(l: &mut Vec<(String, &'static str)>, s: &str, base: &'static str) {
    let mut rest = s;
    'next: while !rest.is_empty() {
        for (i, c) in rest.char_indices().filter(|(_, c)| matches!(c, '*' | '_')) {
            let mark = if rest[i + 1..].starts_with(c) { &rest[i..i + 2] } else { &rest[i..i + 1] };
            let from = i + mark.len();
            let open = rest[..i].chars().last().is_none_or(|b| !b.is_alphanumeric()) && rest[from..].chars().next().is_some_and(|n| !n.is_whitespace() && n != c);
            if !open {
                continue;
            }
            let close = rest[from..].match_indices(mark).map(|(k, _)| from + k).find(|&k| {
                !rest[..k].ends_with(char::is_whitespace) && !rest[k + mark.len()..].starts_with(|n: char| n.is_alphanumeric() || n == c)
            });
            if let Some(k) = close.filter(|&k| k > from) {
                l.push((rest[..i].to_string(), base));
                l.push((rest[from..k].to_string(), "muted"));
                rest = &rest[k + mark.len()..];
                continue 'next;
            }
        }
        l.push((rest.to_string(), base));
        break;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &Lines) -> Vec<String> {
        lines.iter().map(|l| l.iter().map(|(t, _)| t.as_str()).collect::<String>()).collect()
    }

    #[test]
    fn csv_fields_follow_rfc_4180() {
        let r = records("a,\"b, c\",\"say \"\"hi\"\"\"\r\n\"two\nlines\",,x\n\"\"\nlast,\"open", false);
        assert_eq!(r, vec![vec!["a", "b, c", "say \"hi\""], vec!["two\nlines", "", "x"], vec![""], vec!["last", "open"]]);
        assert_eq!(records("a,b\nc,d", true), vec![vec!["a", "b"]], "a file read only in part loses its last, maybe partial, record");
        assert_eq!(records("a,b\n", false), vec![vec!["a", "b"]], "a final line break makes no empty record");
        assert_eq!(records("x\"y\",z", false), vec![vec!["x\"y\"", "z"]], "a quote inside an unquoted field is text");
        assert!(records("", false).is_empty());
    }

    #[test]
    fn a_csv_table_aligns_ragged_rows_and_cuts_wide_cells() {
        let wide = "w".repeat(60);
        let t = table(&records(&format!("name,n\na.rs,12,extra\nb\n\"x\ny\",{wide}\n界界,1\n"), false));
        let sp = |n: usize| " ".repeat(n);
        assert_eq!(
            text(&t),
            [
                format!("name │ n{} │ ", sp(39)),
                format!("a.rs │ 12{} │ extra", sp(38)),
                format!("b{} │ {} │ ", sp(3), sp(40)),
                format!("x↵y{} │ {}… │ ", sp(1), "w".repeat(39)),
                format!("界界 │ 1{} │ ", sp(39)),
            ]
        );
        assert!(t[0].iter().filter(|(s, _)| !s.contains('│')).all(|(_, r)| *r == "accent"), "the header row");
        assert!(t[1].iter().filter(|(s, _)| !s.contains('│')).all(|(_, r)| *r == "text"));
        assert_eq!(t[0][1].1, "faint");
    }

    #[test]
    fn the_caps_cut_a_long_or_wide_table() {
        let rows = |n: usize| (0..n).map(|i| format!("{i}\n")).collect::<String>();
        let (lines, cut) = finish(table(&records(&rows(3000), false)));
        assert!(cut && lines.len() == PREVIEW_LINES);
        let (lines, cut) = finish(table(&records(&rows(PREVIEW_LINES), false)));
        assert!(!cut && lines.len() == PREVIEW_LINES, "exactly as many records as lines is whole");
        let wide: String = (0..40).map(|i| format!("{}{i},", "c".repeat(30))).collect();
        let (lines, cut) = finish(table(&records(&wide, false)));
        let w: usize = lines[0].iter().map(|s| cells(&s.text)).sum();
        assert!(cut && w == LINE_CELLS, "{w}");
        assert!(crate::slots::over_cap(&PluginMsg::Preview { req: 1, lines, cut, modes: vec![], mode: String::new() }).is_none(), "under the caps a plugin keeps");
    }

    #[test]
    fn json_is_laid_out_as_written_and_coloured_as_the_plain_view_colours_it() {
        let j = json("{\"a\": 1.5e3, \"b\": [true, null, \"x,{y}\"], \"e\": {}, \"f\": [ ], \"s\": \"q\\\"uote\"}").unwrap();
        assert_eq!(text(&j), ["{", "  \"a\": 1.5e3,", "  \"b\": [", "    true,", "    null,", "    \"x,{y}\"", "  ],", "  \"e\": {},", "  \"f\": [],", "  \"s\": \"q\\\"uote\"", "}"]);
        assert_eq!(j[1], vec![("  ".into(), "text"), ("\"a\"".into(), "accent"), (": ".into(), "text"), ("1.5e3".into(), "teal"), (",".into(), "text")]);
        assert_eq!(j[5][1], ("\"x,{y}\"".into(), "ok"));
        assert!(json("{\"a\": }").is_none(), "not JSON: the plain preview stays");
        assert_eq!(text(&json("[]").unwrap()), ["[]"]);
    }

    #[test]
    fn a_line_of_emoji_stays_within_the_caps() {
        let (lines, cut) = finish(json(&format!("{{\"k\":\"{}\"}}", "\u{2764}\u{FE0F}".repeat(400))).unwrap());
        assert!(cut && lines.iter().all(|l| l.iter().map(|s| cells(&s.text)).sum::<usize>() <= LINE_CELLS));
    }

    #[test]
    fn markdown_is_rendered_into_styled_lines() {
        let m = markdown("# Title\n#tag\n- one\n  * two\n12. twelve\n> quote `c`\n---\n```\n# not a heading\n```\nSee [site](https://x.y) and `code` **now**, *this* and _that_ but not snake_case or a * b.\n", 20);
        assert_eq!(
            text(&m),
            ["Title", "#tag", "• one", "  • two", "12. twelve", "│ quote c", &"─".repeat(20), "```", "# not a heading", "```", "See site (https://x.y) and code now, this and that but not snake_case or a * b."]
        );
        assert_eq!(m[0], vec![("Title".into(), "accent")]);
        assert!(m[10].contains(&("site".into(), "teal")) && m[10].contains(&("code".into(), "teal")) && m[10].contains(&(" (https://x.y)".into(), "faint")));
        assert_eq!(m[8], vec![("# not a heading".into(), "text")], "code blocks stay as written");
        for w in ["now", "this", "that"] {
            assert!(m[10].contains(&(w.into(), "muted")), "{w}: {:?}", m[10]);
        }
        let mut l = Vec::new();
        emphasis(&mut l, "**bold** x_y_z *a*b 2*3*4 _", "text");
        assert_eq!(l, vec![("".into(), "text"), ("bold".into(), "muted"), (" x_y_z *a*b 2*3*4 _".into(), "text")]);
    }

    #[test]
    fn only_readable_text_files_of_the_three_kinds_are_answered() {
        let dir = std::env::temp_dir().join(format!("nerd-previews-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (f, body) in [("a.CSV", "a,b\n"), ("b.json", "{"), ("c.txt", "x"), ("d.md", "a\0b")] {
            std::fs::write(dir.join(f), body).unwrap();
        }
        assert!(matches!(answer(7, &dir.join("a.CSV"), 40), Some(PluginMsg::Preview { req: 7, cut: false, .. })), "the extension in any case");
        assert!(answer(7, &dir.join("b.json"), 40).is_none(), "broken JSON");
        assert!(answer(7, &dir.join("c.txt"), 40).is_none());
        assert!(answer(7, &dir.join("d.md"), 40).is_none(), "binary");
        assert!(answer(7, &dir.join("gone.md"), 40).is_none());
        std::fs::write(dir.join("big.csv"), "x,y\n".repeat(300_000)).unwrap();
        assert!(matches!(answer(1, &dir.join("big.csv"), 40), Some(PluginMsg::Preview { cut: true, .. })));
        let (text, cut) = read(&dir.join("big.csv")).unwrap();
        assert!(cut && text.len() as u64 == crate::preview::PREVIEW_LIMIT, "no more read than the plain preview reads: {}", text.len());
        // Opening a FIFO waits for a writer: never opened, answered at once.
        let fifo = dir.join("pipe.csv");
        assert!(std::process::Command::new("/usr/bin/mkfifo").arg(&fifo).status().unwrap().success());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || tx.send(answer(1, &fifo, 40).is_none()).unwrap());
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(2)), Ok(true), "a FIFO hung the preview");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
