use super::*;

/// The strip at the bottom: the keys of one table, grouped, whole entries only - the group's
/// word muted, each key in the text colour and what it does after it, dimmer.
/// A key's width in the strip: its label, and the word that glosses it where it has one.
fn key_width(r: &Key) -> u16 {
    (r.label.chars().count() + if r.word.is_empty() { 0 } else { r.word.chars().count() + 1 }) as u16
}

/// Where each group of the strip lands, within the rows it is allowed. A group that does not
/// fit after what is already on the row moves to the next one rather than being cut there: a
/// row ending inside a group reads as the end of the list, which is how `go` and `move` went
/// unnamed anywhere but the keys screen. A group wider than the whole row still starts one,
/// and `strip` cuts it between keys - some of `do` beats none of it.
fn strip_layout(width: u16, rows: u16, table: &'static [Key]) -> Vec<(u16, u16, &'static str, Vec<&'static Key>)> {
    let mut out = Vec::new();
    let (mut row, mut x) = (0u16, 0u16);
    for (group, word) in [(Group::Do, "do"), (Group::Go, "go"), (Group::Move, "move")] {
        let keys: Vec<&Key> = table.iter().filter(|r| r.group == group && !r.label.is_empty()).collect();
        if keys.is_empty() {
            continue;
        }
        // The two after the word and after every key are the gaps between them; the last gap
        // falls off the end of the row, so it is not counted.
        let w = word.chars().count() as u16 + 2 + keys.iter().map(|r| key_width(r) + 2).sum::<u16>() - 2;
        if x > 0 && x + w > width {
            if row + 1 >= rows {
                continue;
            }
            row += 1;
            x = 0;
        }
        out.push((row, x, word, keys));
        x += w + 2;
    }
    out
}

/// How many rows the strip wants at this width: one, or two when a group would otherwise be
/// dropped. Asked before the list is given its height, since the rows come out of the list.
fn strip_rows(width: u16, table: &'static [Key]) -> u16 {
    let two = strip_layout(width, 2, table);
    if two.iter().any(|(row, ..)| *row > 0) { 2 } else { 1 }
}

fn strip(buf: &mut Buffer, x0: u16, y: u16, width: u16, rows: u16, table: &'static [Key], theme: &Theme) {
    let end = x0 + width;
    for (row, at, word, keys) in strip_layout(width, rows, table) {
        let y = y + row;
        let mut x = x0 + at;
        if x + word.chars().count() as u16 + 1 > end {
            continue;
        }
        buf.set_string(x, y, word, Style::default().fg(theme.faint));
        x += word.chars().count() as u16 + 2;
        for r in keys {
            // Whole entries only: half a key name says nothing and looks like damage.
            if x + key_width(r) > end {
                break;
            }
            buf.set_string(x, y, r.label, Style::default().fg(theme.text));
            if !r.word.is_empty() {
                buf.set_string(x + r.label.chars().count() as u16 + 1, y, r.word, Style::default().fg(theme.faint));
            }
            x += key_width(r) + 2;
        }
    }
}

impl Tree {
    /// Paints the preview's selection over what was just drawn, and takes its text while the
    /// cells are there to read: the preview has no screen of its own to go back to.
    fn paint_selection(&self, v: &mut TreeView, buf: &mut Buffer, area: Rect, theme: &Theme) {
        let Some(span) = v.psel else { return };
        let take = std::mem::take(&mut v.psel_copy);
        // The line numbers are drawn, not read from the file: the selection starts after them,
        // wherever the drag began.
        let text_x = area.x + self.preview.as_ref().map(|p| p.gutter()).unwrap_or(0);
        let mut text = String::new();
        for y in area.y..area.bottom() {
            let mut line = String::new();
            let mut any = false;
            for x in text_x..area.right() {
                if !crate::select::in_span(span, y, x) {
                    continue;
                }
                any = true;
                if let Some(cell) = buf.cell_mut((x, y)) {
                    line.push_str(cell.symbol());
                    cell.set_bg(theme.fill).set_fg(theme.on_fill);
                }
            }
            if take && any {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(line.trim_end());
            }
        }
        if take && !text.is_empty() {
            v.pending_copy = Some(text);
        }
    }

