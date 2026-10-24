//! One panel widget for plugins' panels and nerd's own: rows as a list or a tree, lines over and under them, in a dialog.

use std::collections::HashSet;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::render::Theme;
use crate::slots::{Line, Role};

/// Rows a plugin's panel shows at most; the rest scroll.
pub const ROWS: usize = 15;

/// Text placed at a column of the dialog's inner width, in its style.
pub type Spans = Vec<(usize, String, Style)>;

/// One row: `base` fills it, `spans` go on top; `parent` rows open and close instead of being chosen.
pub struct Row {
    pub id: String,
    pub base: Style,
    pub spans: Spans,
    pub parent: bool,
}

/// What a panel shows this frame, laid out for its inner width.
#[derive(Default)]
pub struct Content {
    pub head: Vec<Spans>,
    pub rows: Vec<Row>,
    pub body: Vec<Spans>,
    /// Lines kept for the body whatever is selected, so choosing does not resize the dialog.
    pub body_h: usize,
    /// Row count with no filter applied, so the window does not resize while typing; zero when nothing is filtered.
    pub full: usize,
    /// Buttons for the chosen row, `[Key] outcome`, on the bottom line before `[Esc] close`.
    pub buttons: Vec<String>,
    /// Lines kept for the buttons whatever is selected; more are taken when they do not fit.
    pub button_lines: usize,
}

/// Where a frame put its rows, by index into `Content::rows`, and its close button.
#[derive(Default)]
pub struct Drawn {
    pub rows: Vec<(Rect, usize)>,
    pub close: Rect,
    /// Where each of `Content::buttons` went.
    pub buttons: Vec<Rect>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Hit {
    None,
    Close,
    /// A click selected this row.
    Row(usize),
    /// Enter on this row.
    Enter(usize),
    /// A click on one of the content's buttons.
    Button(usize),
}

/// What stays while the content is replaced: the selection by id, the scroll, the filter and the open tree rows.
pub struct View {
    id: Option<String>,
    at: usize,
    list: crate::chrome::Scroll,
    pub query: String,
    pub open: HashSet<String>,
    rows: usize,
    pub filter: bool,
    /// The body's lines: a detail's text scrolls on its own.
    text: crate::chrome::Scroll,
    /// The window's width, frame included.
    width: u16,
}

/// The buttons and close in lines of at most `w` cells, one cell apart: each line's indices, close as `buttons.len()`.
pub fn button_lines(buttons: &[String], w: usize) -> Vec<Vec<usize>> {
    let cells = |i: usize| buttons.get(i).map_or(crate::chrome::CLOSE.chars().count(), |b| crate::slots::cells(b));
    let mut lines: Vec<Vec<usize>> = Vec::new();
    let mut x = 0;
    for i in 0..=buttons.len() {
        match lines.last_mut() {
            Some(line) if x + 1 + cells(i) <= w => {
                line.push(i);
                x += 1 + cells(i);
            }
            _ => {
                lines.push(vec![i]);
                x = cells(i);
            }
        }
    }
    lines
}

impl View {
    /// `width`: the window's, frame included.
    pub fn new(rows: usize, filter: bool, width: u16) -> Self {
        Self { id: None, at: 0, list: Default::default(), query: String::new(), open: HashSet::new(), rows, filter, text: Default::default(), width }
    }

    /// The dialog's inner width on this screen.
    pub fn inner(&self, screen: Rect) -> usize {
        crate::chrome::fit(screen, self.width, 0).width.saturating_sub(2 * crate::chrome::DIALOG_FRAME.horizontal).max(1) as usize
    }

    /// Chooses the row with this id, wherever it is once drawn.
    pub fn select_id(&mut self, id: &str) {
        self.id = Some(id.into());
    }

    /// The selected row: the one with the selected id, else the one at its old place.
    pub fn selected(&self, c: &Content) -> usize {
        self.id.as_ref().and_then(|id| c.rows.iter().position(|r| r.id == *id)).unwrap_or(self.at.min(c.rows.len().saturating_sub(1)))
    }

    fn select(&mut self, i: usize, c: &Content) {
        self.at = i;
        self.id = c.rows.get(i).map(|r| r.id.clone());
    }

