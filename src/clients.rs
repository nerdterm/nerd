//! Every client that attached since the server started, newest first: menu > Sessions.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use serde::{Deserialize, Serialize};

use crate::render::Theme;

const CAP: usize = 20;
/// Table rows the dialog shows at most; the rest scroll.
const ROWS: u16 = 8;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// The human chose Detach.
    Detached,
    /// The terminal closed or the link dropped: the client never sends Detach itself.
    Closed,
    TakenOver,
    /// A frame could not be written.
    Lost,
    /// Disconnect everyone, from the menu or `nerd --disconnect`.
    Disconnected,
    /// A later build's end, after a handover back.
    #[serde(other)]
    Unknown,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ClientRecord {
    pub id: u64,
    /// Unix seconds.
    pub since: u64,
    pub host: Option<String>,
    pub program: Option<String>,
    pub ended: Option<(u64, End)>,
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// The one on screen before is taken over by the new one.
pub fn attach(list: &mut Vec<ClientRecord>, rec: ClientRecord) {
    let at = rec.since;
    for r in list.iter_mut().filter(|r| r.ended.is_none()) {
        r.ended = Some((at, End::TakenOver));
    }
    list.insert(0, rec);
    list.truncate(CAP);
}

/// A record closes once: the EOF that follows a quit or a lost frame must not overwrite the first end.
pub fn close(list: &mut [ClientRecord], id: u64, end: End, at: u64) {
    if let Some(r) = list.iter_mut().find(|r| r.id == id && r.ended.is_none()) {
        r.ended = Some((at, end));
    }
}

/// After an update in place; a handover from an older build carries its client but no record of it.
pub fn carry(mut list: Vec<ClientRecord>, client: Option<u64>, at: u64) -> Vec<ClientRecord> {
    if let Some(id) = client
        && !list.iter().any(|r| r.id == id && r.ended.is_none())
    {
        attach(&mut list, ClientRecord { id, since: at, host: None, program: None, ended: None });
    }
    list
}

/// One table row, in words.
struct Row {
    since: String,
    machine: String,
    how: String,
    /// The end time alone, with the date only when it is not the day of Since; empty while on screen.
    end_at: String,
    status: String,
    remote: bool,
    /// An old client names no host.
    no_host: bool,
    /// Carried over from a build that kept no record: its time is the update's, not the attach's.
    seeded: bool,
    current: bool,
}

fn row(r: &ClientRecord, own: Option<&str>, today: Option<(i32, i32)>) -> Row {
    let seeded = r.host.is_none() && r.program.is_none();
    let machine = match r.host.as_deref() {
        Some(h) if Some(h) == own => "this Mac".to_string(),
        Some(h) => h.strip_suffix(".local").unwrap_or(h).to_string(),
        None => "unknown".to_string(),
    };
    let how = match r.program.as_deref() {
        Some("nerd") => "directly".to_string(),
        Some(p) if p.starts_with("sshd") => "over ssh".to_string(),
        Some(p) => p.to_string(),
        None => "unknown".to_string(),
    };
    let (end_at, status) = match r.ended {
        None => (String::new(), "live".to_string()),
        Some((at, end)) => {
            let t = when(at, day(r.since));
            let word = match end {
                End::Detached => "detached",
                End::Closed => "closed",
                End::TakenOver => "replaced",
                End::Lost => "lost",
                End::Disconnected => "disconnected",
                End::Unknown => "ended",
            };
            (t.clone(), format!("{word} at {t}"))
        }
    };
    let remote = how == "over ssh" || (r.host.is_some() && machine != "this Mac");
    let since = if seeded { "earlier".to_string() } else { when(r.since, today) };
    Row { since, machine, how, end_at, status, remote, no_host: r.host.is_none(), seeded, current: r.ended.is_none() }
}

/// One line of what `nerd --disconnect` ended, in the table's words.
pub fn summary(r: &ClientRecord, own: Option<&str>) -> String {
    let row = row(r, own, day(now()));
    format!("ended: {} {} {} {}", row.since, row.machine, row.how, row.status)
}

fn sessions(n: usize) -> &'static str {
    if n == 1 { "session" } else { "sessions" }
}