    /// The tree and its preview in one frame: they are one thing, and the frame says so. The
    /// halves are split by a seam, and one hint strip runs under both — in a single column it
    /// only ever had room for its first group.
    pub fn draw_pair(&self, v: &mut TreeView, buf: &mut Buffer, area: Rect, theme: &Theme, tree_w: u16) -> Pair {
        v.hits.clear();
        v.tree_area = Rect::ZERO;
        v.prev_area = Rect::ZERO;
        v.btn_close = Rect::ZERO;
        v.btn_zoom = Rect::ZERO;
        v.list_h = 0;
        let Some(inner) = panel(buf, area, theme, "", self.active) else {
            return Pair::default();
        };
        let frame = area;
        // The title is the switcher: the view in hand is bold, the other waits, both
        // answer a click. In `changes` one more word names what a click on it does to the shape.
        v.btn_files = Rect::ZERO;
        v.btn_changes = Rect::ZERO;
        v.btn_shape = Rect::ZERO;
        let mut words: Vec<(&str, bool)> = vec![("files", self.view == View::Files), ("changes", self.view == View::Changes)];
        if self.view == View::Changes {
            words.push((if self.changes_flat { "as a tree" } else { "as a list" }, false));
        }
        // A word that would run into the buttons at the end is left out, not cut in the middle.
        let edge = inner.right().saturating_sub(if inner.width >= 14 { 8 } else { 0 });
        let mut at = inner.x + 1;
        let fits = words.iter().take_while(|(w, _)| {
            let ok = at + w.chars().count() as u16 + 2 <= edge;
            at += w.chars().count() as u16 + 3;
            ok
        }).count();
        words.truncate(fits);
        // The border runs under the whole title, so the gaps are cleared before the words go on.
        let bar = format!(" {} ", words.iter().map(|(w, _)| *w).collect::<Vec<_>>().join(" · "));
        buf.set_stringn(inner.x, area.y, &bar, inner.width as usize, Style::default().fg(theme.border));
        let mut x = inner.x + 1;
        for (i, (word, on)) in words.iter().enumerate() {
            let w = word.chars().count() as u16;
            // the view in hand is chosen, so bold; its colour follows the frame like any title
            let st = match (on, i < 2) {
                (true, true) => Style::default().fg(if self.active { theme.accent } else { theme.text }).add_modifier(Modifier::BOLD),
                (true, false) => Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
                _ => Style::default().fg(theme.muted),
            };
            buf.set_stringn(x, area.y, word, w as usize, st);
            let hit = Rect::new(x, area.y, w, 1);
            match i {
                0 => v.btn_files = hit,
                1 => v.btn_changes = hit,
                _ => v.btn_shape = hit,
            }
            x += w + 1;
            if i + 1 < words.len() {
                buf.set_stringn(x, area.y, "·", 1, Style::default().fg(theme.border));
                x += 2;
            }
        }
        // Full screen and close at the title line's end, so neither needs a key. `⊡` rather
        // than `□`: a squared operator sits on the same optical line as `×`, a geometric square
        // sits lower, and the two are read as a pair.
        if inner.width >= 14 {
            let x = inner.right().saturating_sub(7);
            buf.set_stringn(x, area.y, " \u{22a1} \u{b7} \u{d7} ", 7, Style::default().fg(theme.muted));
            buf.set_stringn(x + 3, area.y, "\u{b7}", 1, Style::default().fg(theme.border));
            if self.preview_on {
                v.btn_zoom = Rect::new(x, area.y, 3, 1);
            } else {
                buf.set_stringn(x, area.y, "   ", 3, Style::default().fg(theme.muted));
            }
            v.btn_close = Rect::new(x + 4, area.y, 3, 1);
        }

        // The strip belongs to the pair, and names the keys of the half that holds them.
        let mut body = inner;
        if self.active && body.height >= 3 {
            let table = if self.focus == Focus::Preview { PREVIEW } else { COMMAND };
            // The second row comes out of the list, so it is taken only where the list can
            // spare it - a panel with four rows left is a panel that stops being a tree.
            let rows = if body.height >= 8 { strip_rows(inner.width, table) } else { 1 };
            body.height -= rows;
            strip(buf, inner.x, inner.bottom() - rows, inner.width, rows, table, theme);
        }

        let seam_style = Style::default().fg(theme.border).bg(theme.ground);
        if self.preview_on && body.width >= tree_w + 25 {
            // one column of padding on each side of the seam, as inside the frame itself
            let seam = body.x + tree_w + 1;
            // The frame's own line is never overwritten: a junction drawn into it reads as a
            // break in one object, and leaves a grey notch when the frame is lit.
            for y in body.y..body.bottom() {
                buf.set_string(seam, y, "\u{2502}", seam_style);
            }
            let tree = Rect { width: tree_w, ..body };
            let preview = Rect { x: seam + 2, width: body.right() - seam - 2, ..body };
            self.draw_rows(v, buf, tree, theme);
            self.draw_preview_body(v, buf, preview, theme);
            v.tree_area = tree;
            v.prev_area = preview;
            return Pair { frame, tree, preview: Some(preview), seam: Some(seam) };
        }

        if !self.preview_on || body.height < 8 {
            self.draw_rows(v, buf, body, theme);
            v.tree_area = body;
            return Pair { frame, tree: body, preview: None, seam: None };
        }
        // Too narrow for two columns: the seam lies down instead, and the frame stays one.
        let th = (body.height * 3 / 5).max(3);
        let rule = body.y + th;
        for x in body.x..body.right() {
            buf.set_string(x, rule, "\u{2500}", seam_style);
        }
        let tree = Rect { height: th, ..body };
        let preview = Rect { y: rule + 1, height: body.bottom() - rule - 1, ..body };
        self.draw_rows(v, buf, tree, theme);
        self.draw_preview_body(v, buf, preview, theme);
        v.tree_area = tree;
        v.prev_area = preview;
        Pair { frame, tree, preview: Some(preview), seam: None }
    }