    /// The dialog, as tall as the whole content, and how many rows fit in it.
    fn geometry(&self, screen: Rect, c: &Content) -> (Rect, usize) {
        let w = self.inner(screen);
        let more = button_lines(&c.buttons, w).len().max(c.button_lines).saturating_sub(1);
        // head, rows, blank, body, the button lines over close's
        let fixed = (c.head.len() + 1 + c.body_h + more) as u16;
        let room = crate::chrome::limits(screen).1.saturating_sub(fixed + 4).max(1);
        let visible = (c.rows.len().max(c.full).min(self.rows) as u16).min(room);
        (crate::chrome::fit(screen, self.width, fixed + visible), visible as usize)
    }

    pub fn draw(&self, buf: &mut Buffer, screen: Rect, theme: &Theme, title: &str, c: &Content) -> Drawn {
        let mut drawn = Drawn::default();
        let (r, visible) = self.geometry(screen, c);
        let r = r.intersection(buf.area);
        if r.is_empty() {
            return drawn;
        }
        let content = crate::chrome::dialog(buf, r, Some(title), theme.border_focus, theme, crate::chrome::Moat::All);
        let w = content.width as usize;
        if w == 0 || content.height == 0 {
            return drawn;
        }
        // The buttons sit on the last lines; nothing else may reach them.
        let lines = button_lines(&c.buttons, w);
        let bottom = content.bottom().saturating_sub(lines.len().max(c.button_lines).max(1) as u16).max(content.y);
        let put = |buf: &mut Buffer, y: u16, spans: &Spans| {
            for (x, text, style) in spans {
                if *x < w {
                    buf.set_stringn(content.x + *x as u16, y, crate::slots::fit(text, w - x), w - x, *style);
                }
            }
        };
        let mut y = content.y;
        for line in &c.head {
            if y < bottom {
                put(buf, y, line);
            }
            y += 1;
        }
        let selected = self.selected(c);
        let offset = self.list.follow(selected, visible, c.rows.len());
        for (i, row) in c.rows.iter().enumerate().skip(offset).take(visible) {
            if y >= bottom {
                break;
            }
            buf.set_stringn(content.x, y, " ".repeat(w), w, row.base);
            put(buf, y, &row.spans);
            if i == selected {
                buf.set_style(Rect::new(content.x, y, w as u16, 1), theme.cursor());
            }
            drawn.rows.push((Rect::new(content.x, y, w as u16, 1), i));
            y += 1;
        }
        y += 1;
        // The row over the buttons says what is hidden: the rows, the text, or both; it takes the text's last line.
        let room = bottom.saturating_sub(y) as usize;
        let shown = if self.list.hides() || c.body.len() > room && room > 1 { room.saturating_sub(1) } else { room };
        let from = self.text.place(shown, c.body.len());
        for line in c.body.iter().skip(from).take(shown) {
            put(buf, y, line);
            y += 1;
        }
        let at = match (self.list.hidden("rows"), (shown < room).then(|| self.text.hidden("lines")).flatten()) {
            (Some(rows), Some(lines)) => Some(format!("{rows} · {lines}")),
            (Some(_), None) => self.list.hidden("lines"),
            (None, lines) => lines,
        };
        if let Some(at) = at {
            crate::chrome::say(buf, r, bottom.saturating_sub(1), &at, theme);
        }
        // The buttons, then close, centred together line by line: `[Key] outcome` each, one cell apart.
        let text = Style::default().bg(theme.ground).fg(theme.text);
        let label = |i: usize| c.buttons.get(i).map_or(crate::chrome::CLOSE, String::as_str);
        for (n, line) in lines.iter().enumerate() {
            let y = content.bottom().saturating_sub((lines.len() - n) as u16);
            if y < content.y {
                continue;
            }
            let lw = line.iter().map(|&i| crate::slots::cells(label(i)) + 1).sum::<usize>().saturating_sub(1);
            let mut x = content.x + (w as u16).saturating_sub(lw as u16) / 2;
            for &i in line {
                let len = (crate::slots::cells(label(i)) as u16).min(content.right().saturating_sub(x));
                buf.set_stringn(x, y, label(i), len as usize, text);
                let at = Rect::new(x, y, len, 1);
                if i == c.buttons.len() { drawn.close = at } else { drawn.buttons.push(at) }
                x += len + 1;
            }
        }
        drawn
    }