/// One sentence over the table, and whether it warns; it opens with a capital whatever the machine is called.
fn headline(rows: &[Row]) -> (String, bool) {
    let (text, warn) = sentence(rows);
    let mut chars = text.chars();
    (chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default(), warn)
}

fn sentence(rows: &[Row]) -> (String, bool) {
    let who = |r: &Row| if r.no_host { "an unknown machine".to_string() } else { r.machine.clone() };
    if let Some(r) = rows.iter().find(|r| r.current && r.remote) {
        let over = if r.how == "over ssh" { ", over ssh" } else { "" };
        return (format!("{} holds the live session now{over}.", who(r)), true);
    }
    let remote: Vec<&Row> = rows.iter().filter(|r| r.remote).collect();
    if let Some(r) = remote.first() {
        let n = remote.len();
        let more = if n > 1 { format!(" and {} more", n - 1) } else { String::new() };
        return (format!("{n} {} came from elsewhere: {}, {}–{}{more}.", sessions(n), who(r), r.since, r.end_at), true);
    }
    let n = rows.len();
    let since = rows.iter().rev().find(|r| !r.seeded).map(|r| format!(" since {}", r.since)).unwrap_or_default();
    (format!("Only this Mac. {n} {}{since}.", sessions(n)), false)
}

/// The selected row in full: dates, the host as reported, the program as measured, the end as a sentence.
fn details(r: &ClientRecord, row: &Row) -> String {
    let opened = if row.seeded {
        format!("It came over an update at {}; when it opened and from where is unknown.", full(r.since))
    } else {
        let how = if row.how == "directly" || row.how == "over ssh" { format!(" {}", row.how) } else { String::new() };
        format!(
            "Opened {} from {}{how} ({}).",
            full(r.since),
            r.host.as_deref().unwrap_or("an unknown machine"),
            r.program.as_deref().unwrap_or("unknown program")
        )
    };
    let end = match r.ended {
        None => "This is the live session.".to_string(),
        Some((at, end)) => {
            let what = match end {
                End::Closed => "Its terminal closed or the link dropped",
                End::Detached => "It was detached from the menu",
                End::TakenOver => "Another session replaced it",
                End::Lost => "A frame could not be written to it",
                End::Disconnected => "Everyone was disconnected",
                End::Unknown => "It ended",
            };
            format!("{what} at {}.", full(at))
        }
    };
    format!("{opened} {end}")
}

fn fit(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    match n {
        0 => String::new(),
        _ => format!("{}…", s.chars().take(n - 1).collect::<String>()),
    }
}

/// Read-only: the table, the headline over it and the details of the selected row, on the shared panel.
pub struct Sessions {
    list: Vec<ClientRecord>,
    /// This machine's hostname, which reads as `this Mac`.
    own: Option<String>,
    view: crate::panel::View,
}

pub type Drawn = crate::panel::Drawn;

impl Sessions {
    pub fn new(list: Vec<ClientRecord>, own: Option<String>) -> Self {
        Self { list, own, view: crate::panel::View::new(ROWS as usize, false) }
    }

    fn rows(&self) -> Vec<Row> {
        let today = day(now());
        self.list.iter().map(|r| row(r, self.own.as_deref(), today)).collect()
    }