    /// The preview half: header with path and kind, the content, and the line it sits on.
    fn draw_preview_body(&self, v: &mut TreeView, buf: &mut Buffer, inner: Rect, theme: &Theme) {
        let focused = self.focus == Focus::Preview && self.active;
        let Some((path, line, size)) = self.rows.get(self.selected).map(|r| (r.path.clone(), r.line, r.size))
        else {
            let y = inner.y + inner.height / 2;
            let x = inner.x + inner.width.saturating_sub(13) / 2;
            buf.set_stringn(x, y, "select a file", inner.width as usize, Style::default().fg(theme.faint));
            return;
        };
        if inner.width < 3 || inner.height == 0 {
            return;
        }
        // Only past the early returns: without a file or room to show it, an image sent earlier must go.
        v.preview_drawn = true;

        let content = preview_content(inner);
        let (top, bot) = (content.y, content.bottom());
        let w = inner.width as usize;

        if top > inner.y {
            // Size/kind stay whole; the long path is trimmed on the left instead,
            // since the tail matters more.
            let tail = format!(" · {} · {}{}", human(size), kind_name(self.preview.as_ref()), view_name(self.preview.as_ref()));
            let room = w.saturating_sub(tail.chars().count());
            let head = format!("{}{tail}", elide_left(&crate::paths::shown(&path), room));
            buf.set_stringn(inner.x, inner.y, head, w, Style::default().fg(theme.muted));
        }

        // Until the new one is loaded, the previous preview stays.
        let Some(p) = self.preview.as_ref() else { return };
        // Fresh and not yet measured by `follow`: this frame finds the line itself, as `follow` will.
        let scroll = match (self.fresh && p.path == path, line) {
            (true, Some(l)) => p.scroll_for(l, content.height),
            (true, None) => 0,
            (false, _) => self.pscroll,
        };
        let path = p.path.clone();
        // We don't scroll the tail into emptiness, nor past the longest line's end; `follow` keeps the same bounds.
        let total = p.line_count();
        let scroll = scroll.min(total.saturating_sub(1));
        let hscroll = if self.wrap { 0 } else { self.phscroll.min(p.side_room(content.width)) };

        // preview.rs shows a directory as one line; here there's room for a name
        // list, everything else it draws itself.
        if let crate::preview::Kind::Dir(listing) = &p.kind {
            draw_dir(buf, content, listing.as_ref(), theme);
        } else {
            p.draw(buf, content, scroll, hscroll, self.wrap, theme);
        }

        if bot < inner.bottom() && total > 0 {
            let foot = format!("line {} of {}", scroll + 1, total);
            buf.set_stringn(inner.x, bot, foot, w, Style::default().fg(theme.faint));
        }
        // The thumb takes the accent while the keys are here: with no border of its own, this
        // half has to say so itself.
        let rows = content.height as usize;
        if total > rows {
            let colour = if focused { theme.accent } else { theme.faint };
            crate::render::draw_scrollbar(buf, content, total - scroll - rows, scroll, colour, theme);
        }

        self.paint_selection(v, buf, content, theme);

        // Handed over once per file (and again if the area moved): ratatui doesn't
        // touch or refresh the image itself.
        if self.image_view != crate::preview::ImageView::Pixels {
            v.hide_image();
        } else if v.sent_img.as_ref().map(|(sp, sa)| (sp.as_path(), *sa)) != Some((path.as_path(), content)) {
            match self.preview.as_ref().and_then(|p| p.image_bytes(content, IMG_ID)) {
                Some(b) => {
                    v.pending_img = b;
                    v.sent_img = Some((path, content));
                }
                None => {
                    if v.sent_img.take().is_some() {
                        v.pending_img = crate::preview::clear_image_bytes(IMG_ID);
                    }
                }
            }
        }
    }