    pub fn key(&mut self, k: crossterm::event::KeyEvent, _screen: Rect, c: &Content) -> Hit {
        use crossterm::event::{KeyCode::*, KeyModifiers};
        let mut at = self.selected(c);
        let query = self.query.clone();
        match k.code {
            PageUp | PageDown | Home | End if self.text.hides() => _ = self.text.key(k.code),
            // With one row there is nothing to choose: the arrows read the text.
            Up | Down if c.rows.len() <= 1 && self.text.hides() => _ = self.text.key(k.code),
            Esc if self.filter && !self.query.is_empty() => self.query.clear(),
            Esc => return Hit::Close,
            Enter => return Hit::Enter(at),
            Up => at = at.saturating_sub(1),
            Down => at = (at + 1).min(c.rows.len().saturating_sub(1)),
            PageUp | PageDown | Home | End => at = self.list.pick(k.code, at, c.rows.len()).unwrap_or(at),
            Backspace if self.filter => {
                self.query.pop();
            }
            Char(ch) if self.filter && !k.modifiers.contains(KeyModifiers::CONTROL) => self.query.push(ch),
            _ => {}
        }
        if self.query != query {
            // Other rows now: the first match is selected.
            (self.id, self.at) = (None, 0);
            self.list.set(0);
            return Hit::None;
        }
        self.select(at, c);
        Hit::None
    }