    /// The panel's content for this screen; `selected` picks the details. Details take the height of the longest, so selecting does not resize it.
    fn content(&self, screen: Rect, theme: &Theme, selected: Option<usize>) -> crate::panel::Content {
        let w = crate::panel::inner(screen);
        let ground = Style::default().bg(theme.ground);
        let rows = self.rows();
        let (head, warn) = headline(&rows);
        let head_style = ground.fg(if warn { theme.warn } else { theme.muted });
        let mut lines: Vec<crate::panel::Spans> = crate::menu::wrap(&head, w).into_iter().map(|l| vec![(0, l, head_style)]).collect();
        lines.push(Vec::new());

        let width = |f: fn(&Row) -> &str, title: &str| rows.iter().map(|r| f(r).chars().count()).chain([title.chars().count()]).max().unwrap_or(0);
        let since_w = width(|r| &r.since, "Since");
        let how_w = width(|r| &r.how, "How");
        let status_w = width(|r| &r.status, "Status");
        // Machine and How share what Since and Status leave, half each at least, so neither column vanishes.
        let rest = w.saturating_sub(since_w + status_w + 6);
        let machine_want = width(|r| &r.machine, "Machine");
        let how_w = how_w.min(rest.saturating_sub(machine_want.min(rest / 2)));
        let machine_w = machine_want.min(rest.saturating_sub(how_w));
        let xs = [0, since_w + 2, since_w + machine_w + 4, since_w + machine_w + how_w + 6];
        let cells = |texts: [&str; 4], styles: [Style; 4]| -> crate::panel::Spans {
            (0..4).map(|i| (xs[i], fit(texts[i], if i == 3 { w.saturating_sub(xs[3]) } else { [since_w, machine_w, how_w][i] }), styles[i])).collect()
        };
        lines.push(cells(["Since", "Machine", "How", "Status"], [ground.fg(theme.muted); 4]));

        let table = rows
            .iter()
            .zip(&self.list)
            .map(|(row, r)| {
                let base = if row.current { theme.chosen().bg(theme.ground) } else { Style::default().fg(theme.text).bg(theme.ground) };
                let origin = if row.current {
                    base
                } else if row.seeded {
                    base.fg(theme.faint)
                } else if row.remote {
                    base.fg(theme.warn)
                } else {
                    base
                };
                let spans = cells([&row.since, &row.machine, &row.how, &row.status], [base, origin, origin, base]);
                crate::panel::Row { id: r.id.to_string(), base, spans, parent: false }
            })
            .collect();
        let details = |i: usize| crate::menu::wrap(&details(&self.list[i], &rows[i]), w);
        let body = selected.filter(|i| *i < rows.len()).map(details).unwrap_or_default();
        let body_h = (0..rows.len()).map(|i| details(i).len()).max().unwrap_or(0);
        let body = body.into_iter().map(|l| vec![(0, l, ground.fg(theme.text))]).collect();
        crate::panel::Content { head: lines, rows: table, body, body_h, ..Default::default() }
    }

    /// The content with the selected row's details; keys and clicks need only its shape, which no theme changes.
    fn laid_out(&self, screen: Rect, theme: &Theme) -> crate::panel::Content {
        let c = self.content(screen, theme, None);
        self.content(screen, theme, Some(self.view.selected(&c)))
    }

    pub fn draw(&self, buf: &mut Buffer, screen: Rect, theme: &Theme) -> Drawn {
        self.view.draw(buf, screen, theme, "Session history", &self.laid_out(screen, theme))
    }

    /// `true` means close; `screen` tells how many rows fit.
    pub fn key(&mut self, k: crossterm::event::KeyEvent, screen: Rect) -> bool {
        let c = self.laid_out(screen, &Theme::dark());
        matches!(self.view.key(k, screen, &c), crate::panel::Hit::Close | crate::panel::Hit::Enter(_))
    }

    /// `true` means close; `drawn`: where the last frame put the rows and the button.
    pub fn mouse(&mut self, ev: crossterm::event::MouseEvent, screen: Rect, drawn: &Drawn) -> bool {
        let c = self.laid_out(screen, &Theme::dark());
        self.view.mouse(ev, screen, &c, drawn) == crate::panel::Hit::Close
    }
}

fn day(secs: u64) -> Option<(i32, i32)> {
    local(secs).map(|t| (t.tm_year, t.tm_yday))
}