    fn draw_rows(&self, v: &mut TreeView, buf: &mut Buffer, area: Rect, theme: &Theme) {
        let w = area.width as usize;
        let mut y = area.y;

        // The query line eats into the top - without it you can't see what's being typed.
        if self.mode != Mode::Tree {
            let prefix = if self.mode == Mode::Filter { "/" } else { "⌕ " };
            buf.set_stringn(
                area.x,
                y,
                format!("{prefix}{}", self.query),
                w,
                Style::default().fg(theme.accent),
            );
            // Where we're searching from - otherwise it's unclear why a neighboring directory stays silent.
            let scope = crate::paths::shown(&self.scope);
            let sw = scope.chars().count();
            if w > self.query.chars().count() + sw + 4 {
                buf.set_stringn(area.x + (w - sw) as u16, y, &scope, sw, Style::default().fg(theme.faint));
            }
            y += 1;
            if let Some(cut) = self.cut.filter(|_| y < area.bottom()) {
                buf.set_stringn(area.x, y, format!(" {}", cut.say()), w, Style::default().fg(theme.faint));
                y += 1;
            }
        }

        let bot = area.bottom();
        if bot <= y {
            return;
        }
        let list_h = (bot - y) as usize;
        v.list_h = list_h;

        if self.rows.is_empty() {
            // "Empty" alone leaves you guessing why; we say whether it failed, is
            // hidden-only, or truly empty.
            let msg = if self.view == View::Changes {
                " nothing changed".to_string()
            } else if self.mode != Mode::Tree {
                " nothing found".to_string()
            } else {
                self.empty.clone()
            };
            buf.set_stringn(area.x, y, &msg, w, Style::default().fg(theme.faint));
            return;
        }

        let selected = self.selected.min(self.rows.len() - 1);
        for k in follow_top(self.top, selected, self.rows.len(), list_h)..self.rows.len() {
            if y >= bot {
                break;
            }
            let row = &self.rows[k];
            let selected = k == selected;

            let state = mark_of(&self.marks, row).filter(|_| !row.up && row.more == 0);
            let badges = if row.up || row.more > 0 { Vec::new() } else { crate::slots::tree_badges(&self.badges, &row.path) };
            // Badges follow the name and its marks, one cell apart each.
            let bw: usize = badges.iter().map(|b| 1 + crate::slots::cells(&b.text)).sum();
            // We color the mark and the arrow separately, so we remember their columns.
            let mut mark = None;
            let mut line = String::new();
            // In `changes` every row has a state, so it leads the row as a column and the names
            // line up under each other. In the tree only some rows have one: it follows the name.
            let flat = self.view == View::Changes && self.changes_flat;
            if self.view == View::Changes {
                mark = state.map(|m| (0, m));
                line.push(state.unwrap_or(' '));
                line.push(' ');
                // A flat list has nothing to fold, so the column the arrows sit in is a gap.
                if flat {
                    line.push(' ');
                }
            }
            for _ in 0..row.depth {
                line.push_str("  ");
            }
            if !flat {
                line.push_str(if row.up || !row.is_dir {
                    "  "
                } else if row.open {
                    "▾ "
                } else {
                    "▸ "
                });
            }
            // In `changes` the label is a path, longer than the panel: its end names the file.
            if self.view == View::Changes {
                // The state already has its column at the front, so the label may run to the edge.
                line.push_str(&elide_left(&row.label, w.saturating_sub(line.chars().count() + bw)));
            } else {
                line.push_str(&row.label);
                mark = state.map(|m| {
                    let at = crate::slots::cells(&line) + 1;
                    line.push(' ');
                    line.push(m);
                    (at, m)
                });
            }
            let arrow = row.link.then(|| {
                let at = crate::slots::cells(&line) + 1;
                line.push_str(" →");
                at
            });
            if row.err {
                line.push_str(" ⚠");
            }

            // A changed file takes its state's color: the mark says which state, the color says
            // there is one without being looked for. Directories keep the plain text - the
            // mark they carry is their contents', and a whole branch in one color reads as noise.
            let tint = state.filter(|_| !row.is_dir).map(|m| mark_color(m, theme));
            let tone = if row.more > 0 { theme.muted } else { tint.unwrap_or(theme.text) };
            let style = Style::default().fg(tone).bg(theme.ground);
            // Pad with spaces: the highlight stretches from frame to frame.
            let padded: String = line.chars().chain(std::iter::repeat(' ')).take(w).collect();
            buf.set_stringn(area.x, y, &padded, w, style);
            if let Some((at, m)) = mark.filter(|&(at, _)| at < w) {
                buf.set_stringn(area.x + at as u16, y, m.to_string(), 1, Style::default().fg(mark_color(m, theme)));
            }
            if let Some(at) = arrow.filter(|&at| at < w) {
                buf.set_stringn(area.x + at as u16, y, "→", 1, Style::default().fg(theme.faint));
            }
            let lw = crate::slots::cells(&line);
            let mut at = lw + 1;
            for b in &badges {
                if at < w {
                    buf.set_stringn(area.x + at as u16, y, &b.text, w - at, Style::default().fg(b.role.color(theme)));
                }
                at += 1 + crate::slots::cells(&b.text);
            }

            // On the right: where `../` leads, or a file's size - only if it doesn't run into the name.
            let room = w.saturating_sub(line.chars().count() + 2);
            let right = if row.up {
                Some(elide_left(&crate::paths::shown(&row.path), room))
            } else if !row.is_dir && row.more == 0 && state != Some('D') {
                // A deleted file has no size; "0B" would be an answer about a file that is gone.
                Some(human(row.size))
            } else if row.err {
                Some("no access".to_string())
            } else {
                None
            };
            if let Some(s) = right {
                let sw = s.chars().count();
                if w > lw + bw + sw + 1 {
                    buf.set_stringn(area.x + (w - sw) as u16, y, &s, sw, Style::default().fg(theme.faint));
                }
            }
            if selected {
                let keys = self.focus == Focus::Tree && self.active;
                buf.set_style(Rect::new(area.x, y, w as u16, 1), if keys { theme.cursor() } else { theme.cursor_away() });
            }

            v.hits.push((Rect::new(area.x, y, area.width, 1), k));
            y += 1;
        }
    }
}