    /// `drawn`: where the last frame put the rows and the button.
    pub fn mouse(&mut self, ev: crossterm::event::MouseEvent, screen: Rect, c: &Content, drawn: &Drawn) -> Hit {
        use crossterm::event::MouseEventKind::*;
        let at = (ev.column, ev.row).into();
        match ev.kind {
            Down(_) => {
                if drawn.close.contains(at) {
                    return Hit::Close;
                }
                if let Some(i) = drawn.buttons.iter().position(|r| r.contains(at)) {
                    return Hit::Button(i);
                }
                if let Some(&(_, i)) = drawn.rows.iter().find(|(r, _)| r.contains(at)) {
                    self.select(i, c);
                    return Hit::Row(i);
                }
                if self.geometry(screen, c).0.contains(at) { Hit::None } else { Hit::Close }
            }
            // The wheel moves the text when it is longer than its room, else the rows and the selection with them.
            kind if self.text.hides() => {
                self.text.wheel(kind);
                Hit::None
            }
            kind => {
                if self.list.wheel(kind) && !c.rows.is_empty() {
                    let at = self.list.within(self.selected(c));
                    self.select(at, c);
                }
                Hit::None
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    List,
    Tree,
    Detail,
}

/// A plugin's item, checked and cleaned by the ingest.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub id: String,
    pub label: String,
    pub detail: String,
    pub role: Role,
    pub children: Vec<Item>,
    pub opens: bool,
}

/// A plugin's panel as it last said it: rows, and for a detail panel the lines under them.
#[derive(Clone, Debug, PartialEq)]
pub struct Data {
    pub kind: Kind,
    pub title: String,
    pub items: Vec<Item>,
    pub lines: Vec<Line>,
}

/// A plugin panel's rows for this frame: the tree as far as it is open, or every match of the filter, flat.
pub fn content(d: Option<&Data>, v: &View, w: usize, theme: &Theme) -> Content {
    let ground = Style::default().bg(theme.ground);
    let Some(d) = d else {
        return Content { body: vec![vec![(0, "Waiting for the plugin…".into(), ground.fg(theme.faint))]], body_h: 2, ..Default::default() };
    };
    let q = v.query.to_lowercase();
    let tree = d.kind == Kind::Tree;
    let row = |&(depth, it): &(usize, &Item)| -> Row {
        let parent = tree && !it.children.is_empty();
        let arrow = match (tree, parent, v.open.contains(&it.id)) {
            (false, ..) => "",
            (true, false, _) => "  ",
            (true, true, false) => "\u{25b8} ",
            (true, true, true) => "\u{25be} ",
        };
        let label = format!("{}{arrow}{}", "  ".repeat(depth), it.label);
        let at = 1 + crate::slots::cells(&label) + 2;
        let mut spans = vec![(1, label, ground.fg(it.role.color(theme)))];
        if !it.detail.is_empty() {
            spans.push((at, it.detail.clone(), ground.fg(theme.faint)));
        }
        Row { id: it.id.clone(), base: ground.fg(theme.text), spans, parent }
    };
    let mut found = Vec::new();
    walk(&d.items, 0, tree, v, &q, &mut found);
    let rows: Vec<Row> = found.iter().map(row).collect();
    // Unfiltered, the rows are all there is; filtered, the rest is counted.
    let all = if q.is_empty() {
        rows.len()
    } else {
        let mut all = Vec::new();
        walk(&d.items, 0, tree, v, "", &mut all);
        all.len()
    };
    // A dialog has no sideways scroll: a line longer than the window wraps onto the next.
    let room = w.max(1);
    let mut body: Vec<Spans> = Vec::new();
    for l in &d.lines {
        let mut line: Spans = Vec::new();
        let mut x = 0;
        for (t, role) in l {
            let style = ground.fg(role.color(theme));
            let mut run = String::new();
            let mut at = x;
            for ch in t.chars() {
                let cw = crate::slots::cells(ch.encode_utf8(&mut [0; 4]));
                if x + cw > room && x > 0 {
                    if !run.is_empty() {
                        line.push((at, std::mem::take(&mut run), style));
                    }
                    body.push(std::mem::take(&mut line));
                    (x, at) = (0, 0);
                }
                run.push(ch);
                x += cw;
            }
            if !run.is_empty() {
                line.push((at, run, style));
            }
        }
        body.push(line);
    }
    let mut head = Vec::new();
    if d.kind != Kind::Detail {
        // the count sits at the right edge
        let count = format!("{}/{}", rows.len(), count(&d.items));
        let room = w.saturating_sub(crate::slots::cells(&count) + 1);
        head.push(vec![(0, format!("/{}", v.query), ground.fg(theme.text)), (room + 1, count, ground.fg(theme.faint))]);
    }
    Content { head, rows, body_h: body.len() + usize::from(!body.is_empty()), body, full: all, ..Default::default() }
}

/// Items at any depth.
fn count(items: &[Item]) -> usize {
    items.iter().map(|i| 1 + count(&i.children)).sum()
}

/// The rows to show with their depth: a tree's open branches, or with a query every match at depth 0.
fn walk<'a>(items: &'a [Item], depth: usize, tree: bool, v: &View, q: &str, out: &mut Vec<(usize, &'a Item)>) {
    for it in items {
        if q.is_empty() {
            out.push((depth, it));
        } else if format!("{} {}", it.label, it.detail).to_lowercase().contains(q) {
            out.push((0, it));
        }
        if tree && (!q.is_empty() || v.open.contains(&it.id)) {
            walk(&it.children, depth + 1, tree, v, q, out);
        }
    }
}

/// An open panel of a plugin or a public built-in, drawn from the store's latest content each frame.
pub struct Open {
    pub src: crate::slots::Src,
    pub id: String,
    /// The manifest's title, until the content names one.
    pub title: String,
    pub view: View,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    None,
    Close,
    /// A row that is not a tree branch was chosen: its id.
    Chosen(String),
    /// A row marked `opens` was chosen: its id, and the panel stays for what the plugin sends next.
    Opened(String),
}

impl Open {
    pub fn new(src: crate::slots::Src, id: String, title: String) -> Self {
        Self { src, id, title, view: View::new(ROWS, true, crate::chrome::WINDOW_W) }
    }

    pub fn draw(&self, buf: &mut Buffer, screen: Rect, theme: &Theme, d: Option<&Data>) -> Drawn {
        let title = d.map(|d| d.title.as_str()).filter(|t| !t.is_empty()).unwrap_or(&self.title);
        self.view.draw(buf, screen, theme, title, &content(d, &self.view, self.view.inner(screen), theme))
    }

    /// Keys and clicks need only the content's shape, which no theme changes.
    pub fn key(&mut self, k: crossterm::event::KeyEvent, screen: Rect, d: Option<&Data>) -> Outcome {
        self.view.filter = d.is_some_and(|d| d.kind != Kind::Detail);
        let c = content(d, &self.view, self.view.inner(screen), &Theme::dark());
        let hit = self.view.key(k, screen, &c);
        self.act(hit, &c, d)
    }

    pub fn mouse(&mut self, ev: crossterm::event::MouseEvent, screen: Rect, d: Option<&Data>, drawn: &Drawn) -> Outcome {
        let c = content(d, &self.view, self.view.inner(screen), &Theme::dark());
        let hit = self.view.mouse(ev, screen, &c, drawn);
        self.act(hit, &c, d)
    }

    /// A branch opens or closes; any other row is chosen.
    fn act(&mut self, hit: Hit, c: &Content, d: Option<&Data>) -> Outcome {
        match hit {
            Hit::Close => Outcome::Close,
            Hit::Row(i) | Hit::Enter(i) => match c.rows.get(i) {
                Some(r) if r.parent => {
                    if !self.view.open.remove(&r.id) {
                        self.view.open.insert(r.id.clone());
                    }
                    Outcome::None
                }
                Some(r) if d.and_then(|d| find(&d.items, &r.id)).is_some_and(|it| it.opens) => {
                    // The next content is another place: it starts at its top, unfiltered. Until it comes, the
                    // chosen row stays lit, so a second Enter does not choose another.
                    self.view = View::new(self.view.rows, true, self.view.width);
                    self.view.select_id(&r.id);
                    Outcome::Opened(r.id.clone())
                }
                Some(r) => Outcome::Chosen(r.id.clone()),
                None => Outcome::None,
            },
            Hit::None | Hit::Button(_) => Outcome::None,
        }
    }
}

fn find<'a>(items: &'a [Item], id: &str) -> Option<&'a Item> {
    items.iter().find_map(|it| if it.id == id { Some(it) } else { find(&it.children, id) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    fn span_w(l: &Spans) -> usize {
        l.iter().map(|(x, t, _)| x + crate::slots::cells(t)).max().unwrap_or(0)
    }

    fn item(id: &str, label: &str, children: Vec<Item>) -> Item {
        Item { id: id.into(), label: label.into(), detail: String::new(), role: Role::Text, children, opens: false }
    }

    fn list(ids: &[&str]) -> Data {
        Data { kind: Kind::List, title: "L".into(), items: ids.iter().map(|i| item(i, i, vec![])).collect(), lines: vec![] }
    }

    fn tree() -> Data {
        let leaf = |id: &str| item(id, id, vec![]);
        let items = vec![item("src", "src/", vec![item("deep", "deep/", vec![leaf("a.rs:3")]), leaf("b.rs:1")]), leaf("c.rs:9")];
        Data { kind: Kind::Tree, title: "T".into(), items, lines: vec![] }
    }

    fn detail() -> Data {
        let lines = vec![vec![("    1  ".into(), Role::Faint), ("fn a() {".into(), Role::Muted)], vec![("    2  ".into(), Role::Faint), ("// TODO here".into(), Role::Warn)]];
        Data { kind: Kind::Detail, title: String::new(), items: vec![item("a.rs:2", "a.rs:2", vec![])], lines }
    }

    const SCREEN: Rect = Rect { x: 0, y: 0, width: 80, height: 24 };

    #[test]
    fn rows_and_text_both_hidden_say_so_on_one_row() {
        let theme = Theme::dark();
        let row = |i: usize| Row { id: format!("{i}"), base: Style::default(), spans: vec![(0, format!("row {i}"), Style::default())], parent: false };
        let line = |i: usize| vec![(0, format!("line {i}"), Style::default())];
        let c = Content { rows: (0..10).map(row).collect(), body: (0..30).map(line).collect(), body_h: 30, ..Content::default() };
        let mut v = View::new(ROWS, false, crate::chrome::WINDOW_W);
        let shown = |v: &View| {
            let mut buf = Buffer::empty(SCREEN);
            v.draw(&mut buf, SCREEN, &theme, "Panel", &c);
            crate::app::snapshots::dump(&buf, SCREEN).lines().take(24).collect::<Vec<_>>().join("\n")
        };
        let at = shown(&v);
        let place = at.lines().position(|l| l.contains("│ rows 1–1 of 10 · lines 1–")).unwrap_or_else(|| panic!("{at}"));
        assert!(at.lines().nth(place + 1).is_some_and(|l| l.contains("[Esc] close")), "the row over the buttons: {at}");
        v.key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE), SCREEN, &c);
        assert!(shown(&v).contains("· lines 17–30 of 30"), "the keys read the text: {}", shown(&v));
        let only_rows = Content { body: Vec::new(), body_h: 0, ..Content { rows: (0..30).map(row).collect(), ..Content::default() } };
        let mut buf = Buffer::empty(SCREEN);
        View::new(ROWS, false, crate::chrome::WINDOW_W).draw(&mut buf, SCREEN, &theme, "Panel", &only_rows);
        let text = crate::app::snapshots::dump(&buf, SCREEN);
        assert!(text.contains("│ lines 1–") && !text.contains("rows "), "rows alone keep the word lines: {text}");
    }