fn local(secs: u64) -> Option<nix::libc::tm> {
    let t = secs as nix::libc::time_t;
    // SAFETY: tm is plain data, and localtime_r writes only into it.
    let mut tm: nix::libc::tm = unsafe { std::mem::zeroed() };
    (!unsafe { nix::libc::localtime_r(&t, &mut tm) }.is_null()).then_some(tm)
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// `HH:MM on Mon DD`, for the details.
fn full(secs: u64) -> String {
    let Some(t) = local(secs) else { return "?".into() };
    format!("{:02}:{:02} on {} {}", t.tm_hour, t.tm_min, MONTHS[t.tm_mon.clamp(0, 11) as usize], t.tm_mday)
}

/// Local `HH:MM`, with the date in front when it is not on `today`.
fn when(secs: u64, today: Option<(i32, i32)>) -> String {
    let Some(t) = local(secs) else { return "?".into() };
    let hm = format!("{:02}:{:02}", t.tm_hour, t.tm_min);
    if Some((t.tm_year, t.tm_yday)) == today {
        hm
    } else {
        format!("{} {} {hm}", MONTHS[t.tm_mon.clamp(0, 11) as usize], t.tm_mday)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: u64, since: u64) -> ClientRecord {
        ClientRecord { id, since, host: Some("h".into()), program: Some("nerd".into()), ended: None }
    }

    #[test]
    fn a_second_attach_takes_over_and_its_own_eof_closes_it() {
        let mut l = Vec::new();
        attach(&mut l, rec(1, 10));
        attach(&mut l, rec(2, 20));
        close(&mut l, 2, End::Closed, 30);
        assert_eq!(l[0].ended, Some((30, End::Closed)));
        assert_eq!(l[1].ended, Some((20, End::TakenOver)));
        close(&mut l, 1, End::Closed, 40);
        assert_eq!(l[1].ended, Some((20, End::TakenOver)), "the replaced client's EOF changes nothing");
    }

    #[test]
    fn the_first_end_stays() {
        for first in [End::Detached, End::Lost] {
            let mut l = Vec::new();
            attach(&mut l, rec(1, 10));
            close(&mut l, 1, first, 20);
            close(&mut l, 1, End::Closed, 21);
            assert_eq!(l[0].ended, Some((20, first)));
        }
    }

    #[test]
    fn the_list_keeps_twenty() {
        let mut l = Vec::new();
        for id in 1..=25 {
            attach(&mut l, rec(id, id));
        }
        assert_eq!(l.len(), 20);
        assert_eq!((l[0].id, l[19].id), (25, 6));
        assert_eq!(l.iter().filter(|r| r.ended.is_none()).count(), 1);
    }

    #[test]
    fn a_carried_client_gets_an_open_record() {
        let l = carry(Vec::new(), Some(7), 100);
        assert_eq!(l, vec![ClientRecord { id: 7, since: 100, host: None, program: None, ended: None }]);
        assert_eq!(carry(l.clone(), Some(7), 200), l, "a record from this build is kept as is");
        assert!(carry(Vec::new(), None, 100).is_empty());
    }

    #[test]
    fn today_shows_only_the_time() {
        let t = now();
        let today = day(t);
        assert_eq!(when(t, today).len(), 5);
        assert!(when(t - 3 * 86400, today).len() > 5);
    }

    fn own(id: u64, since: u64, ended: Option<(u64, End)>) -> ClientRecord {
        ClientRecord { id, since, host: Some("this-host".into()), program: Some("nerd".into()), ended }
    }

    fn ssh(id: u64, since: u64, ended: Option<(u64, End)>) -> ClientRecord {
        ClientRecord { id, since, host: Some("MacBook-Air.local".into()), program: Some("sshd-session".into()), ended }
    }

    fn rows_of(list: &[ClientRecord]) -> Vec<Row> {
        Sessions::new(list.to_vec(), Some("this-host".into())).rows()
    }

    #[test]
    fn the_headline_says_what_matters_first() {
        let t = now() - 600;
        let (text, warn) = headline(&rows_of(&[own(2, t + 60, None), own(1, t, Some((t + 60, End::TakenOver)))]));
        assert!(text.starts_with("Only this Mac. 2 sessions since ") && !warn, "{text}");

        let (text, warn) = headline(&rows_of(&[ssh(2, t + 60, None), own(1, t, Some((t + 60, End::TakenOver)))]));
        assert_eq!((text.as_str(), warn), ("MacBook-Air holds the live session now, over ssh.", true));

        let list = [own(3, t + 120, None), ssh(2, t + 60, Some((t + 120, End::TakenOver))), ssh(1, t, Some((t + 60, End::Closed)))];
        let (text, warn) = headline(&rows_of(&list));
        assert!(text.starts_with("2 sessions came from elsewhere: MacBook-Air, ") && text.ends_with(" and 1 more.") && warn, "{text}");

        let old = |id, ended| ClientRecord { id, since: t, host: None, program: Some("sshd-session".into()), ended };
        assert_eq!(headline(&rows_of(&[old(1, None)])).0, "An unknown machine holds the live session now, over ssh.");
        let (text, _) = headline(&rows_of(&[own(2, t + 60, None), old(1, Some((t + 60, End::Closed)))]));
        assert!(text.starts_with("1 session came from elsewhere: an unknown machine, "), "{text}");

        // A relay on this same Mac: the machine is "this Mac", and the sentence still opens with a capital.
        let here = ClientRecord { id: 1, since: t, host: Some("this-host".into()), program: Some("sshd-session".into()), ended: None };
        assert_eq!(headline(&rows_of(&[here])).0, "This Mac holds the live session now, over ssh.");
    }

    #[test]
    fn a_seeded_row_knows_nothing_it_was_not_told() {
        let r = &rows_of(&carry(Vec::new(), Some(1), now()))[0];
        assert_eq!((r.since.as_str(), r.machine.as_str(), r.how.as_str()), ("earlier", "unknown", "unknown"));
        assert!(r.seeded && !r.remote);
    }

    #[test]
    fn a_status_carries_a_date_only_when_the_day_differs() {
        let t = now() - 3 * 86400;
        let same = &rows_of(&[own(1, t, Some((t, End::Closed)))])[0];
        assert_eq!(same.status.len(), "closed at 14:02".len(), "{}", same.status);
        let later = &rows_of(&[own(1, t, Some((t + 2 * 86400, End::Detached)))])[0];
        assert!(later.status.starts_with("detached at ") && later.status.len() > "detached at 14:02".len(), "{}", later.status);
        let replaced = &rows_of(&[own(2, t, None), own(1, t, Some((t, End::TakenOver)))])[1];
        assert!(replaced.status.starts_with("replaced at "));
        assert_eq!(rows_of(&[own(1, t, None)])[0].status, "live");
    }

    #[test]
    fn a_disconnect_reads_as_one_and_a_later_end_as_ended() {
        let t = now() - 600;
        let list = [own(1, t, Some((t + 60, End::Disconnected)))];
        let rows = rows_of(&list);
        assert!(rows[0].status.starts_with("disconnected at "), "{}", rows[0].status);
        assert!(details(&list[0], &rows[0]).contains(" Everyone was disconnected at "), "{}", details(&list[0], &rows[0]));
        assert!(summary(&list[0], Some("this-host")).starts_with("ended: ") && summary(&list[0], Some("this-host")).contains(" this Mac directly disconnected at "));
        let later: End = serde_json::from_str("\"SomethingNew\"").unwrap();
        assert_eq!(later, End::Unknown);
        assert!(rows_of(&[own(1, t, Some((t, later)))])[0].status.starts_with("ended at "));
    }

    fn line(buf: &Buffer, y: u16) -> String {
        (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
    }

    #[test]
    fn each_colour_means_one_thing_and_nothing_is_cut() {
        let t = now() - 600;
        let list = vec![
            own(4, t + 300, None),
            own(3, t + 200, Some((t + 300, End::TakenOver))),
            ssh(2, t + 100, Some((t + 200, End::Closed))),
            ClientRecord { id: 1, since: t, host: None, program: None, ended: Some((t + 100, End::TakenOver)) },
            ClientRecord { id: 0, since: t, host: Some("this-host".into()), program: Some("a-python-relay-with-a-long-name".into()), ended: Some((t + 3 * 86400, End::Lost)) },
        ];
        let theme = Theme::dark();
        let screen = Rect::new(0, 0, 100, 30);
        let mut w = Sessions::new(list, Some("this-host".into()));
        w.key(crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Down), screen);
        let mut buf = Buffer::empty(screen);
        let rows_at = w.draw(&mut buf, screen, &theme).rows;
        assert_eq!(rows_at.len(), 5);
        assert_eq!(rows_at[0].0.width, 64, "a 64-column content width");
        let text: Vec<String> = rows_at.iter().map(|(r, _)| line(&buf, r.y)).collect();
        for (i, want) in [["this Mac", "directly", "live"], ["this Mac", "directly", "replaced at "], ["MacBook-Air", "over ssh", "closed at "], ["earlier", "unknown", "replaced at "], ["this Mac", "a-python", "lost at "]].iter().enumerate() {
            for part in want {
                assert!(text[i].contains(part), "row {i} lost {part:?}: {}", text[i]);
            }
        }

        let header = line(&buf, rows_at[0].0.y - 1);
        assert!(text[4].contains('…') && header.contains("Machine") && header.contains("How"), "{header}\n{}", text[4]);
        let (r0, r1, r2) = (rows_at[0].0, rows_at[1].0, rows_at[2].0);
        let col = |s: &str, part: &str| s[..s.find(part).unwrap()].chars().count() as u16;
        let live = &buf[(col(&text[0], "this Mac"), r0.y)];
        assert!(live.fg == theme.accent && live.modifier.contains(ratatui::style::Modifier::BOLD), "the live row, unselected, is chosen: {live:?}");
        let bar = |c: &ratatui::buffer::Cell| c.fg == theme.ground && c.bg == theme.accent;
        assert!((r1.x..r1.right()).all(|x| bar(&buf[(x, r1.y)])), "the selected row is the accent bar across");
        for part in ["MacBook-Air", "over ssh"] {
            assert_eq!(buf[(col(&text[2], part), r2.y)].fg, theme.warn, "{part} came from elsewhere");
        }
        assert_ne!(buf[(col(&text[2], "closed"), r2.y)].fg, theme.warn, "only the origin warns");

        w.key(crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Down), screen);
        w.key(crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Up), screen);
        w.key(crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Up), screen);
        let mut buf = Buffer::empty(screen);
        w.draw(&mut buf, screen, &theme);
        assert!((r0.x..r0.right()).all(|x| bar(&buf[(x, r0.y)])), "selected, the live row is the bar, not the tint");
        w.key(crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Down), screen);
        w.key(crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Down), screen);
        let mut buf = Buffer::empty(screen);
        w.draw(&mut buf, screen, &theme);
        assert!((r2.x..r2.right()).all(|x| bar(&buf[(x, r2.y)])), "no warn on a selected row");
        let all: Vec<String> = (0..30).map(|y| line(&buf, y)).collect();
        assert!(all.iter().any(|l| l.contains("MacBook-Air.local over ssh")), "{}", all.join("\n"));
    }

    #[test]
    fn a_tall_screen_shows_eight_rows_and_scrolls_the_rest() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut l = Vec::new();
        for id in 1..=20 {
            attach(&mut l, rec(id, now() - 1000 + id));
        }
        let screen = Rect::new(0, 0, 100, 80);
        let mut w = Sessions::new(l, Some("this-host".into()));
        assert_eq!(w.draw(&mut Buffer::empty(screen), screen, &Theme::dark()).rows.len(), 8, "a dialog, not a page");
        for _ in 0..10 {
            w.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), screen);
        }
        let drawn = w.draw(&mut Buffer::empty(screen), screen, &Theme::dark());
        assert_eq!(drawn.rows.len(), 8);
        assert!(drawn.rows.iter().any(|(_, i)| *i == 10), "the chosen row scrolled into view");
    }

    #[test]
    fn a_click_selects_a_row_and_a_click_outside_closes() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let click = |column, row| MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column, row, modifiers: KeyModifiers::NONE };
        let t = now() - 600;
        let screen = Rect::new(0, 0, 100, 30);
        let mut w = Sessions::new(vec![own(2, t + 60, None), own(1, t, Some((t + 60, End::Closed)))], Some("this-host".into()));
        let drawn = w.draw(&mut Buffer::empty(screen), screen, &Theme::dark());
        let r = drawn.rows[1].0;
        assert!(!w.mouse(click(r.x + 3, r.y), screen, &drawn));
        assert_eq!(w.view.selected(&w.laid_out(screen, &Theme::dark())), 1);
        let c = drawn.close;
        assert!(w.mouse(click(c.x + 1, c.y), screen, &drawn), "the close button closes");
        assert!(w.mouse(click(0, 0), screen, &drawn), "a click outside closes");
    }
}