/// What the tree cannot say about a directory: how much is in it, and what was touched
/// last. The names themselves are one column to the left already.
fn draw_dir(buf: &mut Buffer, area: Rect, listing: Option<&crate::preview::Listing>, theme: &Theme) {
    let Some(l) = listing else { return };
    let w = area.width as usize;
    let Some(bytes) = l.bytes else {
        // its size and what changed last would take a stat of every entry
        buf.set_stringn(area.x, area.y, format!("{} files · {} folders", l.files, l.folders), w, Style::default().fg(theme.muted));
        return;
    };
    let head = format!("{} files · {} folders · {}", l.files, l.folders, human(bytes));
    buf.set_stringn(area.x, area.y, head, w, Style::default().fg(theme.muted));
    if l.newest.is_empty() || area.height < 3 {
        return;
    }

    buf.set_stringn(area.x, area.y + 2, "changed last", w, Style::default().fg(theme.faint));
    let now = std::time::SystemTime::now();
    for (k, (name, is_dir, changed)) in l.newest.iter().enumerate() {
        let y = area.y + 3 + k as u16;
        if y >= area.bottom() {
            break;
        }
        let when = changed
            .and_then(|t| now.duration_since(t).ok())
            .map(crate::sidebar::ago)
            .unwrap_or_default();
        let name = if *is_dir { format!("{name}/") } else { name.clone() };
        buf.set_stringn(area.x, y, elide_left(&name, w.saturating_sub(when.len() + 1)), w, Style::default().fg(theme.text));
        if w > name.chars().count() + when.len() + 1 {
            buf.set_stringn(
                area.x + (w - when.len()) as u16,
                y,
                &when,
                when.len(),
                Style::default().fg(theme.faint),
            );
        }
    }
}