    #[test]
    fn hidden_rows_say_so_whatever_the_text_takes() {
        let theme = Theme::dark();
        let row = |i: usize| Row { id: format!("{i}"), base: Style::default(), spans: vec![(0, format!("row {i}"), Style::default())], parent: false };
        let line = |i: usize| vec![(0, format!("line {i}"), Style::default())];
        // 30 rows, of which 15 show at most; the text's room is what its body_h keeps, one more than it
        for (body, body_h) in [(4, 3), (3, 3), (1, 0), (0, 0)] {
            let c = Content { rows: (0..30).map(row).collect(), body: (0..body).map(line).collect(), body_h, ..Content::default() };
            let mut buf = Buffer::empty(SCREEN);
            View::new(ROWS, false, crate::chrome::WINDOW_W).draw(&mut buf, SCREEN, &theme, "Panel", &c);
            let text: Vec<String> = crate::app::snapshots::dump(&buf, SCREEN).lines().take(24).map(String::from).collect();
            let at = text.iter().position(|l| l.contains("│ lines 1–") || l.contains("│ rows 1–")).unwrap_or_else(|| panic!("{body} lines in {body_h}: {text:#?}"));
            assert!(text[at + 1].contains("[Esc] close"), "{body} lines in {body_h}: over the buttons {text:#?}");
        }
    }

