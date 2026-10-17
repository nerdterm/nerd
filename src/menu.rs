//! Popup menu and input line drawn over the frame.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use crate::render::Theme;

/// Menu item. The action is the caller's; the menu only hands it back.
pub struct Item<A> {
    pub label: String,
    pub action: A,
    pub enabled: bool,
    /// Hotkey, shown to the right of the label. Empty means don't show it.
    pub key: String,
    /// A yellow dot after the label: something is waiting behind this item.
    pub mark: bool,
}

impl<A> Item<A> {
    pub fn new(label: impl Into<String>, action: A) -> Self {
        Self {
            label: label.into(),
            action,
            enabled: true,
            key: String::new(),
            mark: false,
        }
    }

    /// A line between groups: never selected or chosen; `action` is never handed back.
    pub fn separator(action: A) -> Self {
        Self { enabled: false, ..Self::new("", action) }
    }

    pub fn key(mut self, key: &str) -> Self {
        self.key = key.into();
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn mark(mut self, mark: bool) -> Self {
        self.mark = mark;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome<A> {
    None,
    Cancelled,
    Chosen(A),
}

pub struct Menu<A> {
    pub items: Vec<Item<A>>,
    pub selected: usize,
    /// Recomputed from this every frame: the screen may have resized while open.
    anchor: (u16, u16),
}

impl<A: Copy> Menu<A> {
    pub fn new(items: Vec<Item<A>>, anchor: (u16, u16)) -> Self {
        let selected = items.iter().position(|i| i.enabled).unwrap_or(0);
        Self {
            items,
            selected,
            anchor,
        }
    }

    pub fn rect(&self, screen: Rect) -> Rect {
        let w = self.width().min(screen.width);
        let h = self.height().min(screen.height);
        // Shift inward instead of clipping: a clipped menu loses its right border.
        let x = self
            .anchor
            .0
            .min((screen.x + screen.width).saturating_sub(w))
            .max(screen.x);
        let y = self
            .anchor
            .1
            .min((screen.y + screen.height).saturating_sub(h))
            .max(screen.y);
        Rect::new(x, y, w, h)
    }

    pub fn draw(&self, buf: &mut Buffer, screen: Rect, theme: &Theme) {
        let r = self.rect(screen).intersection(buf.area);
        if r.is_empty() {
            return;
        }
        crate::chrome::dialog(buf, r, None, theme.border_focus, theme, crate::chrome::Moat::NotAbove);

        // rows run border to border so the selected one's highlight does too; the padding is in the label
        let pad = crate::chrome::DIALOG_FRAME.horizontal;
        let w = r.width.saturating_sub(2) as usize;
        let rows = r.height.saturating_sub(2) as usize;
        for (i, item) in self.items.iter().enumerate().take(rows) {
            let selected = i == self.selected && item.enabled;
            let style = if item.label.is_empty() && !item.enabled {
                Style::default().fg(theme.border).bg(theme.ground)
            } else if !item.enabled {
                Style::default().fg(theme.faint).bg(theme.ground)
            } else if i == self.selected {
                theme.cursor()
            } else {
                Style::default().fg(theme.text).bg(theme.ground)
            };
            // Pad to full width so the selected item's highlight reaches the border.
            let line: String = if item.label.is_empty() && !item.enabled {
                "─".repeat(w)
            } else {
                std::iter::repeat_n(' ', (pad - 1) as usize).chain(item.label.chars()).chain(std::iter::repeat(' ')).take(w).collect()
            };
            let y = r.y + 1 + i as u16;
            buf.set_stringn(r.x + 1, y, &line, w, style);
            if item.mark {
                let x = r.x + pad + item.label.chars().count() as u16 + 1;
                buf.set_stringn(x, y, "\u{25cf}", 1, if selected { style } else { style.fg(theme.warn) });
            }
            if !item.key.is_empty() {
                let kw = item.key.chars().count() as u16;
                let ks = if selected { style } else { style.fg(theme.muted) };
                buf.set_stringn(r.x + r.width.saturating_sub(pad + kw), y, &item.key, kw as usize, ks);
            }
        }
    }

    pub fn key(&mut self, k: crossterm::event::KeyEvent) -> Outcome<A> {
        use crossterm::event::KeyCode::*;
        let k = crate::keys::normalize(k);
        match k.code {
            Esc => Outcome::Cancelled,
            Up => {
                self.step(-1);
                Outcome::None
            }
            Down => {
                self.step(1);
                Outcome::None
            }
            Enter => self.choose(self.selected),
            Char(c @ '1'..='9') => {
                // Separators are not counted; a greyed row keeps its number but chooses nothing.
                let n = c as usize - '1' as usize;
                let row = self.items.iter().enumerate().filter(|(_, it)| !(it.label.is_empty() && !it.enabled)).nth(n);
                row.map_or(Outcome::None, |(i, _)| self.choose(i))
            }
            _ => Outcome::None,
        }
    }

    /// An item a click would choose is under this point.
    pub fn points_at(&self, screen: Rect, col: u16, row: u16) -> bool {
        self.item_at(self.rect(screen), col, row).is_some_and(|i| self.items[i].enabled)
    }

    pub fn mouse(&mut self, ev: crossterm::event::MouseEvent, screen: Rect) -> Outcome<A> {
        use crossterm::event::MouseEventKind::*;
        let r = self.rect(screen);
        let at = self.item_at(r, ev.column, ev.row);
        match ev.kind {
            Moved => {
                if let Some(i) = at.filter(|&i| self.items[i].enabled) {
                    self.selected = i;
                }
                Outcome::None
            }
            Down(_) => {
                if !r.contains((ev.column, ev.row).into()) {
                    return Outcome::Cancelled;
                }
                at.map_or(Outcome::None, |i| self.choose(i))
            }
            ScrollUp => {
                self.step(-1);
                Outcome::None
            }
            ScrollDown => {
                self.step(1);
                Outcome::None
            }
            _ => Outcome::None,
        }
    }

    fn width(&self) -> u16 {
        let longest = self
            .items
            .iter()
            .map(|i| i.label.chars().count())
            .max()
            .unwrap_or(0);
        // Room for a mark dot plus two spaces of key-column padding.
        let longest = longest + usize::from(self.items.iter().any(|i| i.mark)) * 2;
        let keys = self.items.iter().map(|i| i.key.chars().count()).max().unwrap_or(0);
        let keys = if keys > 0 { keys + 2 } else { 0 };
        // the padding on each side plus the border
        let frame = 2 * crate::chrome::DIALOG_FRAME.horizontal as usize;
        (longest + keys + frame).min(u16::MAX as usize) as u16
    }

    fn height(&self) -> u16 {
        (self.items.len().min(u16::MAX as usize) as u16).saturating_add(2)
    }

    /// Only inside the border counts - the border itself doesn't.
    fn item_at(&self, r: Rect, col: u16, row: u16) -> Option<usize> {
        let inside = col > r.x && col + 1 < r.x + r.width && row > r.y && row + 1 < r.y + r.height;
        inside
            .then(|| (row - r.y - 1) as usize)
            .filter(|&i| i < self.items.len())
    }

    fn choose(&self, i: usize) -> Outcome<A> {
        match self.items.get(i) {
            Some(it) if it.enabled => Outcome::Chosen(it.action),
            _ => Outcome::None,
        }
    }

    /// Wraps around, skipping disabled items; if none are enabled, selection stays put.
    fn step(&mut self, delta: isize) {
        let n = self.items.len();
        if n == 0 {
            return;
        }
        let mut i = self.selected as isize;
        for _ in 0..n {
            i = (i + delta).rem_euclid(n as isize);
            if self.items[i as usize].enabled {
                self.selected = i as usize;
                return;
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptOutcome {
    None,
    Cancelled,
    Submitted(String),
}

/// Input line drawn over the frame: renaming a tab, space, or pane.
pub struct Prompt {
    pub title: String,
    pub value: String,
    /// A faint line under the field.
    pub note: String,
    /// Position in characters, not bytes: the value can contain Cyrillic too.
    cursor: usize,
    /// What Enter does, named on its button.
    verb: &'static str,
}

impl Prompt {
    pub fn new(title: impl Into<String>, initial: impl Into<String>) -> Self {
        let value: String = initial.into();
        Self {
            title: title.into(),
            cursor: value.chars().count(),
            value,
            note: String::new(),
            verb: "save",
        }
    }

    pub fn does(mut self, verb: &'static str) -> Self {
        self.verb = verb;
        self
    }

    fn labels(&self) -> [String; 2] {
        [format!(" [Enter] {} ", self.verb), " [Esc] cancel ".into()]
    }

    /// Where the action and cancel buttons sit: the last row inside the frame, centred, one space apart.
    pub fn buttons(&self, screen: Rect) -> [Rect; 2] {
        let inner = self.rect(screen).inner(crate::chrome::DIALOG_FRAME);
        let labels = self.labels();
        let len = |i: usize| labels[i].chars().count() as u16;
        let total = len(0) + 1 + len(1);
        let x = inner.x + inner.width.saturating_sub(total) / 2;
        let y = inner.bottom().saturating_sub(1);
        let one = |at: u16, n: u16| Rect::new(at, y, n.min(inner.right().saturating_sub(at)), 1);
        [one(x, len(0)), one(x + len(0) + 1, len(1))]
    }

    /// A click on the action submits, on cancel or outside the window cancels.
    pub fn mouse(&self, ev: crossterm::event::MouseEvent, screen: Rect) -> PromptOutcome {
        if !matches!(ev.kind, crossterm::event::MouseEventKind::Down(_)) {
            return PromptOutcome::None;
        }
        let at = (ev.column, ev.row).into();
        let [act, cancel] = self.buttons(screen);
        if act.contains(at) {
            PromptOutcome::Submitted(self.value.trim().to_string())
        } else if cancel.contains(at) || !self.rect(screen).contains(at) {
            PromptOutcome::Cancelled
        } else {
            PromptOutcome::None
        }
    }

    pub fn note(mut self, note: &str) -> Self {
        self.note = note.into();
        self
    }

    /// One input line of a fixed width: the window must not resize while the text is typed.
    pub fn rect(&self, screen: Rect) -> Rect {
        let w = (self.title.chars().count() as u16 + 2).max(self.note.chars().count() as u16).max(44);
        crate::chrome::fit(screen, w, 1 + u16::from(!self.note.is_empty()))
    }

    pub fn draw(&self, buf: &mut Buffer, screen: Rect, theme: &Theme) {
        let r = self.rect(screen).intersection(buf.area);
        if r.is_empty() {
            return;
        }
        let content = crate::chrome::dialog(buf, r, Some(&self.title), theme.border_focus, theme, crate::chrome::Moat::All);
        let w = content.width as usize;
        if w == 0 || r.height < 5 {
            return;
        }
        let key = Style::default().fg(theme.border_focus).bg(theme.ground);
        for ((label, b), style) in self.labels().iter().zip(self.buttons(screen)).zip([key, Style::default().fg(theme.text).bg(theme.ground)]) {
            buf.set_stringn(b.x, b.y, label, b.width as usize, style);
        }
        let text: String = self
            .value
            .chars()
            .skip(self.offset(w))
            .chain(std::iter::repeat(' '))
            .take(w)
            .collect();
        buf.set_stringn(
            content.x,
            content.y,
            &text,
            w,
            Style::default().fg(theme.text).bg(theme.ground),
        );
        if !self.note.is_empty() && content.height > 1 {
            buf.set_stringn(content.x, content.y + 1, &self.note, w, Style::default().fg(theme.faint).bg(theme.ground));
        }
    }

    pub fn cursor(&self, screen: Rect) -> (u16, u16) {
        // the same inner rect the text is drawn into, or the caret lands beside the character it marks
        let inner = self.rect(screen).inner(crate::chrome::DIALOG_FRAME);
        let col = self.cursor - self.offset((inner.width as usize).max(1));
        (inner.x + col as u16, inner.y)
    }

    pub fn key(&mut self, k: crossterm::event::KeyEvent) -> PromptOutcome {
        use crossterm::event::{KeyCode::*, KeyModifiers};
        let len = self.value.chars().count();
        match k.code {
            Esc => return PromptOutcome::Cancelled,
            Enter => return PromptOutcome::Submitted(self.value.trim().to_string()),
            Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                self.value.clear();
                self.cursor = 0;
            }
            Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => {
                let at = self.byte(self.cursor);
                self.value.insert(at, c);
                self.cursor += 1;
            }
            Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                let at = self.byte(self.cursor);
                self.value.remove(at);
            }
            Delete if self.cursor < len => {
                let at = self.byte(self.cursor);
                self.value.remove(at);
            }
            Left => self.cursor = self.cursor.saturating_sub(1),
            Right => self.cursor = (self.cursor + 1).min(len),
            Home => self.cursor = 0,
            End => self.cursor = len,
            _ => {}
        }
        PromptOutcome::None
    }

    // Cursor can sit past the last character, so it needs one more slot than the chars before it.
    fn offset(&self, w: usize) -> usize {
        self.cursor.saturating_sub(w.saturating_sub(1))
    }

    fn byte(&self, ci: usize) -> usize {
        self.value
            .char_indices()
            .nth(ci)
            .map_or(self.value.len(), |(b, _)| b)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmOutcome {
    None,
    Yes,
    No,
    /// A third choice, offered only by dialogs that ask for one.
    Other,
}

/// What a menu answers, and what a picker answers - two different overlays with two different
/// sets, which the keys screen used to describe as one. These rows are what it prints; the
/// handlers below match the same codes.
pub const MENU_KEYS: &[(&str, &str, &str)] = &[
    ("↑↓", "", "up and down the items"),
    ("1..9", "", "the item with that number"),
    ("enter", "choose", "choose the selected item"),
    ("esc", "close", "close without choosing"),
];

pub const PICKER_KEYS: &[(&str, &str, &str)] = &[
    ("↑↓", "", "up and down the list"),
    ("control+n control+p", "", "the same, without leaving the letters"),
    ("enter", "choose", "choose the selected one"),
    ("esc", "close", "clear what was typed, then close"),
    ("any letter", "search", "narrows a long list as it is typed"),
];

pub const CONFIRM_KEYS: &[(&str, &str, &str)] = &[
    ("enter", "", "press the selected button"),
    ("y", "", "press the first button, the one that does it"),
    ("n, esc", "", "press the second button, which cancels"),
    ("d", "", "the third button, where a window has one"),
    ("←→ tab", "", "select another button"),
];

pub const FIELD_KEYS: &[(&str, &str, &str)] = &[
    ("enter", "", "submit the text"),
    ("esc", "", "cancel"),
    ("←→ Home End", "", "move in the text"),
    ("ctrl+u", "", "clear the line"),
];

const CONFIRM_YES_LABEL: &str = " [Enter] yes ";
const CONFIRM_NO_LABEL: &str = " [Esc] no ";
/// Upper bound on content width - with the border that's about 60 columns.
const CONFIRM_MAX_WIDTH: u16 = 58;

/// A button and what pressing it means; the label carries its own key hint.
struct ConfirmButton {
    label: String,
    outcome: ConfirmOutcome,
}

/// `danger` paints the border in `theme.bad`, for irreversible actions.
pub struct Confirm {
    pub title: String,
    pub body: String,
    /// What the dialog is about to take, on its own line above the body and in the accent.
    /// A sentence can be read past; a name on its own line has to be read.
    pub about: String,
    pub(crate) danger: bool,
    /// Rows under the body, the first a header; aligned, never wrapped.
    table: Vec<Vec<String>>,
    header: bool,
    /// Index into `buttons`; they are laid out left to right.
    selected: usize,
    buttons: Vec<ConfirmButton>,
    /// The third button's key, as its label shows it in brackets.
    other_key: Option<char>,
    /// Yes only by a click: a question that pops up while someone types must not take a stray Enter.
    click_yes: bool,
    /// When it opened: a click already on its way when it popped up must not land on yes.
    since: std::time::Instant,
}

/// How long a click-only question ignores clicks on yes after it opens.
const CLICK_AFTER: std::time::Duration = std::time::Duration::from_millis(400);

impl Confirm {
    pub fn new(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            body: body.into(),
            about: String::new(),
            danger: false,
            table: Vec::new(),
            header: true,
            selected: 0,
            buttons: vec![
                ConfirmButton { label: CONFIRM_YES_LABEL.into(), outcome: ConfirmOutcome::Yes },
                ConfirmButton { label: CONFIRM_NO_LABEL.into(), outcome: ConfirmOutcome::No },
            ],
            other_key: None,
            click_yes: false,
            since: std::time::Instant::now(),
        }
    }

    /// Names the thing this dialog would take: the pane by what runs in it, the tab by its
    /// name, a path by its path.
    pub fn about(mut self, what: impl Into<String>) -> Self {
        self.about = what.into();
        self
    }

    pub fn table(mut self, rows: Vec<Vec<String>>) -> Self {
        self.table = rows;
        self
    }

    /// A table without a header row.
    pub fn rows(mut self, rows: Vec<Vec<String>>) -> Self {
        self.header = false;
        self.table(rows)
    }

    /// The body wrapped to `width`, then a blank line and the table, when there is one.
    pub fn body_lines(&self, width: usize) -> Vec<String> {
        let mut lines = wrap(&self.body, width);
        if !self.table.is_empty() {
            lines.push(String::new());
            lines.extend(table_lines(&self.table, width));
        }
        lines
    }

    /// Replaces the default labels, for a dialog whose answer is not yes or no.
    pub fn labels(mut self, yes: impl Into<String>, no: impl Into<String>) -> Self {
        self.buttons[0].label = yes.into();
        self.buttons[1].label = no.into();
        self
    }

    /// A window that only tells: one button, `[Esc] close`, and Enter closes it too. Two buttons that both close
    /// read as a choice that is not there.
    pub fn info(mut self) -> Self {
        self.buttons = vec![ConfirmButton { label: " [Esc] close ".into(), outcome: ConfirmOutcome::No }];
        self.selected = 0;
        self
    }

    /// The button that acts is named after what it does; Esc always leaves the question.
    pub fn does(self, verb: &str) -> Self {
        self.labels(format!(" [Enter] {verb} "), " [Esc] cancel ")
    }

    /// A third button, returning [`ConfirmOutcome::Other`].
    pub fn other(mut self, label: impl Into<String>) -> Self {
        let label = label.into();
        self.other_key = label.split_once('[').and_then(|(_, k)| k.chars().next()).map(|c| c.to_ascii_lowercase());
        self.buttons.push(ConfirmButton { label, outcome: ConfirmOutcome::Other });
        self
    }

    /// The button the dialog opens on, by position: 1 is the second one.
    pub fn select(mut self, index: usize) -> Self {
        self.selected = index.min(self.buttons.len() - 1);
        self
    }

    pub fn danger(mut self, danger: bool) -> Self {
        self.danger = danger;
        self
    }

    pub fn rect(&self, screen: Rect) -> Rect {
        let frame = 2 * crate::chrome::DIALOG_FRAME.horizontal;
        let w = crate::chrome::fit(screen, self.content_width(screen), 0).width;
        let rows = self.body_lines(w.saturating_sub(frame).max(1) as usize).len() + usize::from(!self.about.is_empty());
        crate::chrome::fit(screen, self.content_width(screen), rows as u16)
    }

    /// Draws the dialog; returns where each button landed.
    pub fn draw(&self, buf: &mut Buffer, screen: Rect, theme: &Theme) -> Vec<Rect> {
        let r = self.rect(screen).intersection(buf.area);
        let mut rects = vec![Rect::default(); self.buttons.len()];
        if r.is_empty() {
            return rects;
        }

        // One colour says what kind of answer this is: red where the action takes something
        // away, the accent where it only asks. The frame, the named thing and the button that
        // would do it all wear it, so the three cannot disagree.
        let key = self.key_color(theme);
        let content = crate::chrome::dialog(buf, r, Some(&self.title), key, theme, crate::chrome::Moat::All);

        let inner_w = content.width as usize;
        if inner_w == 0 || r.height < 4 {
            return rects;
        }

        let mut y = content.y;
        let mut body_rows = r.height.saturating_sub(4) as usize;
        if !self.about.is_empty() && body_rows > 0 {
            buf.set_stringn(
                content.x,
                y,
                elide(&self.about, inner_w),
                inner_w,
                Style::default().fg(key).bg(theme.ground).add_modifier(Modifier::BOLD),
            );
            y += 1;
            body_rows -= 1;
        }
        let lines = self.body_lines(inner_w);
        let header = lines.len().saturating_sub(self.table.len());
        for (i, line) in lines.iter().enumerate().take(body_rows) {
            let style = Style::default().fg(theme.text).bg(theme.ground);
            buf.set_stringn(content.x, y + i as u16, line, inner_w, if i == header && self.header && !self.table.is_empty() { style.add_modifier(Modifier::BOLD) } else { style });
        }

        let btn_row = r.y + r.height - 2;
        let total = self.buttons_width();
        let mut x = content.x + (inner_w as u16).saturating_sub(total) / 2;
        for (i, (b, at)) in self.buttons.iter().zip(rects.iter_mut()).enumerate() {
            let len = b.label.chars().count() as u16;
            let room = inner_w.saturating_sub((x - content.x) as usize);
            if room == 0 {
                break;
            }
            buf.set_stringn(x, btn_row, &b.label, room, self.button_style(i, theme));
            *at = Rect::new(x, btn_row, len.min(room as u16), 1);
            x += len + 1;
        }
        rects
    }

    /// Every label, with one space between neighbours.
    fn buttons_width(&self) -> u16 {
        let labels: u16 = self.buttons.iter().map(|b| b.label.chars().count() as u16).sum();
        labels + self.buttons.len().saturating_sub(1) as u16
    }

    /// `y`/`n` pick directly, bypassing the selection.
    /// Yes only by a click; the dialog opens on the button that does not act.
    pub fn click_yes(mut self) -> Self {
        self.click_yes = true;
        self.since = std::time::Instant::now();
        self.select(1)
    }

    /// As if it had been open for `d`: tests click without waiting.
    #[cfg(test)]
    pub fn aged(&mut self, d: std::time::Duration) {
        self.since = std::time::Instant::now().checked_sub(d).unwrap_or(self.since);
    }

    pub fn key(&mut self, k: crossterm::event::KeyEvent) -> ConfirmOutcome {
        match self.key_any(k) {
            ConfirmOutcome::Yes if self.click_yes => ConfirmOutcome::None,
            out => out,
        }
    }

    fn key_any(&mut self, k: crossterm::event::KeyEvent) -> ConfirmOutcome {
        use crossterm::event::KeyCode::*;
        // yes, no and the third button are commands, so they are read off the key.
        let k = crate::keys::normalize(k);
        match k.code {
            Enter => self.buttons[self.selected].outcome,
            Esc => ConfirmOutcome::No,
            Char('y') | Char('Y') => ConfirmOutcome::Yes,
            // A first button named `[k] …` answers to its own letter too.
            Char(c) if self.buttons[0].label.trim_start().strip_prefix('[').and_then(|l| l.strip_prefix(c.to_ascii_lowercase())).is_some_and(|l| l.starts_with(']')) => ConfirmOutcome::Yes,
            Char('n') | Char('N') => ConfirmOutcome::No,
            // The key its label shows in brackets; without a third button there is nothing to answer.
            Char(c) if self.buttons.len() > 2 && Some(c.to_ascii_lowercase()) == self.other_key => ConfirmOutcome::Other,
            Left | BackTab => {
                self.selected = (self.selected + self.buttons.len() - 1) % self.buttons.len();
                ConfirmOutcome::None
            }
            Right | Tab => {
                self.selected = (self.selected + 1) % self.buttons.len();
                ConfirmOutcome::None
            }
            _ => ConfirmOutcome::None,
        }
    }

    /// `buttons`: where the last frame's `draw` put them.
    pub fn mouse(&mut self, ev: crossterm::event::MouseEvent, screen: Rect, buttons: &[Rect]) -> ConfirmOutcome {
        use crossterm::event::MouseEventKind::*;
        if !matches!(ev.kind, Down(_)) {
            return ConfirmOutcome::None;
        }
        let at = (ev.column, ev.row).into();
        if let Some(i) = buttons.iter().position(|r| r.contains(at)).filter(|&i| i < self.buttons.len()) {
            if self.click_yes && self.buttons[i].outcome == ConfirmOutcome::Yes && self.since.elapsed() < CLICK_AFTER {
                return ConfirmOutcome::None;
            }
            self.selected = i;
            return self.buttons[i].outcome;
        }
        if !self.rect(screen).contains(at) {
            return ConfirmOutcome::No;
        }
        ConfirmOutcome::None
    }

    /// Red for a dialog that takes something away, the accent for one that only asks.
    fn key_color(&self, theme: &Theme) -> Color {
        if self.danger { theme.bad } else { theme.border_focus }
    }

    fn button_style(&self, i: usize, theme: &Theme) -> Style {
        let key = self.key_color(theme);
        if self.selected == i {
            if self.danger { theme.cursor().bg(theme.bad) } else { theme.cursor() }
        } else if i == 0 {
            // The first button is the one that does it; it says so even while another is held.
            Style::default().fg(key).bg(theme.ground)
        } else {
            Style::default().fg(theme.text).bg(theme.ground)
        }
    }

    fn content_width(&self, screen: Rect) -> u16 {
        let buttons_w = self.buttons_width();
        // The cap is for prose; buttons are never cut, since a cut label hides its key.
        let avail = crate::chrome::limits(screen).0.saturating_sub(4).clamp(1, CONFIRM_MAX_WIDTH.max(buttons_w));
        let title_w = self.title.chars().count() as u16 + 2;
        let body_w =
            (self.body.chars().count().max(self.about.chars().count()) as u16).min(CONFIRM_MAX_WIDTH);
        // a table is not prose: it widens the dialog up to the screen rather than wrap a row
        let table_w = (table_width(&self.table) as u16).min(crate::chrome::limits(screen).0.saturating_sub(4));
        body_w.max(buttons_w).max(title_w).min(avail).max(table_w).max(1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerOutcome {
    None,
    Chosen(usize),
    Cancelled,
}

/// `detail` is a dim second column (e.g. a path); `id` is opaque, like `action` on `Item`.
pub struct PickerItem {
    pub label: String,
    pub detail: String,
    pub id: usize,
}

const PICKER_ROWS: usize = 15;

/// Lists up to this long show no search row.
const SEARCH_FROM: usize = 9;
const SEARCH: &str = "/ search";

pub struct Picker {
    pub title: String,
    items: Vec<PickerItem>,
    query: String,
    /// Index into the filtered list, not into `items` - its length changes on every keystroke.
    selected: usize,
    scroll: usize,
}

impl Picker {
    pub fn new(title: impl Into<String>, items: Vec<PickerItem>) -> Self {
        Self {
            title: title.into(),
            items,
            query: String::new(),
            selected: 0,
            scroll: 0,
        }
    }

    /// Case-insensitive substring match on label+detail; an empty query passes everything.
    fn filtered_indices(&self) -> Vec<usize> {
        if self.query.is_empty() {
            return (0..self.items.len()).collect();
        }
        let q = self.query.to_lowercase();
        self.items
            .iter()
            .enumerate()
            .filter(|(_, it)| format!("{} {}", it.label, it.detail).to_lowercase().contains(&q))
            .map(|(i, _)| i)
            .collect()
    }

    /// A list longer than this gets a search row; a shorter one is read at a glance.
    fn searchable(&self) -> bool {
        self.items.len() > SEARCH_FROM
    }

    /// Fits every item, not the filtered ones, so it does not move while typing.
    pub fn rect(&self, screen: Rect) -> Rect {
        let widest = self.items.iter().map(|i| 2 + i.label.chars().count() + if i.detail.is_empty() { 0 } else { 2 + i.detail.chars().count() }).max().unwrap_or(0);
        // the search row, then the rows, at least one for "no matches"
        let search = u16::from(self.searchable());
        crate::chrome::fit(screen, widest.max(SEARCH.chars().count()) as u16, search + self.items.len().clamp(1, PICKER_ROWS) as u16)
    }

    /// Draws the picker; returns the visible rows' rects, in `scroll..` order.
    pub fn draw(&self, buf: &mut Buffer, screen: Rect, theme: &Theme) -> Vec<Rect> {
        let r = self.rect(screen).intersection(buf.area);
        let mut rows = Vec::new();
        if r.is_empty() {
            return rows;
        }

        let content = crate::chrome::dialog(buf, r, Some(&self.title), theme.border_focus, theme, crate::chrome::Moat::All);
        crate::chrome::close_button(buf, r, theme);

        let inner_w = content.width as usize;
        if inner_w == 0 || r.height < 5 {
            return rows;
        }

        let filtered = self.filtered_indices();

        let search = u16::from(self.searchable());
        if search == 1 {
            // the count only while searching: before that it just repeats the list's length
            let counter = if self.query.is_empty() { String::new() } else { format!("{}/{}", filtered.len(), self.items.len()) };
            let counter_w = counter.chars().count().min(inner_w);
            let query_room = inner_w.saturating_sub(counter_w + 1);
            let (text, fg) = if self.query.is_empty() { (SEARCH.to_string(), theme.faint) } else { (format!("/{}", self.query), theme.text) };
            buf.set_stringn(content.x, r.y + 1, &text, query_room, Style::default().fg(fg).bg(theme.ground));
            buf.set_stringn(content.x + query_room as u16 + 1, r.y + 1, &counter, counter_w, Style::default().fg(theme.faint).bg(theme.ground));
        }

        let rows_shown = r.height.saturating_sub(4 + search) as usize;
        for i in 0..rows_shown {
            let Some(&idx) = filtered.get(self.scroll + i) else {
                break;
            };
            let item = &self.items[idx];
            let is_selected = self.scroll + i == self.selected;
            let row_y = r.y + 1 + search + i as u16;
            let style = if is_selected {
                theme.cursor()
            } else {
                Style::default().fg(theme.text).bg(theme.ground)
            };
            let line: String = " "
                .chars()
                .chain(item.label.chars())
                .chain(std::iter::repeat(' '))
                .take(inner_w)
                .collect();
            buf.set_stringn(content.x, row_y, &line, inner_w, style);

            if !item.detail.is_empty() {
                let label_w = item.label.chars().count() + 2;
                let room = inner_w.saturating_sub(label_w);
                if room > 0 {
                    let detail: String = item
                        .detail
                        .chars()
                        .rev()
                        .take(room)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect();
                    let detail_style = if is_selected {
                        style
                    } else {
                        Style::default().fg(theme.faint).bg(theme.ground)
                    };
                    let x = content.right() - detail.chars().count() as u16;
                    buf.set_stringn(x, row_y, &detail, room, detail_style);
                }
            }

            rows.push(Rect::new(content.x, row_y, inner_w as u16, 1));
        }
        rows
    }

    /// Esc first clears the query and only cancels once it's empty.
    pub fn key(&mut self, k: crossterm::event::KeyEvent) -> PickerOutcome {
        use crossterm::event::{KeyCode::*, KeyModifiers};
        match k.code {
            Esc => {
                if self.query.is_empty() {
                    return PickerOutcome::Cancelled;
                }
                self.query.clear();
                self.clamp_selected();
            }
            Enter => return self.choose(),
            Up => self.step(-1),
            Down => self.step(1),
            Char('n') if k.modifiers.contains(KeyModifiers::CONTROL) => self.step(1),
            Char('p') if k.modifiers.contains(KeyModifiers::CONTROL) => self.step(-1),
            Backspace => {
                self.query.pop();
                self.clamp_selected();
            }
            Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) && self.searchable() => {
                self.query.push(c);
                self.clamp_selected();
            }
            _ => {}
        }
        PickerOutcome::None
    }

    /// `rows`: where the last frame's `draw` put them.
    pub fn mouse(&mut self, ev: crossterm::event::MouseEvent, screen: Rect, rows: &[Rect]) -> PickerOutcome {
        use crossterm::event::MouseEventKind::*;
        match ev.kind {
            Down(_) => {
                let at = (ev.column, ev.row).into();
                for (i, row) in rows.iter().enumerate() {
                    if row.contains(at) {
                        self.selected = self.scroll + i;
                        return self.choose();
                    }
                }
                let r = self.rect(screen);
                if !r.contains(at) || crate::chrome::close_rect(r).contains(at) {
                    return PickerOutcome::Cancelled;
                }
                PickerOutcome::None
            }
            ScrollUp => {
                self.scroll = self.scroll.saturating_sub(1);
                PickerOutcome::None
            }
            ScrollDown => {
                let n = self.filtered_indices().len();
                self.scroll = (self.scroll + 1).min(n.saturating_sub(PICKER_ROWS));
                PickerOutcome::None
            }
            _ => PickerOutcome::None,
        }
    }

    fn choose(&self) -> PickerOutcome {
        self.filtered_indices()
            .get(self.selected)
            .and_then(|&idx| self.items.get(idx))
            .map_or(PickerOutcome::None, |it| PickerOutcome::Chosen(it.id))
    }

    fn step(&mut self, delta: isize) {
        let n = self.filtered_indices().len();
        if n == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(n as isize) as usize;
        self.ensure_visible();
    }

    fn ensure_visible(&mut self) {
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + PICKER_ROWS {
            self.scroll = self.selected + 1 - PICKER_ROWS;
        }
    }

    fn clamp_selected(&mut self) {
        let n = self.filtered_indices().len();
        self.selected = self.selected.min(n.saturating_sub(1));
        self.scroll = self.scroll.min(self.selected);
        self.ensure_visible();
    }
}

/// A word longer than `width` is cut character by character, not just dropped.
/// Keeps the end of a name that does not fit: a path is identified by what is at its end.
fn table_width(rows: &[Vec<String>]) -> usize {
    let n = rows.iter().map(Vec::len).max().unwrap_or(0);
    (0..n).map(|c| rows.iter().filter_map(|r| r.get(c)).map(|s| s.chars().count()).max().unwrap_or(0)).sum::<usize>() + 2 * n.saturating_sub(1)
}

/// Rows as aligned lines, columns two apart. Too wide for `width`, the last column goes, then the next,
/// down to one; a row is cut at the edge, never wrapped.
pub fn table_lines(rows: &[Vec<String>], width: usize) -> Vec<String> {
    let mut rows: Vec<Vec<String>> = rows.to_vec();
    while table_width(&rows) > width && rows.iter().any(|r| r.len() > 1) {
        for r in &mut rows {
            r.truncate(r.len().saturating_sub(1).max(1));
        }
    }
    let n = rows.iter().map(Vec::len).max().unwrap_or(0);
    let w: Vec<usize> = (0..n).map(|c| rows.iter().filter_map(|r| r.get(c)).map(|s| s.chars().count()).max().unwrap_or(0)).collect();
    rows.iter()
        .map(|r| {
            let line: String = r.iter().enumerate().map(|(c, s)| format!("{s:<0$}", w[c] + 2)).collect();
            line.trim_end().chars().take(width).collect()
        })
        .collect()
}

fn elide(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max || max <= 1 {
        return s.chars().take(max).collect();
    }
    format!("…{}", s.chars().skip(n - max + 1).collect::<String>())
}

pub fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let word_len = word.chars().count();
        if word_len > width {
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            let mut chars = word.chars();
            loop {
                let chunk: String = chars.by_ref().take(width).collect();
                if chunk.is_empty() {
                    break;
                }
                lines.push(chunk);
            }
            continue;
        }
        let candidate = if line.is_empty() {
            word_len
        } else {
            line.chars().count() + 1 + word_len
        };
        if candidate > width && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_only_question_takes_no_key_for_yes() {
        let k = |c| crossterm::event::KeyEvent::new(c, crossterm::event::KeyModifiers::NONE);
        let mut c = Confirm::new("Install a plugin", "x").does("install plug").click_yes();
        assert_eq!(c.key(k(crossterm::event::KeyCode::Enter)), ConfirmOutcome::No, "it opens on cancel");
        assert_eq!(c.key(k(crossterm::event::KeyCode::Char('y'))), ConfirmOutcome::None);
        c.key(k(crossterm::event::KeyCode::Left));
        assert_eq!(c.key(k(crossterm::event::KeyCode::Enter)), ConfirmOutcome::None, "not even with yes chosen");
        assert_eq!(Confirm::new("Close", "x").key(k(crossterm::event::KeyCode::Char('y'))), ConfirmOutcome::Yes, "other questions keep their keys");

        // A click already on its way when it opened does not land on yes; a later one does.
        let screen = Rect::new(0, 0, 100, 30);
        let mut c = Confirm::new("Install a plugin", "x").does("install plug").click_yes();
        let rects = c.draw(&mut Buffer::empty(screen), screen, &crate::render::Theme::dark());
        let click = crossterm::event::MouseEvent { kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left), column: rects[0].x + 1, row: rects[0].y, modifiers: crossterm::event::KeyModifiers::NONE };
        assert_eq!(c.mouse(click, screen, &rects), ConfirmOutcome::None, "too soon");
        c.aged(std::time::Duration::from_secs(1));
        assert_eq!(c.mouse(click, screen, &rects), ConfirmOutcome::Yes);
    }

    #[test]
    fn a_window_that_only_tells_has_one_button_and_enter_closes_it() {
        let mut c = Confirm::new("Message", "updated").info();
        assert_eq!(c.buttons.iter().map(|b| b.label.as_str()).collect::<Vec<_>>(), [" [Esc] close "]);
        let key = |code| crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE);
        assert_eq!(c.key(key(crossterm::event::KeyCode::Enter)), ConfirmOutcome::No);
        assert_eq!(c.key(key(crossterm::event::KeyCode::Right)), ConfirmOutcome::None, "nowhere else to go");
        assert_eq!(c.key(key(crossterm::event::KeyCode::Enter)), ConfirmOutcome::No);
    }
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };

    /// The hook dialog for these agents on a screen `cols` wide: its body as drawn.
    fn hook_dialog(agents: &[[&str; 3]], cols: u16) -> Vec<String> {
        let ask = crate::hook::Ask { rows: agents.iter().map(|r| r.map(String::from)).collect(), changed: false };
        let c = Confirm::new("Install the hook", ask.intro()).table(ask.table());
        let inner = c.rect(Rect::new(0, 0, cols, 40)).width - 2 * crate::chrome::DIALOG_FRAME.horizontal;
        c.body_lines(inner as usize)
    }

    const QWEN: [&str; 3] = ["Qwen Code", "~/.qwen/settings.json", "qwen --resume <id>"];
    const GEMINI: [&str; 3] = ["Gemini CLI", "~/.gemini/settings.json", "gemini --resume <id>"];
    const COPILOT: [&str; 3] = ["Copilot CLI", "~/.copilot/hooks/nerd.json", "copilot --resume <id>"];
    const CURSOR: [&str; 3] = ["Cursor", "~/.cursor/hooks.json", "no resume"];

    #[test]
    fn the_hook_dialog_lists_one_agent_a_row() {
        assert_eq!(
            hook_dialog(&[QWEN, CURSOR], 120),
            [
                "nerd adds its hook to these agents, so it sees when they",
                "work and when they wait for you.",
                "",
                "Agent      Settings file          Resumes with",
                "Qwen Code  ~/.qwen/settings.json  qwen --resume <id>",
                "Cursor     ~/.cursor/hooks.json   no resume",
            ]
        );
        assert_eq!(
            hook_dialog(&[QWEN, GEMINI, COPILOT], 120),
            [
                "nerd adds its hook to these agents, so it sees when they work",
                "and when they wait for you.",
                "",
                "Agent        Settings file               Resumes with",
                "Qwen Code    ~/.qwen/settings.json       qwen --resume <id>",
                "Gemini CLI   ~/.gemini/settings.json     gemini --resume <id>",
                "Copilot CLI  ~/.copilot/hooks/nerd.json  copilot --resume <id>",
            ]
        );
    }

    #[test]
    fn a_narrow_hook_dialog_drops_the_resume_column_rather_than_wrap_a_row() {
        let lines = hook_dialog(&[QWEN, GEMINI, COPILOT], 50);
        assert_eq!(
            lines[lines.len() - 4..],
            [
                "Agent        Settings file",
                "Qwen Code    ~/.qwen/settings.json",
                "Gemini CLI   ~/.gemini/settings.json",
                "Copilot CLI  ~/.copilot/hooks/nerd.json",
            ]
        );
    }

    fn theme() -> Theme {
        Theme::dark()
    }

    fn menu() -> Menu<u16> {
        Menu::new(
            vec![
                Item::new("New tab", 1),
                Item::new("Rename", 2),
                Item::new("Close", 3),
            ],
            (0, 0),
        )
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn text_at(buf: &Buffer, x: u16, y: u16, len: u16) -> String {
        (x..x + len).map(|i| buf[(i, y)].symbol()).collect()
    }

    #[test]
    fn box_is_drawn_with_the_selected_row_on_the_accent() {
        let screen = Rect::new(0, 0, 30, 10);
        let mut buf = Buffer::empty(screen);
        let th = theme();
        menu().draw(&mut buf, screen, &th);

        assert_eq!(buf[(0, 0)].symbol(), "┌");
        assert_eq!(buf[(10, 0)].symbol(), "┐");
        assert_eq!(buf[(0, 4)].symbol(), "└");
        assert_eq!(text_at(&buf, 1, 1, 8), " New tab");

        let sel = &buf[(1, 1)];
        assert_eq!(sel.bg, th.accent);
        assert_eq!(sel.fg, th.ground);
        assert!(!sel.modifier.contains(Modifier::BOLD), "the cursor changes the colour, never the weight");
        assert_eq!(buf[(0, 1)].fg, th.border_focus, "a menu has the keys, so its frame is lit");
        assert_eq!(buf[(9, 1)].bg, th.accent);

        let plain = &buf[(1, 2)];
        assert_eq!(plain.fg, th.text);
        assert_eq!(plain.bg, th.ground);
    }

    #[test]
    fn a_digit_counts_rows_but_not_separators_and_greyed_rows_keep_theirs() {
        let digit = |m: &mut Menu<i32>, c| m.key(key(KeyCode::Char(c)));
        let mut m = Menu::new(vec![Item::new("a", 1), Item::separator(0), Item::new("b", 2), Item::new("c", 3)], (0, 0));
        assert_eq!(digit(&mut m, '2'), Outcome::Chosen(2));
        assert_eq!(digit(&mut m, '3'), Outcome::Chosen(3));
        assert_eq!(digit(&mut m, '4'), Outcome::None);
        m.items[0].enabled = false;
        assert_eq!(digit(&mut m, '1'), Outcome::None, "a greyed row chooses nothing");
        assert_eq!(digit(&mut m, '2'), Outcome::Chosen(2), "later numbers do not shift");
    }

    #[test]
    fn menu_stays_on_screen_near_the_right_and_bottom_edges() {
        let screen = Rect::new(0, 0, 30, 10);
        let m = Menu::new(
            vec![Item::new("New tab", 1), Item::new("Rename", 2)],
            (28, 9),
        );
        let r = m.rect(screen);
        assert!(r.x + r.width <= 30, "{r:?}");
        assert!(r.y + r.height <= 10, "{r:?}");
        assert_eq!(r, Rect::new(19, 6, 11, 4));

        let tiny = Rect::new(0, 0, 1, 1);
        assert_eq!(m.rect(tiny), Rect::new(0, 0, 1, 1));
        let mut buf = Buffer::empty(tiny);
        m.draw(&mut buf, tiny, &theme());
    }

    #[test]
    fn arrows_wrap_around_and_skip_disabled_items() {
        let mut m = menu();
        m.items[1].enabled = false;

        assert_eq!(m.selected, 0);
        m.key(key(KeyCode::Down));
        assert_eq!(m.selected, 2, "disabled item is skipped over");
        m.key(key(KeyCode::Down));
        assert_eq!(m.selected, 0, "wraps at the bottom");
        m.key(key(KeyCode::Up));
        assert_eq!(m.selected, 2, "wraps at the top");
        m.key(key(KeyCode::Up));
        assert_eq!(m.selected, 0);

        assert_eq!(m.key(key(KeyCode::Enter)), Outcome::Chosen(1));
        assert_eq!(m.key(key(KeyCode::Esc)), Outcome::Cancelled);
        assert_eq!(m.key(key(KeyCode::Char('2'))), Outcome::None);
    }

    #[test]
    fn a_digit_picks_an_item() {
        let mut m = menu();
        assert_eq!(m.key(key(KeyCode::Char('3'))), Outcome::Chosen(3));
        assert_eq!(m.key(key(KeyCode::Char('9'))), Outcome::None);
    }

    #[test]
    fn disabled_item_is_drawn_faint() {
        let screen = Rect::new(0, 0, 30, 10);
        let mut buf = Buffer::empty(screen);
        let th = theme();
        let mut m = menu();
        m.items[1].enabled = false;
        m.draw(&mut buf, screen, &th);
        assert_eq!(buf[(1, 2)].fg, th.faint);
    }

    #[test]
    fn hovering_selects_and_a_click_outside_cancels() {
        let screen = Rect::new(0, 0, 30, 10);
        let mut m = menu();

        assert_eq!(
            m.mouse(mouse(MouseEventKind::Moved, 4, 3), screen),
            Outcome::None
        );
        assert_eq!(m.selected, 2, "highlight follows the cursor without a click");

        assert_eq!(
            m.mouse(mouse(MouseEventKind::Down(MouseButton::Left), 4, 2), screen),
            Outcome::Chosen(2)
        );
        assert_eq!(
            m.mouse(
                mouse(MouseEventKind::Down(MouseButton::Left), 25, 8),
                screen
            ),
            Outcome::Cancelled
        );

        m.mouse(mouse(MouseEventKind::ScrollUp, 4, 2), screen);
        assert_eq!(m.selected, 1);
    }

    #[test]
    fn prompt_inserts_and_deletes_at_the_cursor() {
        let mut p = Prompt::new("Rename", "abc");
        p.key(key(KeyCode::Left));
        p.key(key(KeyCode::Char('X')));
        assert_eq!(p.value, "abXc");

        p.key(key(KeyCode::Backspace));
        assert_eq!(p.value, "abc");
        p.key(key(KeyCode::Home));
        p.key(key(KeyCode::Delete));
        assert_eq!(p.value, "bc");

        p.key(key(KeyCode::End));
        p.key(key(KeyCode::Char('д')));
        assert_eq!(p.value, "bcд", "a multi-byte char doesn't break the string");

        p.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(p.value, "");
        assert_eq!(p.key(key(KeyCode::Backspace)), PromptOutcome::None);
    }

    #[test]
    fn enter_submits_trimmed_and_esc_cancels() {
        let mut p = Prompt::new("Rename", "  hi  ");
        assert_eq!(
            p.key(key(KeyCode::Enter)),
            PromptOutcome::Submitted("hi".into())
        );
        assert_eq!(p.key(key(KeyCode::Esc)), PromptOutcome::Cancelled);

        let mut empty = Prompt::new("Rename", "");
        assert_eq!(
            empty.key(key(KeyCode::Enter)),
            PromptOutcome::Submitted(String::new())
        );
    }

    #[test]
    fn a_long_value_scrolls_instead_of_overflowing() {
        let screen = Rect::new(0, 0, 20, 9);
        let mut buf = Buffer::empty(screen);
        let th = theme();
        let p = Prompt::new("Rename", "0123456789abcdefghij");
        let r = p.rect(screen);
        assert_eq!(r, Rect::new(1, 2, 18, 5), "the screen less 1/16 a side, centred");

        p.draw(&mut buf, screen, &th);
        let inner = r.inner(crate::chrome::DIALOG_FRAME);
        assert_eq!(inner.width, 14, "the frame takes two cells a side, border and padding");
        assert_eq!(text_at(&buf, inner.x, inner.y, inner.width), "789abcdefghij ");
        assert_eq!(buf[(r.x, r.y + 1)].symbol(), "│");
        assert_eq!(buf[(r.x + r.width - 1, r.y + 1)].symbol(), "│");
        assert_eq!(buf[(r.x + 1, r.y + 1)].symbol(), " ", "a padding cell inside the left border");
        assert_eq!(buf[(r.x + r.width - 2, r.y + 1)].symbol(), " ", "and the same inside the right");
        assert_eq!(p.cursor(screen), (inner.right() - 1, inner.y), "the caret sits on the last visible cell");

        assert_eq!(text_at(&buf, r.x, r.y, 12), "┌─ Rename ──");
    }

    #[test]
    fn prompt_survives_a_one_cell_screen() {
        let screen = Rect::new(0, 0, 1, 1);
        let mut buf = Buffer::empty(screen);
        let p = Prompt::new("Rename", "abc");
        p.draw(&mut buf, screen, &theme());
        assert_eq!(p.rect(screen), Rect::new(0, 0, 1, 1));
    }

    fn confirm() -> Confirm {
        Confirm::new("Close pane?", "The agent is still running.")
    }

    #[test]
    fn enter_picks_the_selected_button_y_n_pick_directly() {
        let mut c = confirm();
        assert_eq!(
            c.key(key(KeyCode::Enter)),
            ConfirmOutcome::Yes,
            "\"yes\" is selected by default"
        );
        assert_eq!(c.key(key(KeyCode::Esc)), ConfirmOutcome::No);
        assert_eq!(c.key(key(KeyCode::Char('y'))), ConfirmOutcome::Yes);
        assert_eq!(c.key(key(KeyCode::Char('n'))), ConfirmOutcome::No);
    }

    #[test]
    fn left_and_right_switch_the_selected_button() {
        let mut c = confirm();
        assert_eq!(c.key(key(KeyCode::Left)), ConfirmOutcome::None);
        assert_eq!(
            c.key(key(KeyCode::Enter)),
            ConfirmOutcome::No,
            "selection switched to \"no\""
        );
        assert_eq!(c.key(key(KeyCode::Right)), ConfirmOutcome::None);
        assert_eq!(
            c.key(key(KeyCode::Enter)),
            ConfirmOutcome::Yes,
            "switched back to \"yes\""
        );
    }

    #[test]
    fn three_buttons_are_drawn_whole() {
        let screen = Rect::new(0, 0, 100, 12);
        let mut buf = Buffer::empty(screen);
        let c = Confirm::new("inside tmux", "short")
            .labels(" [Enter] open a new Ghostty window ", " [Esc] stay here ")
            .other(" [d] never ask ");
        let rects = c.draw(&mut buf, screen, &theme());
        for (i, (b, r)) in c.buttons.iter().zip(rects).enumerate() {
            assert_eq!(r.width as usize, b.label.chars().count(), "button {i} is cut: {:?}", b.label);
        }
    }

    #[test]
    fn a_third_button_answers_for_itself() {
        let screen = Rect::new(0, 0, 60, 12);
        let mut buf = Buffer::empty(screen);
        let th = theme();
        let mut c = Confirm::new("inside tmux", "blocks here; a window outside shows pixels")
            .labels(" [Enter] open ", " [Esc] stay ")
            .other(" [d] never ask ")
            .select(1);
        let rects = c.draw(&mut buf, screen, &th);
        assert_eq!(c.key(key(KeyCode::Enter)), ConfirmOutcome::No, "the second button is the one selected");

        assert_eq!(c.key(key(KeyCode::Char('d'))), ConfirmOutcome::Other, "the label's key works");
        let never = rects[2];
        assert!(!never.is_empty(), "the third button is drawn");
        assert_eq!(
            c.mouse(mouse(MouseEventKind::Down(MouseButton::Left), never.x, never.y), screen, &rects),
            ConfirmOutcome::Other
        );
        // The click left the third button selected; Tab wraps round to the first.
        c.key(key(KeyCode::Tab));
        assert_eq!(c.key(key(KeyCode::Enter)), ConfirmOutcome::Yes, "past the last comes the first");
        assert_eq!(c.key(key(KeyCode::Esc)), ConfirmOutcome::No, "esc always stays");
    }

    #[test]
    fn mouse_click_on_a_button_picks_it_and_outside_is_no() {
        let screen = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(screen);
        let th = theme();
        let mut c = confirm();
        let rects = c.draw(&mut buf, screen, &th);

        let (yes, no) = (rects[0], rects[1]);
        assert!(!yes.is_empty(), "the \"yes\" button should have been drawn");
        assert!(!no.is_empty(), "the \"no\" button should have been drawn");
        assert!(yes.x < no.x, "\"yes\" is to the left of \"no\"");

        assert_eq!(
            c.mouse(mouse(MouseEventKind::Down(MouseButton::Left), no.x, no.y), screen, &rects),
            ConfirmOutcome::No
        );
        assert_eq!(
            c.mouse(mouse(MouseEventKind::Down(MouseButton::Left), yes.x, yes.y), screen, &rects),
            ConfirmOutcome::Yes
        );
        assert_eq!(
            c.mouse(mouse(MouseEventKind::Down(MouseButton::Left), 0, 0), screen, &rects),
            ConfirmOutcome::No,
            "a click outside the dialog also cancels"
        );
    }

    #[test]
    fn danger_paints_the_border_the_name_and_the_doing_button_red() {
        let screen = Rect::new(0, 0, 40, 10);
        let th = theme();

        let mut buf = Buffer::empty(screen);
        let c = confirm().about("README.md").danger(true);
        let r = c.rect(screen);
        let yes = c.draw(&mut buf, screen, &th)[0];
        assert_eq!(buf[(r.x, r.y)].fg, th.bad, "the frame");
        assert_eq!(buf[(r.x + 3, r.y)].fg, th.bad, "the title on it");
        assert_eq!(buf[(r.x + 2, r.y + 1)].fg, th.bad, "the thing it would take");
        assert_eq!(buf[(yes.x, yes.y)].bg, th.bad, "and the button that would do it");

        // a dialog that only asks wears the accent in the same three places
        let mut buf = Buffer::empty(screen);
        let c = confirm().about("README.md");
        let r = c.rect(screen);
        let yes = c.draw(&mut buf, screen, &th)[0];
        assert_eq!(buf[(r.x, r.y)].fg, th.border_focus, "the frame");
        assert_eq!(buf[(r.x + 2, r.y + 1)].fg, th.border_focus, "the name");
        assert_eq!(buf[(yes.x, yes.y)].bg, th.border_focus, "the button");
    }

    #[test]
    fn the_named_thing_leads_the_dialog_and_takes_the_accent() {
        let screen = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(screen);
        let th = theme();
        let c = confirm().about("build");
        let r = c.rect(screen);
        c.draw(&mut buf, screen, &th);

        // the name is the first line inside the frame, and the body moved under it
        let line: String = (r.x + 2..r.right() - 2).map(|x| buf[(x, r.y + 1)].symbol()).collect();
        assert!(line.starts_with("build"), "{line:?}");
        assert_eq!(buf[(r.x + 2, r.y + 1)].fg, th.border_focus, "the thing about to be taken is accented");

        let under: String = (r.x + 2..r.right() - 2).map(|x| buf[(x, r.y + 2)].symbol()).collect();
        assert!(!under.trim().is_empty(), "the body still shows: {under:?}");
        assert_eq!(buf[(r.x + 2, r.y + 2)].fg, th.text, "and stays plain text");
    }

    #[test]
    fn a_long_body_wraps_onto_several_lines() {
        assert_eq!(wrap("aa bb cc dd", 5), vec!["aa bb", "cc dd"]);
        assert_eq!(wrap("короткий текст", 30), vec!["короткий текст"]);

        let word = "суперкалифрагилистикэкспиалидоциус";
        let lines = wrap(word, 5);
        assert!(lines.iter().all(|l| l.chars().count() <= 5));
        assert_eq!(lines.concat().chars().count(), word.chars().count());

        let screen = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(screen);
        let c = Confirm::new(
            "Delete everything?",
            "This action is irreversible and affects every open tab in every space.",
        );
        c.draw(&mut buf, screen, &theme());
        let r = c.rect(screen);
        assert!(r.height > 5, "body took more than one line: {r:?}");
    }

    #[test]
    fn confirm_survives_a_tiny_screen() {
        let screen = Rect::new(0, 0, 1, 1);
        let mut buf = Buffer::empty(screen);
        let mut c = confirm();
        let rects = c.draw(&mut buf, screen, &theme());
        assert_eq!(c.rect(screen), Rect::new(0, 0, 1, 1));
        assert_eq!(
            c.mouse(mouse(MouseEventKind::Down(MouseButton::Left), 0, 0), screen, &rects),
            ConfirmOutcome::None
        );
    }

    fn picker() -> Picker {
        Picker::new(
            "Open worktree",
            vec![
                PickerItem { label: "main".into(), detail: "/repo".into(), id: 10 },
                PickerItem { label: "feature-x".into(), detail: "/repo-feature-x".into(), id: 20 },
                PickerItem { label: "bugfix".into(), detail: "/repo-bugfix".into(), id: 30 },
            ]
            .into_iter()
            .chain((0..SEARCH_FROM).map(|n| PickerItem { label: format!("old-{n}"), detail: format!("/repo-old-{n}"), id: 100 + n }))
            .collect(),
        )
    }

    #[test]
    fn a_short_list_has_no_search_row_and_a_long_one_says_what_its_row_is() {
        let screen = Rect::new(0, 0, 80, 30);
        let text = |p: &Picker| {
            let mut buf = Buffer::empty(screen);
            p.draw(&mut buf, screen, &Theme::dark());
            let r = p.rect(screen);
            (r, (r.x..r.right()).map(|x| buf[(x, r.y + 1)].symbol()).collect::<String>())
        };
        let short = Picker::new("Export logs", ["Last hour", "Last day", "Last week"].iter().enumerate().map(|(i, l)| PickerItem { label: l.to_string(), detail: String::new(), id: i }).collect());
        let (r, first) = text(&short);
        assert!(first.contains("Last hour") && !first.contains('/'), "the list starts at once: {first:?}");
        assert_eq!(r.height, 2 + 3 + 2, "frame, three rows, blank row and close");
        let mut s = short;
        s.key(key(KeyCode::Char('d')));
        assert!(s.query.is_empty(), "nothing to search in three rows");

        let mut long = picker();
        let (_, first) = text(&long);
        assert!(first.contains("/ search"), "the row says what it is: {first:?}");
        assert!(!first.contains(&format!("/{}", long.items.len())), "no count before searching: {first:?}");
        long.key(key(KeyCode::Char('b')));
        let (_, first) = text(&long);
        assert!(first.contains("/b") && first.contains(&format!("1/{}", long.items.len())), "{first:?}");
    }

    #[test]
    fn typing_narrows_the_list_by_label_or_detail() {
        let mut p = picker();
        for c in "feat".chars() {
            p.key(key(KeyCode::Char(c)));
        }
        assert_eq!(p.filtered_indices(), vec![1], "substring match in label is case-insensitive");
    }

    #[test]
    fn enter_returns_the_id_of_the_visible_selection_not_the_raw_index() {
        let mut p = picker();
        for c in "bug".chars() {
            p.key(key(KeyCode::Char(c)));
        }
        assert_eq!(p.key(key(KeyCode::Enter)), PickerOutcome::Chosen(30));
    }

    #[test]
    fn esc_clears_the_query_first_then_cancels() {
        let mut p = picker();
        p.key(key(KeyCode::Char('x')));
        assert_eq!(p.key(key(KeyCode::Esc)), PickerOutcome::None, "first esc clears the query");
        assert_eq!(p.query, "");
        assert_eq!(p.key(key(KeyCode::Esc)), PickerOutcome::Cancelled, "second esc cancels");
    }

    #[test]
    fn empty_list_does_not_panic() {
        let screen = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(screen);
        let mut p = Picker::new("Empty", Vec::new());
        p.draw(&mut buf, screen, &theme());
        assert_eq!(p.key(key(KeyCode::Enter)), PickerOutcome::None);
        assert_eq!(p.key(key(KeyCode::Down)), PickerOutcome::None);
    }

    #[test]
    fn mouse_click_on_a_row_chooses_it() {
        let screen = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(screen);
        let mut p = picker();
        let rows = p.draw(&mut buf, screen, &theme());
        let row = rows[1];
        assert_eq!(
            p.mouse(mouse(MouseEventKind::Down(MouseButton::Left), row.x, row.y), screen, &rows),
            PickerOutcome::Chosen(20)
        );
    }

    #[test]
    fn picker_survives_a_tiny_screen() {
        let screen = Rect::new(0, 0, 1, 1);
        let mut buf = Buffer::empty(screen);
        let p = picker();
        p.draw(&mut buf, screen, &theme());
        assert_eq!(p.rect(screen), Rect::new(0, 0, 1, 1));
    }
}