/// The first row drawn: the one before, moved just enough that the selection is inside a window of `h` rows.
pub(super) fn follow_top(top: usize, selected: usize, len: usize, h: usize) -> usize {
    let top = if selected < top {
        selected
    } else if h > 0 && selected >= top + h {
        selected + 1 - h
    } else {
        top
    };
    top.min(len.saturating_sub(1))
}

/// The preview half less its header and footer rows, which it keeps unless it is quite short.
pub(super) fn preview_content(inner: Rect) -> Rect {
    if inner.height >= 3 {
        Rect { y: inner.y + 1, height: inner.height - 2, ..inner }
    } else {
        inner
    }
}

fn mark_color(m: char, theme: &Theme) -> Color {
    match m {
        'M' => theme.warn,
        // New is new whether or not git was told: one color, two letters.
        'N' | 'A' => theme.ok,
        'D' => theme.bad,
        _ => theme.muted,
    }
}

/// A labeled frame; returns the inside, or `None` if there's nowhere to draw. Border
/// color is the focus indicator.
fn panel(buf: &mut Buffer, area: Rect, theme: &Theme, title: &str, focus: bool) -> Option<Rect> {
    let r = area.intersection(buf.area);
    if r.is_empty() {
        return None;
    }
    let border = if focus { theme.border_focus } else { theme.border };
    Block::bordered().style(Style::default().bg(theme.ground).fg(border)).render(r, buf);
    let inner = r.inner(Margin::new(1, 1));
    if inner.width < 3 || inner.height == 0 {
        return None;
    }
    buf.set_stringn(inner.x + 1, r.y, format!(" {title} "), inner.width as usize - 1, theme.title(focus));
    // one column of padding inside every frame, as in the sidebar
    Some(Rect { x: inner.x + 1, width: inner.width - 2, ..inner })
}