    fn labels(d: &Data, o: &Open) -> Vec<String> {
        content(Some(d), &o.view, o.view.inner(SCREEN), &Theme::dark()).rows.iter().map(|r| r.spans[0].1.trim().to_string()).collect()
    }

    fn click_row(o: &mut Open, d: &Data, label: &str) -> Outcome {
        let mut buf = Buffer::empty(SCREEN);
        let drawn = o.draw(&mut buf, SCREEN, &Theme::dark(), Some(d));
        let at = labels(d, o).iter().position(|l| l.ends_with(label)).unwrap_or_else(|| panic!("no {label} in {:?}", labels(d, o)));
        let r = drawn.rows.iter().find(|(_, i)| *i == at).unwrap().0;
        o.mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: r.x + 2, row: r.y, modifiers: KeyModifiers::NONE }, SCREEN, Some(d), &drawn)
    }

    #[test]
    fn a_detail_line_wider_than_the_window_wraps_and_loses_nothing() {
        let long: String = (0..30).map(|i| format!("w{i:02} ")).collect();
        let d = Data { kind: Kind::Detail, title: "T".into(), items: vec![item("back", "Back", vec![])], lines: vec![vec![(long.clone(), Role::Text), ("tail".into(), Role::Faint)]] };
        let o = Open::new((true, "p".into()), "p".into(), "P".into());
        let c = content(Some(&d), &o.view, 40, &Theme::dark());
        assert!(c.body.len() >= 4, "one line became several: {}", c.body.len());
        assert!(c.body.iter().all(|l| span_w(l) <= 40), "none wider than the window");
        let joined: String = c.body.iter().flat_map(|l| l.iter().map(|s| s.1.clone())).collect();
        assert_eq!(joined, format!("{long}tail"), "every character, in order");
    }

    #[test]
    fn a_detail_longer_than_its_room_scrolls_by_wheel_and_keys_and_says_where_it_is() {
        let mut o = Open::new((true, "p".into()), "p".into(), "P".into());
        let lines = (1..=100).map(|n| vec![(format!("line {n}"), Role::Text)]).collect();
        let d = Data { kind: Kind::Detail, title: "Long".into(), items: vec![item("back", "Back", vec![])], lines };
        let screen = |o: &Open| {
            let mut buf = Buffer::empty(SCREEN);
            let drawn = o.draw(&mut buf, SCREEN, &Theme::dark(), Some(&d));
            let text: String = (0..SCREEN.height).map(|y| (0..SCREEN.width).map(|x| buf[(x, y)].symbol()).collect::<String>() + "\n").collect();
            (text, drawn)
        };
        let (text, drawn) = screen(&o);
        assert!(text.contains("line 1 ") && !text.contains("line 100") && text.contains("lines 1–") && text.contains(" of 100"), "{text}");
        let wheel = |o: &mut Open, kind, drawn: &Drawn| o.mouse(MouseEvent { kind, column: 10, row: 10, modifiers: KeyModifiers::NONE }, SCREEN, Some(&d), drawn);
        wheel(&mut o, MouseEventKind::ScrollDown, &drawn);
        let (text, drawn) = screen(&o);
        assert!(text.contains("lines 4–") && !text.contains("line 1 ") && text.contains("line 4 "), "the wheel moves the text: {text}");
        o.key(KeyEvent::from(KeyCode::End), SCREEN, Some(&d));
        let (text, _) = screen(&o);
        assert!(text.contains("line 100") && text.contains("–100 of 100"), "End shows the last line: {text}");
        wheel(&mut o, MouseEventKind::ScrollDown, &drawn);
        assert!(screen(&o).0.contains("–100 of 100"), "and never past it");
        o.key(KeyEvent::from(KeyCode::Home), SCREEN, Some(&d));
        assert!(screen(&o).0.contains("lines 1–"));
        o.key(KeyEvent::from(KeyCode::Down), SCREEN, Some(&d));
        assert!(screen(&o).0.contains("lines 2–"), "with one row the arrows read the text");
        assert_eq!(o.key(KeyEvent::from(KeyCode::Enter), SCREEN, Some(&d)), Outcome::Chosen("back".into()), "Enter still chooses the row");
    }

    #[test]
    fn a_row_that_opens_keeps_the_panel_and_starts_the_next_content_at_its_top() {
        let mut o = Open::new((true, "p".into()), "p".into(), "P".into());
        let mut d = list(&["a", "b", "c"]);
        d.items[2].opens = true;
        o.key(KeyEvent::from(KeyCode::Char('c')), SCREEN, Some(&d));
        assert_eq!(o.view.query, "c");
        assert_eq!(o.key(KeyEvent::from(KeyCode::Enter), SCREEN, Some(&d)), Outcome::Opened("c".into()));
        assert!(o.view.query.is_empty() && o.view.list.get() == 0, "the filter and the scroll were the last place's");
        assert_eq!(o.view.selected(&content(Some(&d), &o.view, o.view.inner(SCREEN), &Theme::dark())), 2, "until the plugin answers, the chosen row stays lit");
        assert_eq!(o.view.selected(&content(Some(&list(&["x", "y"])), &o.view, o.view.inner(SCREEN), &Theme::dark())), 0, "and its answer starts at the top");
        assert_eq!(click_row(&mut o, &d, "c"), Outcome::Opened("c".into()), "a click opens it too");
        assert_eq!(click_row(&mut o, &d, "b"), Outcome::Chosen("b".into()), "a row without opens still closes the panel");
        let mut t = tree();
        t.items[0].children[1].opens = true;
        o.key(KeyEvent::from(KeyCode::Char('b')), SCREEN, Some(&t));
        assert_eq!(click_row(&mut o, &t, "b.rs:1"), Outcome::Opened("b.rs:1".into()), "found at any depth");
    }

    #[test]
    fn the_selection_stays_on_its_id_when_the_content_is_replaced() {
        let mut o = Open::new((true, "p".into()), "p".into(), "P".into());
        let d = list(&["a", "b", "c"]);
        o.key(KeyEvent::from(KeyCode::Down), SCREEN, Some(&d));
        let sel = |o: &Open, d: &Data| o.view.selected(&content(Some(d), &o.view, o.view.inner(SCREEN), &Theme::dark()));
        assert_eq!(sel(&o, &d), 1);
        assert_eq!(sel(&o, &list(&["x", "y", "z", "b"])), 3, "b moved: the selection went with it");
        assert_eq!(sel(&o, &list(&["x", "y", "z"])), 1, "b is gone: its old place");
        assert_eq!(sel(&o, &list(&["x"])), 0, "and never past the end");
        assert_eq!(o.key(KeyEvent::from(KeyCode::Enter), SCREEN, Some(&list(&["q", "b"]))), Outcome::Chosen("b".into()));
    }

    #[test]
    fn a_click_opens_a_branch_and_chooses_a_leaf() {
        let d = tree();
        let mut o = Open::new((true, "p".into()), "t".into(), "T".into());
        assert_eq!(labels(&d, &o), ["\u{25b8} src/", "c.rs:9"], "closed at first");
        assert_eq!(click_row(&mut o, &d, "src/"), Outcome::None);
        assert_eq!(labels(&d, &o), ["\u{25be} src/", "\u{25b8} deep/", "b.rs:1", "c.rs:9"]);
        assert_eq!(click_row(&mut o, &d, "deep/"), Outcome::None);
        assert_eq!(click_row(&mut o, &d, "a.rs:3"), Outcome::Chosen("a.rs:3".into()));
        assert_eq!(click_row(&mut o, &d, "src/"), Outcome::None);
        assert_eq!(labels(&d, &o), ["\u{25b8} src/", "c.rs:9"], "closed again");
        assert!(o.view.open.contains("deep"), "a branch inside keeps its state");

        // The filter looks at every depth, flat, and Esc empties it before it closes.
        for c in "rs:".chars() {
            o.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), SCREEN, Some(&d));
        }
        assert_eq!(labels(&d, &o), ["a.rs:3", "b.rs:1", "c.rs:9"]);
        assert_eq!(o.key(KeyEvent::from(KeyCode::Esc), SCREEN, Some(&d)), Outcome::None);
        assert_eq!(o.view.query, "");
        assert_eq!(o.key(KeyEvent::from(KeyCode::Esc), SCREEN, Some(&d)), Outcome::Close);

        // A click on the close button or outside closes; a detail panel does not filter.
        let mut o = Open::new((true, "p".into()), "d".into(), "D".into());
        let d = detail();
        o.key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE), SCREEN, Some(&d));
        assert_eq!(o.view.query, "");
        let drawn = o.draw(&mut Buffer::empty(SCREEN), SCREEN, &Theme::dark(), Some(&d));
        let down = |column, row| MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column, row, modifiers: KeyModifiers::NONE };
        assert_eq!(o.mouse(down(drawn.close.x + 1, drawn.close.y), SCREEN, Some(&d), &drawn), Outcome::Close);
        assert_eq!(o.mouse(down(0, 0), SCREEN, Some(&d), &drawn), Outcome::Close);
        assert_eq!(click_row(&mut o, &d, "a.rs:2"), Outcome::Chosen("a.rs:2".into()));
    }

    #[test]
    fn golden_panels_in_each_mode() {
        let theme = Theme::dark();
        let mut out = String::new();
        let mut roles = list(&["one", "two"]);
        roles.items[0].detail = "a detail".into();
        roles.items[1].role = Role::Warn;
        let mut open_tree = Open::new((true, "p".into()), "t".into(), "T".into());
        open_tree.view.open.extend(["src".to_string(), "deep".to_string()]);
        let screen = Rect::new(0, 0, 60, 16);
        for (tag, o, d) in [
            ("list", Open::new((true, "p".into()), "l".into(), "From the manifest".into()), Some(roles)),
            ("tree", open_tree, Some(tree())),
            ("detail", Open::new((true, "p".into()), "d".into(), "From the manifest".into()), Some(detail())),
            ("empty", Open::new((true, "p".into()), "e".into(), "Nothing sent".into()), None),
        ] {
            let mut buf = Buffer::empty(screen);
            let drawn = o.draw(&mut buf, screen, &theme, d.as_ref());
            out += &format!("== {tag} {:?} {:?}\n{}", drawn.rows, drawn.close, crate::app::snapshots::dump(&buf, screen));
        }
        crate::hook::tests::golden("ui/panels.txt", &out);
    }
}