/// Ellipsis on the left: the path's start is already visible in the tree, the tail
/// (file name) isn't.
fn elide_left(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    if max <= 1 {
        return "…".to_string();
    }
    format!("…{}", s.chars().skip(n - max + 1).collect::<String>())
}

/// " · Table" when a plugin offers views of this file: the one shown.
fn view_name(p: Option<&crate::preview::Preview>) -> String {
    match p.map(|p| &p.kind) {
        Some(crate::preview::Kind::Styled { modes, mode, .. }) if modes.len() > 1 => {
            modes.iter().find(|(id, _)| id == mode).map(|(_, label)| format!(" · {label}")).unwrap_or_default()
        }
        _ => String::new(),
    }
}

/// Content kind in one word, for the header.
/// ceiling: duplicates preview.rs's private `Lang`/`ImageFormat::name`; merge once
/// that module exposes them.
fn kind_name(p: Option<&crate::preview::Preview>) -> &str {
    use crate::preview::{ImageFormat as F, Kind, Lang};
    match p.map(|p| &p.kind) {
        Some(Kind::Text { lang, .. }) => match lang {
            Lang::Rust => "rust",
            Lang::Js => "js",
            Lang::Ts => "ts",
            Lang::Python => "python",
            Lang::Shell => "shell",
            Lang::Json => "json",
            Lang::Toml => "toml",
            Lang::Yaml => "yaml",
            Lang::Markdown => "markdown",
            Lang::Diff => "diff",
            Lang::Plain => "text",
        },
        Some(Kind::Image { format, .. }) => match format {
            F::Png => "png",
            F::Jpeg => "jpeg",
            F::Gif => "gif",
            F::Webp => "webp",
        },
        Some(Kind::Styled { by, .. }) => by,
        Some(Kind::Binary { .. }) => "binary",
        Some(Kind::Dir(_)) => "directory",
        _ => "—",
    }
}

fn human(n: u64) -> String {
    const U: [&str; 4] = ["B", "K", "M", "G"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i + 1 < U.len() {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n}B") } else { format!("{v:.1}{}", U[i]) }
}

impl TreeView {
    /// A row or a title button a click acts on is under (x, y).
    pub fn clickable(&self, x: u16, y: u16) -> bool {
        let at = (x, y).into();
        self.hits.iter().any(|(r, _)| r.contains(at))
            || [self.btn_zoom, self.btn_close, self.btn_files, self.btn_changes, self.btn_shape].iter().any(|r| r.contains(at))
    }

    /// The preview's room for content, in cells, as the last frame drew it.
    pub fn preview_size(&self) -> (u16, u16) {
        let r = preview_content(self.prev_area);
        (r.width, r.height)
    }

    /// The text of a finished selection, once. The app puts it on the clipboard.
    pub fn take_copy(&mut self) -> Option<String> {
        self.pending_copy.take()
    }

    /// Terminal bytes to append after the frame; empty if the image hasn't changed.
    pub fn take_image_bytes(&mut self) -> Vec<u8> {
        if !std::mem::take(&mut self.preview_drawn) {
            self.hide_image();
        }
        std::mem::take(&mut self.pending_img)
    }

    /// Kitty images stay in the terminal until deleted.
    fn hide_image(&mut self) {
        if self.sent_img.take().is_some() {
            self.pending_img = crate::preview::clear_image_bytes(IMG_ID);
        }
    }

    /// A new client has none of our images: the next frame sends it again.
    pub fn forget_image(&mut self) {
        self.sent_img = None;
    }
}
