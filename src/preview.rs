//! File preview: highlighted text, or an image either sent as raw Kitty escape sequences, bypassing ratatui's buffer (which strips control characters), or drawn into the buffer as half blocks.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Widget};

use crate::render::Theme;

pub const PREVIEW_LIMIT: u64 = 64 * 1024;

const SNIFF: usize = 8 * 1024;

/// Past this we don't read the image: it would cross the daemon-client socket whole.
const IMAGE_LIMIT: u64 = 4 * 1024 * 1024;

/// Kitty protocol's own limit on a base64 chunk in one escape sequence.
const CHUNK: usize = 4096;

/// Longer side of the pixels kept for the block glyphs; a preview column needs far fewer.
const BLOCK_PIXELS: u32 = 800;

/// How an image shows: decided by the caller from the settings and the client terminal.
/// What to say under a blocks image, if anything.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BlockHint {
    /// The user asked for blocks.
    None,
    /// The terminal answered the graphics query with no.
    Terminal,
    /// tmux carries no graphics, whatever the terminal can do.
    Tmux,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ImageView {
    /// Kitty graphics outside the buffer; the buffer gets only the fallback box.
    Pixels,
    Blocks { hint: BlockHint },
    Off,
}

/// Straight (not premultiplied) RGBA, rows top to bottom.
pub struct Pixels {
    w: usize,
    h: usize,
    rgba: Vec<u8>,
}

/// Picked by extension, or by shebang for scripts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Diff,
    Rust,
    Js,
    Ts,
    Python,
    Shell,
    Json,
    Toml,
    Yaml,
    Markdown,
    Plain,
}

/// By magic bytes, not by extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Gif,
    Webp,
}

impl ImageFormat {
    fn name(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::Gif => "gif",
            Self::Webp => "webp",
        }
    }

    fn sniff(b: &[u8]) -> Option<Self> {
        if b.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Self::Png)
        } else if b.starts_with(&[0xff, 0xd8, 0xff]) {
            Some(Self::Jpeg)
        } else if b.starts_with(b"GIF8") {
            Some(Self::Gif)
        } else if b.starts_with(b"RIFF") && b.len() >= 12 && &b[8..12] == b"WEBP" {
            Some(Self::Webp)
        } else {
            None
        }
    }
}

pub enum Kind {
    Text { lines: Vec<String>, lang: Lang, cut: bool },
    /// Lines a plugin or a public built-in drew; `by` names the view in the header.
    Styled { lines: Vec<crate::slots::Line>, cut: bool, by: String, modes: Vec<(String, String)>, mode: String },
    /// `dims` from the header; `png`/`pixels` are made off the loop when first wanted (`need`), `Some(None)` = failed.
    Image { format: ImageFormat, size: u64, dims: Option<(u32, u32)>, png: Option<Option<Arc<Vec<u8>>>>, pixels: Option<Option<Arc<Pixels>>> },
    Binary { size: u64 },
    /// The tree draws a folder itself, from the listing its loader adds.
    Dir(Option<Listing>),
    Note(String),
}

/// What a folder holds: counted always, its size and newest entries only when it is not huge.
pub struct Listing {
    pub files: usize,
    pub folders: usize,
    pub bytes: Option<u64>,
    /// (name, is a folder, last write), newest first.
    pub newest: Vec<(String, bool, Option<std::time::SystemTime>)>,
}

/// What an image still needs made before its view can draw it.
#[derive(Debug, Clone, PartialEq)]
pub enum Need {
    Png(PathBuf),
    Pixels(PathBuf, Option<(u32, u32)>),
}

pub struct Preview {
    pub path: PathBuf,
    pub kind: Kind,
    /// One-based line set by `goto`, drawn with a background highlight.
    target: Option<u32>,
    /// Computed once on load, so scrolling doesn't reparse from the top every frame.
    carries: Vec<Carry>,
    /// Set by the tree before each draw.
    pub view: ImageView,
}

impl Preview {
    /// Columns the line numbers take on the left. They are drawn, not read from the file, so a
    /// selection has no business in them.
    pub fn gutter(&self) -> u16 {
        match &self.kind {
            Kind::Text { lines, .. } => (lines.len().max(1).to_string().len() + 1) as u16,
            _ => 0,
        }
    }

    /// A preview that only says something, in place of the file.
    pub fn note(path: &Path, why: impl Into<String>) -> Preview {
        Preview { path: path.to_path_buf(), kind: Kind::Note(why.into()), target: None, carries: Vec::new(), view: ImageView::Pixels }
    }

    pub fn styled(path: &Path, lines: Vec<crate::slots::Line>, cut: bool, by: &str, modes: Vec<(String, String)>, mode: String) -> Preview {
        Preview { path: path.to_path_buf(), kind: Kind::Styled { lines, cut, by: by.into(), modes, mode }, target: None, carries: Vec::new(), view: ImageView::Pixels }
    }

    /// A preview of text we produced ourselves rather than read: a diff.
    pub fn from_text(path: &Path, text: &str, lang: Lang) -> Preview {
        let lines: Vec<String> = text.lines().map(str::to_string).collect();
        Preview {
            path: path.to_path_buf(),
            kind: Kind::Text { lines, lang, cut: false },
            target: None,
            carries: Vec::new(),
            view: ImageView::Pixels,
        }
    }

    pub fn load(path: &Path) -> Preview {
        let mk = |kind| Preview { path: path.to_path_buf(), kind, target: None, carries: Vec::new(), view: ImageView::Pixels };
        let note = |s: String| Preview {
            path: path.to_path_buf(),
            kind: Kind::Note(s),
            target: None,
            carries: Vec::new(),
            view: ImageView::Pixels,
        };

        let md = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) => return note(format!("failed to read: {}", why_unreadable(&e))),
        };
        if md.is_dir() {
            return match std::fs::read_dir(path) {
                Ok(_) => mk(Kind::Dir(None)),
                Err(e) => note(format!("can't open this folder: {}", why_unreadable(&e))),
            };
        }
        // FIFO, socket, device: `read` on these blocks waiting for a writer, and the daemon has no tty to interrupt it.
        if !md.is_file() {
            return note("not a regular file".to_string());
        }

        let mut f = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) => return note(format!("failed to read: {e}")),
        };
        let mut head = Vec::new();
        if let Err(e) = f.by_ref().take(SNIFF as u64).read_to_end(&mut head) {
            return note(format!("failed to read: {e}"));
        }

        if let Some(format) = ImageFormat::sniff(&head) {
            if md.len() > IMAGE_LIMIT {
                return note(format!("{}, {} - more than 4 MiB, not showing", format.name(), kib(md.len())));
            }
            let mut bytes = head;
            if let Err(e) = f.take(IMAGE_LIMIT).read_to_end(&mut bytes) {
                return note(format!("failed to read: {e}"));
            }
            let dims = image_size(&bytes, format);
            // A small file can still hold a huge picture, and decoding it is the terminal's memory or sips's.
            if let Some((w, h)) = dims.filter(|(w, h)| u64::from(*w) * u64::from(*h) > MAX_PIXELS) {
                return note(format!("{}, {w}x{h} - more than 50 megapixels, not showing", format.name()));
            }
            // Kitty takes PNG as is; other formats go through the system converter, and only once Kitty is there to show them.
            let png = (format == ImageFormat::Png).then_some(Some(Arc::new(bytes)));
            return mk(Kind::Image { format, size: md.len(), dims, png, pixels: None });
        }

        if head.contains(&0) {
            return mk(Kind::Binary { size: md.len() });
        }

        let mut bytes = head;
        let rest = PREVIEW_LIMIT.saturating_sub(bytes.len() as u64);
        if let Err(e) = f.take(rest).read_to_end(&mut bytes) {
            return note(format!("failed to read: {e}"));
        }

        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<String> = text
            .split('\n')
            .map(|l| l.trim_end_matches('\r').replace('\t', "    "))
            .collect();
        let lang = lang_of(path, lines.first().map(String::as_str).unwrap_or(""));

        let mut c = Carry::default();
        let carries = lines
            .iter()
            .map(|l| {
                let entry = c;
                scan(l, lang, &mut c);
                entry
            })
            .collect();

        Preview {
            path: path.to_path_buf(),
            kind: Kind::Text { lines, lang, cut: md.len() > PREVIEW_LIMIT },
            target: None,
            carries,
            view: ImageView::Pixels,
        }
    }

    /// What the current view still needs made for this image, if anything.
    pub fn need(&self) -> Option<Need> {
        let Kind::Image { png, dims, pixels, .. } = &self.kind else { return None };
        match self.view {
            ImageView::Pixels if png.is_none() => Some(Need::Png(self.path.clone())),
            ImageView::Blocks { .. } if pixels.is_none() => Some(Need::Pixels(self.path.clone(), *dims)),
            _ => None,
        }
    }

    pub fn give_png(&mut self, made: Option<Arc<Vec<u8>>>) {
        if let Kind::Image { png, dims, .. } = &mut self.kind {
            // What the converter wrote is what Kitty lays out.
            if let Some(d) = made.as_deref().and_then(|b| png_size(b)) {
                *dims = Some(d);
            }
            *png = Some(made);
        }
    }

    pub fn give_pixels(&mut self, made: Option<Arc<Pixels>>) {
        if let Kind::Image { pixels, .. } = &mut self.kind {
            *pixels = Some(made);
        }
    }

    pub fn line_count(&self) -> usize {
        match &self.kind {
            Kind::Text { lines, .. } => lines.len(),
            Kind::Styled { lines, .. } => lines.len(),
            _ => 0,
        }
    }

    /// How far the text can scroll sideways in `width` cells: until the longest line's end is in sight.
    pub fn side_room(&self, width: u16) -> usize {
        let (gut, widest) = match &self.kind {
            Kind::Text { lines, .. } => (self.gutter() as usize, lines.iter().map(|l| l.chars().count()).max().unwrap_or(0)),
            Kind::Styled { lines, .. } => (0, lines.iter().map(|l| l.iter().map(|(t, _)| t.chars().count()).sum()).max().unwrap_or(0)),
            _ => return 0,
        };
        widest.saturating_sub((width as usize).saturating_sub(gut))
    }

    pub fn goto(&mut self, line: u32) {
        self.target = Some(line.max(1));
    }

    /// Centers the line, but never scrolls past the bottom of the file.
    pub fn scroll_for(&self, line: u32, h: u16) -> usize {
        let i = line.saturating_sub(1) as usize;
        let top = i.saturating_sub(h as usize / 2);
        top.min(self.line_count().saturating_sub(h as usize))
    }

    /// `hscroll`: characters cut from the start of every line; ignored when `wrap` is on.
    pub fn draw(&self, buf: &mut Buffer, area: Rect, scroll: usize, hscroll: usize, wrap: bool, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        match &self.kind {
            // a note can be a sentence of advice, so it wraps rather than running off the edge
            Kind::Note(s) => {
                for (k, l) in crate::menu::wrap(s, area.width as usize).iter().take(area.height as usize).enumerate() {
                    one_line(buf, Rect { y: area.y + k as u16, ..area }, l, theme.muted);
                }
            }
            Kind::Binary { size } => one_line(buf, area, &format!("binary file, {size} bytes"), theme.muted),
            Kind::Dir(_) => one_line(buf, area, "directory", theme.muted),
            Kind::Image { .. } => self.draw_image(buf, area, theme),
            Kind::Text { lines, lang, cut } => {
                let run = |i: usize| {
                    let mut carry = self.carries.get(i).copied().unwrap_or_default();
                    scan(&lines[i], *lang, &mut carry).into_iter().map(|(seg, t)| (seg, style(t, theme))).collect()
                };
                self.draw_lines(buf, area, scroll, hscroll, wrap, lines.len(), self.gutter() as usize, run, theme);
                if *cut {
                    buf.set_stringn(area.x, area.bottom() - 1, "... truncated at 64 KiB", area.width as usize, Style::default().fg(theme.faint));
                }
            }
            Kind::Styled { lines, cut, .. } => {
                let run = |i: usize| lines[i].iter().map(|(t, r)| (t.as_str(), Style::default().fg(r.color(theme)))).collect();
                self.draw_lines(buf, area, scroll, hscroll, wrap, lines.len(), 0, run, theme);
                if *cut {
                    buf.set_stringn(area.x, area.bottom() - 1, "... cut short by its source", area.width as usize, Style::default().fg(theme.faint));
                }
            }
        }
    }

    /// Lines from `scroll` on, each as runs of text in their style, after `gut` columns of line numbers.
    #[allow(clippy::too_many_arguments)]
    fn draw_lines<'a>(&self, buf: &mut Buffer, area: Rect, scroll: usize, hscroll: usize, wrap: bool, count: usize, gut: usize, run: impl Fn(usize) -> Vec<(&'a str, Style)>, theme: &Theme) {
        let w = area.width as usize;
        let mut y = area.y;
        for i in (0..count).skip(scroll) {
            if y >= area.bottom() {
                break;
            }
            let first = y;
            if gut > 0 {
                buf.set_stringn(area.x, y, format!("{:>digits$} ", i + 1, digits = gut - 1), w, Style::default().fg(theme.faint));
            }
            if gut < w {
                let (left, right) = (area.x + gut as u16, area.right());
                let mut x = left;
                let mut skip = if wrap { 0 } else { hscroll };
                // one character at a time, so a wide one that will not fit starts the next row
                'line: for (seg, st) in run(i) {
                    for c in seg.chars() {
                        if skip > 0 {
                            skip -= 1;
                            continue;
                        }
                        let mut tmp = [0u8; 4];
                        let (nx, _) = buf.set_stringn(x, y, c.encode_utf8(&mut tmp), (right - x) as usize, st);
                        if nx > x {
                            x = nx;
                            continue;
                        }
                        // a wrapped row has no number of its own: the gutter counts file lines
                        if !wrap || y + 1 >= area.bottom() {
                            break 'line;
                        }
                        y += 1;
                        x = left;
                        x = buf.set_stringn(x, y, c.encode_utf8(&mut tmp), (right - x) as usize, st).0;
                    }
                }
            }
            // `set_style` only touches the fields given, so the text keeps its colour over the band
            if self.target == Some(i as u32 + 1) {
                buf.set_style(Rect::new(area.x, first, area.width, y - first + 1), Style::default().bg(theme.slab));
            }
            y += 1;
        }
    }

    fn draw_image(&self, buf: &mut Buffer, area: Rect, theme: &Theme) {
        let view = self.view;
        let Kind::Image { format, size, dims, png, pixels } = &self.kind else {
            return;
        };
        let dims = *dims;
        let mut caption = format!("{}, {}{}", format.name(), dims.map(|(w, h)| format!("{w}×{h}, ")).unwrap_or_default(), kib(*size));
        let hint = match view {
            ImageView::Pixels | ImageView::Off => None,
            ImageView::Blocks { hint } => {
                match pixels {
                    // Not made yet: the caption alone until it is.
                    None => None,
                    Some(Some(px)) => {
                        let hint = match hint {
                            BlockHint::None => None,
                            BlockHint::Terminal => Some("low resolution: no image support in this terminal\nGhostty, Kitty or WezTerm show full quality"),
                            BlockHint::Tmux => Some("low resolution: tmux carries no images\nrun nerd outside tmux for full quality"),
                        };
                        draw_blocks(buf, area, px, &caption, hint, theme);
                        return;
                    }
                    Some(None) => Some("could not read the image (sips)"),
                }
            }
        };
        // Pixels: the terminal draws the image itself, so the buffer only carries the caption,
        // in the same place the blocks layout would put it.
        let failed = matches!(png, Some(None));
        if view == ImageView::Pixels
            && let Some((pw, ph)) = dims.filter(|_| !failed)
        {
            let (_, caption_y) = image_layout(pw, ph, area, 1);
            let w = caption.chars().count() as u16;
            let x = area.x + area.width.saturating_sub(w) / 2;
            buf.set_stringn(x, caption_y, &caption, area.width as usize, Style::default().fg(theme.faint));
            return;
        }

        let mut msg = vec![std::mem::take(&mut caption)];
        if failed && view == ImageView::Pixels {
            msg.push("could not convert to png (sips)".to_string());
        } else if let Some(h) = hint {
            msg.push(h.to_string());
        }
        // Nothing to show: a box with what we know about the file.
        Block::bordered().border_style(Style::default().fg(theme.faint)).render(area, buf);
        let top = area.y + area.height.saturating_sub(msg.len() as u16) / 2;
        for (k, line) in msg.iter().enumerate() {
            let y = top + k as u16;
            if y >= area.bottom() {
                break;
            }
            let w = line.chars().count() as u16;
            let x = area.x + area.width.saturating_sub(w) / 2;
            buf.set_stringn(x, y, line, area.width as usize, Style::default().fg(theme.faint));
        }
    }

    /// Kitty graphics protocol bytes: clear image `id`, move the cursor, send as PNG; None until the PNG is there.
    pub fn image_bytes(&self, area: Rect, id: u32) -> Option<Vec<u8>> {
        let Kind::Image { png: Some(Some(bytes)), .. } = &self.kind else {
            return None;
        };
        if area.width == 0 || area.height == 0 {
            return None;
        }
        let (pw, ph) = png_size(bytes)?;
        // The same layout the caption was drawn from, or the two would not line up.
        let (at, _) = image_layout(pw, ph, area, 1);
        let (cols, rows) = (at.width, at.height);

        Some(png_bytes(bytes, at.x, at.y, &format!("c={cols},r={rows}"), id))
    }
}

/// Kitty graphics protocol bytes for a PNG: clear image `id`, move the cursor to (`x`, `y`), send it
/// scaled to `size` (`c=…,r=…`, or only one of them to keep the aspect).
pub fn png_bytes(png: &[u8], x: u16, y: u16, size: &str, id: u32) -> Vec<u8> {
    let mut out = clear_image_bytes(id);
    // The terminal numbers cursor position from one.
    out.extend(format!("\x1b[{};{}H", y + 1, x + 1).into_bytes());

    let b64 = base64(png);
    let mut at = 0;
    let mut first = true;
    while at < b64.len() {
        let end = (at + CHUNK).min(b64.len());
        let more = usize::from(end < b64.len());
        // q=2 mutes terminal replies (else they'd arrive at the client as input); C=1 keeps the cursor in place.
        if first {
            out.extend(format!("\x1b_Ga=T,f=100,i={id},{size},C=1,q=2,m={more};{}\x1b\\", &b64[at..end]).into_bytes());
            first = false;
        } else {
            out.extend(format!("\x1b_Gm={more};{}\x1b\\", &b64[at..end]).into_bytes());
        }
        at = end;
    }
    out
}

/// Converts via macOS's `sips`, which the sandbox allows running (no image decoder in the dependency tree).
/// Slow (0.4 s for 12 MP): only ever called off the loop.
pub fn to_png(path: &Path) -> Option<Arc<Vec<u8>>> {
    convert_to_png(path).ok().map(Arc::new)
}

fn convert_to_png(path: &Path) -> Result<Vec<u8>, String> {
    let tmp = temp_path("png");
    let out = crate::spawn::cmd("/usr/bin/sips")
        .args(["-s", "format", "png"])
        .arg(path)
        .arg("--out")
        .arg(&tmp)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("sips did not run: {e}"))?;
    let result = if out.status.success() {
        match std::fs::read(&tmp) {
            Ok(b) if png_size(&b).is_some() => Ok(b),
            Ok(b) => Err(format!("sips wrote {} bytes that are not a PNG", b.len())),
            Err(e) => Err(format!("sips reported success but {}: {e}", tmp.display())),
        }
    } else {
        Err(format!(
            "sips {}: {}{}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim(),
            String::from_utf8_lossy(&out.stdout).trim()
        ))
    };
    let _ = std::fs::remove_file(&tmp);
    result
}

/// Unique per call, in the system temp dir: tests swap and delete the state dir while others convert.
fn temp_path(ext: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!("nerd-preview-{}-{n}.{ext}", std::process::id()))
}

/// Off the loop, like `to_png`; unknown dimensions are scaled down too.
pub fn to_pixels(path: &Path, dims: Option<(u32, u32)>) -> Option<Arc<Pixels>> {
    let tmp = temp_path("bmp");
    let mut cmd = crate::spawn::cmd("/usr/bin/sips");
    // Without a colour profile sips writes palette PNGs as headerless raw pixels, and gray+alpha as a format we don't read.
    cmd.args(["-s", "format", "bmp", "-m", "/System/Library/ColorSync/Profiles/sRGB Profile.icc"]);
    // -Z upscales too, so only a larger image gets it.
    if dims.is_none_or(|(w, h)| w.max(h) > BLOCK_PIXELS) {
        cmd.args(["-Z", &BLOCK_PIXELS.to_string()]);
    }
    let ok = cmd
        .arg(path)
        .arg("--out")
        .arg(&tmp)
        .stdin(std::process::Stdio::null())
        .output()
        .is_ok_and(|o| o.status.success());
    let px = if ok { std::fs::read(&tmp).ok().and_then(|b| parse_bmp(&b)).map(Arc::new) } else { None };
    let _ = std::fs::remove_file(&tmp);
    px
}

/// Only what `sips` writes: 24 bpp uncompressed, or 32 bpp BGRA bitfields.
fn parse_bmp(b: &[u8]) -> Option<Pixels> {
    let u16_at = |o: usize| b.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]]));
    let u32_at = |o: usize| b.get(o..o + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]));
    if b.get(0..2)? != b"BM" || !matches!(u32_at(14)?, 40 | 108 | 124) {
        return None;
    }
    let off = u32_at(10)? as usize;
    let w = u32_at(18)? as i32;
    let h = u32_at(22)? as i32;
    let bpp = match (u16_at(28)?, u32_at(30)?) {
        (24, 0) => 3,
        (32, 3) if [u32_at(54)?, u32_at(58)?, u32_at(62)?] == [0x00ff_0000, 0x0000_ff00, 0x0000_00ff] => 4,
        _ => return None,
    };
    if w <= 0 || h == 0 || h == i32::MIN {
        return None;
    }
    let (w, hh) = (w as usize, h.unsigned_abs() as usize);
    let stride = (w.checked_mul(bpp)?).div_ceil(4) * 4;
    let end = off.checked_add(stride.checked_mul(hh)?)?;
    let data = b.get(off..end)?;
    let mut rgba = Vec::with_capacity(w.checked_mul(hh)?.checked_mul(4)?);
    for y in 0..hh {
        // Positive height is stored bottom row first.
        let row = if h < 0 { y } else { hh - 1 - y };
        let line = &data[row * stride..row * stride + w * bpp];
        for p in line.chunks_exact(bpp) {
            let a = if bpp == 4 { p[3] } else { 255 };
            rgba.extend([p[2], p[1], p[0], a]);
        }
    }
    Some(Pixels { w, h: hh, rgba })
}

/// Quadrant glyphs by bitmask, bit 0 top-left, 1 top-right, 2 bottom-left, 3 bottom-right.
/// Unicode's Block Elements. SF Mono (Terminal.app's default) and Menlo carry all of them;
/// Courier New, PT Mono and Andale Mono have only the halves and fall back for the quadrants.
/// Index 15 never comes up: all four samples above their own mean is impossible.
const QUADRANTS: [&str; 16] = [
    " ", "\u{2598}", "\u{259d}", "\u{2580}", "\u{2596}", "\u{258c}", "\u{259e}", "\u{259b}", "\u{2597}",
    "\u{259a}", "\u{2590}", "\u{259c}", "\u{2584}", "\u{2599}", "\u{259f}", "\u{2588}",
];

/// Four samples per cell, two colours: the brighter samples go to the foreground glyph,
/// the darker ones to the background. Twice the horizontal detail of half blocks.
fn draw_blocks(buf: &mut Buffer, area: Rect, px: &Pixels, caption: &str, hint: Option<&str>, theme: &Theme) {
    let lines: Vec<&str> = std::iter::once(caption).chain(hint.into_iter().flat_map(|h| h.lines())).collect();
    let (img_at, caption_y) = image_layout(px.w as u32, px.h as u32, area, lines.len() as u16);
    if img_at.width > 0 && img_at.height > 0 {
        let (cols, rows) = (img_at.width, img_at.height);
        let (x0, y0) = (img_at.x, img_at.y);
        let ground = match theme.ground {
            ratatui::style::Color::Rgb(r, g, b) => [r, g, b],
            _ => [0, 0, 0],
        };
        // Sample grid: two columns and two rows inside every cell.
        let (sx, sy) = (cols as usize * 2, rows as usize * 2);
        let span = |i: usize, n: usize, len: usize| (i * len / n, ((i + 1) * len / n).max(i * len / n + 1));
        for cy in 0..rows as usize {
            for cx in 0..cols as usize {
                let quads = [(0, 0), (1, 0), (0, 1), (1, 1)].map(|(qx, qy): (usize, usize)| {
                    average(px, span(cx * 2 + qx, sx, px.w), span(cy * 2 + qy, sy, px.h), ground)
                });
                let (symbol, fg, bg) = cell_of(quads);
                let rgb = |c: [u8; 3]| ratatui::style::Color::Rgb(c[0], c[1], c[2]);
                buf[(x0 + cx as u16, y0 + cy as u16)].set_symbol(symbol).set_fg(rgb(fg)).set_bg(rgb(bg));
            }
        }
    }
    for (k, line) in lines.iter().enumerate() {
        let y = caption_y + k as u16;
        if y >= area.bottom() {
            break;
        }
        let w = line.chars().count() as u16;
        let x = area.x + area.width.saturating_sub(w) / 2;
        buf.set_stringn(x, y, line, area.width as usize, Style::default().fg(theme.faint));
    }
}

/// Splits four samples at their mean brightness: above it draws in the foreground, the rest is background.
fn cell_of(quads: [[u8; 3]; 4]) -> (&'static str, [u8; 3], [u8; 3]) {
    let lum = |c: [u8; 3]| c[0] as u32 * 299 + c[1] as u32 * 587 + c[2] as u32 * 114;
    let mean = quads.iter().map(|c| lum(*c)).sum::<u32>() / 4;
    let mut mask = 0usize;
    let (mut fg, mut bg, mut nf, mut nb) = ([0u32; 3], [0u32; 3], 0u32, 0u32);
    for (i, c) in quads.iter().enumerate() {
        let (sum, n) = if lum(*c) > mean { mask |= 1 << i; (&mut fg, &mut nf) } else { (&mut bg, &mut nb) };
        for k in 0..3 {
            sum[k] += c[k] as u32;
        }
        *n += 1;
    }
    // All four alike: one flat colour, drawn as the background of a blank cell.
    let mean_of = |sum: [u32; 3], n: u32| if n == 0 { quads[0] } else { sum.map(|v| (v / n) as u8) };
    (QUADRANTS[mask], mean_of(fg, nf), mean_of(bg, nb))
}

/// Mean colour of a pixel box, blended over `ground` by alpha.
fn average(px: &Pixels, (x0, x1): (usize, usize), (y0, y1): (usize, usize), ground: [u8; 3]) -> [u8; 3] {
    let (x1, y1) = (x1.min(px.w), y1.min(px.h));
    let mut sum = [0u64; 3];
    let mut n = 0u64;
    for y in y0..y1 {
        for x in x0..x1 {
            let p = &px.rgba[(y * px.w + x) * 4..][..4];
            let a = p[3] as u64;
            for c in 0..3 {
                sum[c] += p[c] as u64 * a + ground[c] as u64 * (255 - a);
            }
            n += 255;
        }
    }
    if n == 0 {
        return ground;
    }
    sum.map(|v| (v / n) as u8)
}

pub fn clear_image_bytes(id: u32) -> Vec<u8> {
    format!("\x1b_Ga=d,d=i,i={id}\x1b\\").into_bytes()
}

/// Width and height from the header, without decoding: what the caption and the layout need before any pixels exist.
/// The most pixels a preview decodes or hands to the terminal.
const MAX_PIXELS: u64 = 50_000_000;

fn image_size(b: &[u8], format: ImageFormat) -> Option<(u32, u32)> {
    let u16_le = |o: usize| b.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]]) as u32);
    let u16_be = |o: usize| b.get(o..o + 2).map(|s| u16::from_be_bytes([s[0], s[1]]) as u32);
    let u24_le = |o: usize| b.get(o..o + 3).map(|s| u32::from_le_bytes([s[0], s[1], s[2], 0]));
    let (w, h) = match format {
        ImageFormat::Png => return png_size(b),
        ImageFormat::Gif => (u16_le(6)?, u16_le(8)?),
        ImageFormat::Webp => match b.get(12..16)? {
            b"VP8 " => (u16_le(26)? & 0x3fff, u16_le(28)? & 0x3fff),
            b"VP8L" => {
                let bits = u32::from_le_bytes(b.get(21..25)?.try_into().ok()?);
                (1 + (bits & 0x3fff), 1 + ((bits >> 14) & 0x3fff))
            }
            b"VP8X" => (1 + u24_le(24)?, 1 + u24_le(27)?),
            _ => return None,
        },
        ImageFormat::Jpeg => {
            // Segments after SOI: the first start-of-frame holds the size; a thumbnail's lives inside APP1 and is skipped with it.
            let mut at = 2;
            loop {
                while *b.get(at)? == 0xff && *b.get(at + 1)? == 0xff {
                    at += 1;
                }
                if *b.get(at)? != 0xff {
                    return None;
                }
                let m = *b.get(at + 1)?;
                if (0xc0..=0xcf).contains(&m) && !matches!(m, 0xc4 | 0xc8 | 0xcc) {
                    break (u16_be(at + 7)?, u16_be(at + 5)?);
                }
                at += 2 + u16_be(at + 2)? as usize;
            }
        }
    };
    (w > 0 && h > 0).then_some((w, h))
}

/// The IHDR chunk is always first, at fixed offsets - no need for a real PNG parser.
fn png_size(b: &[u8]) -> Option<(u32, u32)> {
    if b.len() < 24 || !b.starts_with(b"\x89PNG\r\n\x1a\n") || &b[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(b[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(b[20..24].try_into().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}

/// Centred in what is left above the caption: one layout, so pixels and blocks land alike.
fn image_layout(pw: u32, ph: u32, area: Rect, caption_lines: u16) -> (Rect, u16) {
    let text_h = caption_lines.min(area.height.saturating_sub(1));
    let above = Rect { height: area.height - text_h, ..area };
    if above.width == 0 || above.height == 0 {
        return (Rect::new(area.x, area.y, 0, 0), area.y);
    }
    let (cols, rows) = fit(pw, ph, above);
    let at = Rect::new(above.x + (above.width - cols) / 2, above.y + (above.height - rows) / 2, cols, rows);
    (at, at.bottom())
}

/// Assumes a 1:2 cell aspect ratio (the terminal doesn't report its own). Never upscales.
fn fit(pw: u32, ph: u32, area: Rect) -> (u16, u16) {
    let nat_c = pw as f64;
    let nat_r = ph as f64 / 2.0;
    let s = (area.width as f64 / nat_c).min(area.height as f64 / nat_r).min(1.0);
    let c = ((nat_c * s).round() as u64).clamp(1, area.width as u64) as u16;
    let r = ((nat_r * s).round() as u64).clamp(1, area.height as u64) as u16;
    (c, r)
}

/// ceiling: hand-rolled instead of a dependency; duplicated from select.rs rather than sharing a module for this trifle.
fn base64(data: &[u8]) -> String {
    const ABC: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = u32::from_be_bytes([0, b[0], b[1], b[2]]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ABC[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn kib(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{} KiB", n / 1024)
    } else {
        format!("{:.1} MiB", n as f64 / (1024.0 * 1024.0))
    }
}

/// Why something will not open, in words: the OS error alone reads like a fault in nerd.
pub fn why_unreadable(e: &std::io::Error) -> String {
    match e.raw_os_error() {
        // EPERM: macOS privacy keeps Desktop, Documents, Downloads and the like from a process not allowed in
        Some(1) => "macOS keeps it private. Allow the terminal nerd was started from in System Settings, \
                    Privacy & Security, Full Disk Access"
            .into(),
        Some(13) => "you have no permission to read it".into(),
        _ => e.to_string(),
    }
}

fn one_line(buf: &mut Buffer, area: Rect, s: &str, color: ratatui::style::Color) {
    buf.set_stringn(area.x, area.y, s, area.width as usize, Style::default().fg(color));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tok {
    /// A line the diff takes away.
    Gone,
    Comment,
    Str,
    Num,
    /// Language keyword or a key in json/toml/yaml.
    Key,
    Head,
    Fence,
    Link,
    Plain,
}

/// A line as the highlighter colours it, by role name: what the public built-in previews answer with.
pub(crate) fn roles(line: &str, lang: Lang) -> Vec<(&str, &'static str)> {
    let role = |t| match t {
        Tok::Comment | Tok::Fence => "faint",
        Tok::Gone => "bad",
        Tok::Str => "ok",
        Tok::Num | Tok::Link => "teal",
        Tok::Key | Tok::Head => "accent",
        Tok::Plain => "text",
    };
    scan(line, lang, &mut Carry::default()).into_iter().map(|(s, t)| (s, role(t))).collect()
}

fn style(t: Tok, th: &Theme) -> Style {
    let s = Style::default();
    match t {
        Tok::Comment | Tok::Fence => s.fg(th.faint),
        Tok::Gone => s.fg(th.bad),
        Tok::Str => s.fg(th.ok),
        Tok::Num | Tok::Link => s.fg(th.teal),
        Tok::Key => s.fg(th.accent),
        Tok::Head => s.fg(th.accent).add_modifier(Modifier::BOLD),
        Tok::Plain => s.fg(th.text),
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Carry {
    /// `/* ...` is open.
    block: bool,
    /// A Python triple quote is open.
    triple: bool,
    /// Inside ``` ... ``` in markdown.
    fence: bool,
}

struct Rules {
    line: &'static [&'static str],
    block: bool,
    triple: bool,
    /// A single quote is a character, not a string (Rust: lifetime `'a`).
    char_quote: bool,
    /// Highlight `key:` and `key =` at the start of a line (json/toml/yaml).
    keys: bool,
    kw: &'static [&'static str],
}

const KW_RUST: &[&str] = &[
    "fn", "let", "mut", "const", "static", "struct", "enum", "impl", "trait", "use", "pub", "mod",
    "match", "if", "else", "for", "while", "loop", "return", "break", "continue", "where", "as",
    "self", "crate", "async", "await", "move", "dyn", "type", "ref", "in", "true", "false",
];
const KW_JS: &[&str] = &[
    "function", "const", "let", "var", "class", "if", "else", "for", "while", "return", "import",
    "export", "default", "from", "new", "async", "await", "try", "catch", "throw", "typeof",
    "this", "null", "undefined", "true", "false", "switch", "case", "break", "continue",
];
const KW_TS: &[&str] = &[
    "function", "const", "let", "var", "class", "if", "else", "for", "while", "return", "import",
    "export", "default", "from", "new", "async", "await", "try", "catch", "throw", "typeof",
    "this", "null", "undefined", "true", "false", "switch", "case", "break", "continue",
    "interface", "type", "enum", "implements", "extends", "public", "private", "readonly", "as",
];
const KW_PY: &[&str] = &[
    "def", "class", "import", "from", "as", "if", "elif", "else", "for", "while", "return",
    "yield", "try", "except", "finally", "raise", "with", "lambda", "pass", "break", "continue",
    "and", "or", "not", "in", "is", "None", "True", "False", "global", "async", "await", "self",
];
const KW_SH: &[&str] = &[
    "if", "then", "else", "elif", "fi", "for", "while", "do", "done", "case", "esac", "function",
    "return", "local", "export", "source", "in",
];
const KW_NONE: &[&str] = &[];

fn rules(lang: Lang) -> Rules {
    let base = Rules { line: &[], block: false, triple: false, char_quote: false, keys: false, kw: KW_NONE };
    match lang {
        Lang::Rust => Rules { line: &["//"], block: true, char_quote: true, kw: KW_RUST, ..base },
        Lang::Js => Rules { line: &["//"], block: true, kw: KW_JS, ..base },
        Lang::Ts => Rules { line: &["//"], block: true, kw: KW_TS, ..base },
        Lang::Python => Rules { line: &["#"], triple: true, kw: KW_PY, ..base },
        Lang::Shell => Rules { line: &["#"], kw: KW_SH, ..base },
        Lang::Json => Rules { keys: true, ..base },
        Lang::Toml => Rules { line: &["#"], keys: true, ..base },
        Lang::Yaml => Rules { line: &["#"], keys: true, ..base },
        Lang::Diff | Lang::Markdown | Lang::Plain => base,
    }
}

fn lang_of(path: &Path, first: &str) -> Lang {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let by_ext = match ext.as_str() {
        "rs" => Lang::Rust,
        "js" | "mjs" | "cjs" | "jsx" => Lang::Js,
        "ts" | "tsx" | "mts" => Lang::Ts,
        "py" | "pyi" => Lang::Python,
        "sh" | "bash" | "zsh" => Lang::Shell,
        "json" => Lang::Json,
        "toml" => Lang::Toml,
        "yaml" | "yml" => Lang::Yaml,
        "md" | "markdown" => Lang::Markdown,
        _ => Lang::Plain,
    };
    if by_ext != Lang::Plain || !first.starts_with("#!") {
        return by_ext;
    }
    if first.contains("python") {
        Lang::Python
    } else if first.contains("node") || first.contains("bun") {
        Lang::Js
    } else if first.contains("sh") {
        Lang::Shell
    } else {
        Lang::Plain
    }
}

/// Chunks are slices of `line`; joined in order they reconstruct it exactly.
fn scan<'a>(line: &'a str, lang: Lang, carry: &mut Carry) -> Vec<(&'a str, Tok)> {
    match lang {
        Lang::Diff => scan_diff(line),
        Lang::Markdown => scan_md(line, carry),
        Lang::Plain => {
            if line.is_empty() { Vec::new() } else { vec![(line, Tok::Plain)] }
        }
        _ => scan_code(line, &rules(lang), carry),
    }
}

/// A diff line is read off its first character, the way `git` prints it.
fn scan_diff(line: &str) -> Vec<(&str, Tok)> {
    if line.is_empty() {
        return Vec::new();
    }
    let t = match line.as_bytes()[0] {
        b'+' if !line.starts_with("+++") => Tok::Str,
        b'-' if !line.starts_with("---") => Tok::Gone,
        b'@' => Tok::Num,
        b'd' | b'i' | b'n' | b'+' | b'-' => Tok::Comment,
        _ => Tok::Plain,
    };
    vec![(line, t)]
}

fn scan_md<'a>(line: &'a str, carry: &mut Carry) -> Vec<(&'a str, Tok)> {
    let t = line.trim_start();
    if t.starts_with("```") || t.starts_with("~~~") {
        carry.fence = !carry.fence;
        return vec![(line, Tok::Fence)];
    }
    if carry.fence {
        return vec![(line, Tok::Plain)];
    }
    if t.starts_with('#') {
        return vec![(line, Tok::Head)];
    }

    let mut out = Vec::new();
    let mut i = 0;
    while let Some(p) = line[i..].find('[') {
        let s = i + p;
        let Some(close) = line[s..].find("](") else { break };
        let Some(end) = line[s + close..].find(')') else { break };
        let e = s + close + end + 1;
        if s > i {
            out.push((&line[i..s], Tok::Plain));
        }
        out.push((&line[s..e], Tok::Link));
        i = e;
    }
    if i < line.len() {
        out.push((&line[i..], Tok::Plain));
    }
    out
}

/// Delimiters are single-byte ASCII, so byte offsets always land on a char boundary; multi-byte sequences pass through untouched.
fn scan_code<'a>(line: &'a str, r: &Rules, carry: &mut Carry) -> Vec<(&'a str, Tok)> {
    let b = line.as_bytes();
    let mut out: Vec<(&str, Tok)> = Vec::new();
    let mut i = 0;

    if carry.block {
        match line.find("*/") {
            Some(e) => {
                out.push((&line[..e + 2], Tok::Comment));
                i = e + 2;
                carry.block = false;
            }
            None => return vec![(line, Tok::Comment)],
        }
    } else if carry.triple {
        match line.find("\"\"\"") {
            Some(e) => {
                out.push((&line[..e + 3], Tok::Str));
                i = e + 3;
                carry.triple = false;
            }
            None => return vec![(line, Tok::Str)],
        }
    }

    let key = if r.keys && i == 0 { key_span(line) } else { None };
    let mut plain = i;

    while i < b.len() {
        if let Some((_, ke)) = key.filter(|k| k.0 == i) {
            flush(&mut out, line, &mut plain, i);
            out.push((&line[i..ke], Tok::Key));
            i = ke;
            plain = i;
            continue;
        }
        let rest = &line[i..];

        if r.line.iter().any(|p| rest.starts_with(p)) {
            flush(&mut out, line, &mut plain, i);
            out.push((rest, Tok::Comment));
            return out;
        }
        if r.block && rest.starts_with("/*") {
            flush(&mut out, line, &mut plain, i);
            match rest.find("*/") {
                Some(e) => {
                    out.push((&line[i..i + e + 2], Tok::Comment));
                    i += e + 2;
                }
                None => {
                    carry.block = true;
                    out.push((rest, Tok::Comment));
                    return out;
                }
            }
            plain = i;
            continue;
        }
        if r.triple && rest.starts_with("\"\"\"") {
            flush(&mut out, line, &mut plain, i);
            match rest[3..].find("\"\"\"") {
                Some(e) => {
                    out.push((&line[i..i + 3 + e + 3], Tok::Str));
                    i += 3 + e + 3;
                }
                None => {
                    carry.triple = true;
                    out.push((rest, Tok::Str));
                    return out;
                }
            }
            plain = i;
            continue;
        }

        let c = b[i];
        if c == b'"' || (c == b'\'' && (!r.char_quote || short_char(rest))) {
            flush(&mut out, line, &mut plain, i);
            let start = i;
            i += 1;
            while i < b.len() {
                if b[i] == b'\\' && c == b'"' {
                    i += 2;
                    continue;
                }
                let q = b[i] == c;
                i += 1;
                if q {
                    break;
                }
            }
            i = i.min(b.len());
            out.push((&line[start..i], Tok::Str));
            plain = i;
        } else if c.is_ascii_digit() {
            flush(&mut out, line, &mut plain, i);
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'.') {
                i += 1;
            }
            out.push((&line[start..i], Tok::Num));
            plain = i;
        } else if ident_start(c) {
            let start = i;
            while i < b.len() && ident_byte(b[i]) {
                i += 1;
            }
            let word = &line[start..i];
            let tok = if r.kw.contains(&word) {
                Tok::Key
            } else if r.keys && matches!(word, "true" | "false" | "null" | "yes" | "no") {
                Tok::Num
            } else {
                Tok::Plain
            };
            if tok != Tok::Plain {
                flush(&mut out, line, &mut plain, start);
                out.push((word, tok));
                plain = i;
            }
        } else {
            i += 1;
        }
    }
    flush(&mut out, line, &mut plain, b.len());
    out
}

fn flush<'a>(out: &mut Vec<(&'a str, Tok)>, line: &'a str, plain: &mut usize, to: usize) {
    if to > *plain {
        out.push((&line[*plain..to], Tok::Plain));
    }
    *plain = to;
}

fn ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80
}

fn ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

/// No closing quote nearby means a lifetime (`'a`), not a character literal.
fn short_char(rest: &str) -> bool {
    rest[1..].bytes().take(6).any(|c| c == b'\'')
}

/// `name:`, `"name":`, `name =`, `- name:`. Returns the name's bounds, without delimiting quotes.
fn key_span(line: &str) -> Option<(usize, usize)> {
    let b = line.as_bytes();
    let mut s = 0;
    while s < b.len() && (b[s] == b' ' || b[s] == b'\t') {
        s += 1;
    }
    if b.get(s) == Some(&b'-') && b.get(s + 1) == Some(&b' ') {
        s += 2;
        while s < b.len() && b[s] == b' ' {
            s += 1;
        }
    }
    let start = s;
    let e = if b.get(s) == Some(&b'"') {
        s + 1 + line[s + 1..].find('"')? + 1
    } else {
        let mut e = s;
        while e < b.len() && (ident_byte(b[e]) || matches!(b[e], b'-' | b'.' | b'$')) {
            e += 1;
        }
        e
    };
    if e == start {
        return None;
    }
    let mut j = e;
    while j < b.len() && b[j] == b' ' {
        j += 1;
    }
    matches!(b.get(j), Some(b':') | Some(b'=')).then_some((start, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static N: AtomicUsize = AtomicUsize::new(0);

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "nerd-preview-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn rm(p: &Path) {
        let _ = fs::remove_dir_all(p);
    }

    fn text(name: &str, body: &str) -> (PathBuf, Preview) {
        let d = tmp("txt");
        let f = d.join(name);
        fs::write(&f, body).unwrap();
        let p = Preview::load(&f);
        (d, p)
    }

    fn row(p: &Preview, w: u16, h: u16, y: u16) -> Vec<(String, Color)> {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        p.draw(&mut buf, area, 0, 0, false, &Theme::dark());
        (0..w).map(|x| (buf[(x, y)].symbol().to_string(), buf[(x, y)].fg)).collect()
    }

    fn color_of(cells: &[(String, Color)], needle: &str) -> Color {
        let text: String = cells.iter().map(|(s, _)| s.as_str()).collect();
        let at = text.find(needle).unwrap_or_else(|| panic!("no \"{needle}\" in \"{text}\""));
        // find gives a byte offset, but cells are per character.
        let chars = text[..at].chars().count();
        cells[chars].1
    }

    #[test]
    fn language_comes_from_extension_then_shebang() {
        let cases = [
            ("a.rs", Lang::Rust),
            ("a.tsx", Lang::Ts),
            ("a.mjs", Lang::Js),
            ("a.PY", Lang::Python),
            ("a.yml", Lang::Yaml),
            ("a.toml", Lang::Toml),
            ("a.json", Lang::Json),
            ("a.md", Lang::Markdown),
            ("a.txt", Lang::Plain),
        ];
        for (name, want) in cases {
            assert_eq!(lang_of(Path::new(name), ""), want, "{name}");
        }

        assert_eq!(lang_of(Path::new("run"), "#!/usr/bin/env python3"), Lang::Python);
        assert_eq!(lang_of(Path::new("run"), "#!/bin/bash"), Lang::Shell);
        assert_eq!(lang_of(Path::new("run"), "#!/usr/bin/env node"), Lang::Js);
        assert_eq!(lang_of(Path::new("run"), "#!/usr/bin/perl"), Lang::Plain);
        assert_eq!(lang_of(Path::new("a.rs"), "#!/bin/sh"), Lang::Rust);
    }

    #[test]
    fn rust_gets_keywords_strings_numbers_comments() {
        let th = Theme::dark();
        let (d, p) = text("a.rs", "let x = 42; // счёт\nlet s = \"да\";\n");
        let r0 = row(&p, 40, 4, 0);
        assert_eq!(color_of(&r0, "let"), th.accent, "keyword");
        assert_eq!(color_of(&r0, "42"), th.teal, "number");
        assert_eq!(color_of(&r0, "// счёт"), th.faint, "comment");
        assert_eq!(color_of(&r0, "x"), th.text, "identifier");
        let r1 = row(&p, 40, 4, 1);
        assert_eq!(color_of(&r1, "\"да\""), th.ok, "string");
        rm(&d);
    }

    #[test]
    fn rust_lifetime_is_not_a_string() {
        let th = Theme::dark();
        let (d, p) = text("a.rs", "fn f<'a>(s: &'a str) -> u8 { b'x' }\n");
        let r = row(&p, 60, 2, 0);
        assert_eq!(color_of(&r, "str"), th.text, "still code after the lifetime");
        assert_eq!(color_of(&r, "'x'"), th.ok, "character literal is a string");
        rm(&d);
    }

    #[test]
    fn block_comment_carries_across_lines() {
        let th = Theme::dark();
        let (d, p) = text("a.rs", "/* открыли\nвсё ещё внутри\nзакрыли */ let z = 1;\n");
        for (y, want) in ["открыли", "всё ещё внутри", "закрыли */"].into_iter().enumerate() {
            let r = row(&p, 40, 4, y as u16);
            assert_eq!(color_of(&r, want), th.faint, "line {y}");
        }
        let r2 = row(&p, 40, 4, 2);
        assert_eq!(color_of(&r2, "let"), th.accent, "code again after the closing");
        rm(&d);
    }

    #[test]
    fn python_triple_quote_carries_across_lines() {
        let th = Theme::dark();
        let (d, p) = text("a.py", "def f():\n    \"\"\"что делает\n    и зачем\"\"\"\n    return 1\n");
        assert_eq!(color_of(&row(&p, 40, 5, 0), "def"), th.accent);
        assert_eq!(color_of(&row(&p, 40, 5, 1), "что делает"), th.ok);
        assert_eq!(color_of(&row(&p, 40, 5, 2), "и зачем"), th.ok);
        assert_eq!(color_of(&row(&p, 40, 5, 3), "return"), th.accent);
        rm(&d);
    }

    #[test]
    fn json_toml_yaml_light_up_keys() {
        let th = Theme::dark();
        let (d1, j) = text("a.json", "{\n  \"имя\": \"ник\",\n  \"лет\": 7,\n  \"да\": true\n}\n");
        let r = row(&j, 40, 6, 1);
        assert_eq!(color_of(&r, "\"имя\""), th.accent, "json key");
        assert_eq!(color_of(&r, "\"ник\""), th.ok, "string value");
        assert_eq!(color_of(&row(&j, 40, 6, 2), "7"), th.teal);
        assert_eq!(color_of(&row(&j, 40, 6, 3), "true"), th.teal);
        rm(&d1);

        let (d2, t) = text("a.toml", "name = \"nerd\"\n# заметка\nport = 8080\n");
        assert_eq!(color_of(&row(&t, 40, 4, 0), "name"), th.accent);
        assert_eq!(color_of(&row(&t, 40, 4, 0), "\"nerd\""), th.ok);
        assert_eq!(color_of(&row(&t, 40, 4, 1), "# заметка"), th.faint);
        assert_eq!(color_of(&row(&t, 40, 4, 2), "8080"), th.teal);
        rm(&d2);

        let (d3, y) = text("a.yml", "jobs:\n  - name: сборка\n    run: cargo test\n");
        assert_eq!(color_of(&row(&y, 40, 4, 0), "jobs"), th.accent);
        assert_eq!(color_of(&row(&y, 40, 4, 1), "name"), th.accent);
        rm(&d3);
    }

    #[test]
    fn shell_and_js_get_their_own_keywords() {
        let th = Theme::dark();
        let (d1, s) = text("a.sh", "for f in *.rs; do echo \"$f\"; done # всё\n");
        let r = row(&s, 60, 2, 0);
        assert_eq!(color_of(&r, "for"), th.accent);
        assert_eq!(color_of(&r, "done"), th.accent);
        assert_eq!(color_of(&r, "# всё"), th.faint);
        rm(&d1);

        let (d2, j) = text("a.ts", "export interface A { n: number }\n");
        let r = row(&j, 60, 2, 0);
        assert_eq!(color_of(&r, "export"), th.accent);
        assert_eq!(color_of(&r, "interface"), th.accent, "ts-only word");
        rm(&d2);
    }

    #[test]
    fn markdown_gets_headings_fences_links() {
        let th = Theme::dark();
        let (d, p) = text(
            "a.md",
            "# Заголовок\nтекст [ссылка](http://x) дальше\n```rust\nlet x = 1;\n```\nконец\n",
        );
        let r0 = row(&p, 60, 7, 0);
        assert_eq!(color_of(&r0, "# Заголовок"), th.accent);
        let area = Rect::new(0, 0, 60, 7);
        let mut buf = Buffer::empty(area);
        p.draw(&mut buf, area, 0, 0, false, &th);
        assert!(buf[(4, 0)].modifier.contains(Modifier::BOLD), "heading is bold");

        let r1 = row(&p, 60, 7, 1);
        assert_eq!(color_of(&r1, "[ссылка](http://x)"), th.teal);
        assert_eq!(color_of(&r1, "дальше"), th.text);
        assert_eq!(color_of(&row(&p, 60, 7, 2), "```rust"), th.faint);
        assert_eq!(color_of(&row(&p, 60, 7, 3), "let"), th.text);
        assert_eq!(color_of(&row(&p, 60, 7, 5), "конец"), th.text);
        rm(&d);
    }

    #[test]
    fn a_long_line_scrolls_sideways_under_a_gutter_that_stays() {
        let th = Theme::dark();
        let (d, mut p) = text("long.txt", "0123456789abcdefghij\nshort\n");
        let area = Rect::new(0, 0, 10, 3);
        let line = |p: &mut Preview, h: usize| {
            let mut buf = Buffer::empty(area);
            p.draw(&mut buf, area, 0, h, false, &th);
            (0..2).map(|y| (0..10).map(|x| buf[(x, y)].symbol()).collect::<String>()).collect::<Vec<_>>()
        };
        assert_eq!(line(&mut p, 0), ["1 01234567", "2 short   "], "cut at the edge");
        assert_eq!(line(&mut p, 4), ["1 456789ab", "2 t       "], "four to the left, the numbers where they were");
        assert_eq!(p.side_room(10), 12, "until the longest line's end is in sight");
        rm(&d);
    }

    #[test]
    fn wrapped_rows_carry_no_number_of_their_own() {
        let th = Theme::dark();
        let (d, p) = text("long.txt", "0123456789abcdefghij\nshort\n");
        let area = Rect::new(0, 0, 10, 5);
        let mut buf = Buffer::empty(area);
        p.draw(&mut buf, area, 0, 7, true, &th);
        let rows: Vec<String> = (0..4).map(|y| (0..10).map(|x| buf[(x, y)].symbol()).collect()).collect();
        assert_eq!(rows, ["1 01234567", "  89abcdef", "  ghij    ", "2 short   "], "a sideways offset means nothing while wrapping");
        rm(&d);
    }

    #[test]
    fn line_numbers_and_goto_highlight() {
        let th = Theme::dark();
        let (d, mut p) = text("a.txt", &"line\n".repeat(12));
        p.goto(3);
        let area = Rect::new(0, 0, 30, 5);
        let mut buf = Buffer::empty(area);
        p.draw(&mut buf, area, 0, 0, false, &th);

        // 13 lines -> gutter is two digits plus a space.
        let first: String = (0..3).map(|x| buf[(x, 0)].symbol()).collect();
        assert_eq!(first, " 1 ");
        assert_eq!(buf[(1, 0)].fg, th.faint, "line number is faint");
        assert_eq!(buf[(3, 0)].symbol(), "l", "text right after the gutter");

        assert_eq!(buf[(0, 2)].bg, th.slab, "goto target has the highlight background, visible at 3:1");
        assert_eq!(buf[(29, 2)].bg, th.slab, "background reaches the right edge");
        assert_eq!(buf[(3, 2)].fg, th.text, "letters over the background keep their color");
        assert_ne!(buf[(0, 1)].bg, th.slab, "neighboring line has no background");
        rm(&d);
    }

    #[test]
    fn scroll_for_centres_the_line() {
        let (d, mut p) = text("a.txt", &"x\n".repeat(100));
        assert_eq!(p.line_count(), 101);
        // line 50 in an area 10 tall: top is left at 49-5
        assert_eq!(p.scroll_for(50, 10), 44);
        assert_eq!(p.scroll_for(1, 10), 0, "start doesn't scroll above zero");
        assert_eq!(p.scroll_for(101, 10), 91, "tail is pinned to the bottom of the area");
        assert_eq!(p.scroll_for(500, 10), 91, "same past the end of the file");
        p.goto(0);
        assert_eq!(p.target, Some(1), "there's no line zero");

        let short = Preview {
            path: PathBuf::new(),
            kind: Kind::Note("_".into()),
            target: None,
            carries: Vec::new(),
            view: ImageView::Pixels,
        };
        assert_eq!(short.scroll_for(5, 10), 0, "no text - no scrolling");
        rm(&d);
    }

    #[test]
    fn scrolled_draw_keeps_block_comment_state() {
        let th = Theme::dark();
        let (d, p) = text("a.rs", "/* начали\nодин\nдва\nтри\nчетыре */\n");
        let area = Rect::new(0, 0, 30, 2);
        let mut buf = Buffer::empty(area);
        // Start drawing from the middle of the comment: the flag comes from `carries`.
        p.draw(&mut buf, area, 2, 0, false, &th);
        assert_eq!(buf[(3, 0)].fg, th.faint, "third line is still a comment");
        rm(&d);
    }

    #[test]
    fn binary_dir_and_not_a_file_become_notes() {
        let d = tmp("kinds");
        fs::write(d.join("b.bin"), [0x7fu8, 0x45, 0x00, 0x01]).unwrap();
        assert!(matches!(Preview::load(&d.join("b.bin")).kind, Kind::Binary { size: 4 }));

        fs::create_dir(d.join("sub")).unwrap();
        fs::write(d.join("sub/a"), "a").unwrap();
        assert!(matches!(Preview::load(&d.join("sub")).kind, Kind::Dir(None)));

        match Preview::load(&d.join("нет-такого")).kind {
            Kind::Note(s) => assert!(s.starts_with("failed to read:"), "{s}"),
            _ => panic!("should be a note"),
        }

        // Character device: a plain `read` on a FIFO would wait for a writer.
        match Preview::load(Path::new("/dev/null")).kind {
            Kind::Note(s) => assert_eq!(s, "not a regular file"),
            _ => panic!("should be a note"),
        }
        rm(&d);
    }

    #[test]
    fn text_is_cut_at_64_kib() {
        let d = tmp("cut");
        let f = d.join("big.txt");
        fs::write(&f, "abcdefg\n".repeat(10 * 1024)).unwrap();
        let p = Preview::load(&f);
        match &p.kind {
            Kind::Text { lines, cut, .. } => {
                assert!(cut, "long file is marked as cut");
                assert_eq!(lines.len(), 8193, "64 KiB of 8-byte lines, plus an empty tail");
            }
            _ => panic!("should be text"),
        }
        assert_eq!(p.carries.len(), 8193, "there is state for every line");
        rm(&d);
    }

    #[test]
    fn tabs_and_crlf_are_normalised() {
        let (d, p) = text("a.txt", "\tone\r\n\t\ttwo\r\n");
        match &p.kind {
            Kind::Text { lines, .. } => {
                assert_eq!(lines[0], "    one");
                assert_eq!(lines[1], "        two");
            }
            _ => panic!("should be text"),
        }
        rm(&d);
    }

    /// Built by hand with real CRCs, not just placeholder bytes.
    fn png_1x1() -> Vec<u8> {
        let mut v = Vec::from(*b"\x89PNG\r\n\x1a\n");
        v.extend([0, 0, 0, 13]);
        v.extend(*b"IHDR");
        v.extend([0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0]);
        v.extend([0x90, 0x77, 0x53, 0xde]);
        v.extend([0, 0, 0, 12]);
        v.extend(*b"IDAT");
        v.extend([0x08, 0xd7, 0x63, 0xf8, 0xcf, 0xc0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00]);
        v.extend([0x18, 0xdd, 0x8d, 0xb0]);
        v.extend([0, 0, 0, 0]);
        v.extend(*b"IEND");
        v.extend([0xae, 0x42, 0x60, 0x82]);
        v
    }

    #[test]
    fn jpeg_is_converted_to_png_for_kitty() {
        let d = std::env::temp_dir().join(format!("nerd-jpeg-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let png = d.join("a.png");
        std::fs::write(&png, png_1x1()).unwrap();
        let jpg = d.join("a.jpg");
        let ok = std::process::Command::new("/usr/bin/sips")
            .args(["-s", "format", "jpeg"])
            .arg(&png)
            .arg("--out")
            .arg(&jpg)
            .output()
            .is_ok_and(|o| o.status.success());
        assert!(ok, "sips must produce a jpeg on macOS");
        let mut p = Preview::load(&jpg);
        match &p.kind {
            Kind::Image { format, dims, png, .. } => {
                assert_eq!(*format, ImageFormat::Jpeg, "the caption keeps the real format");
                assert_eq!(*dims, Some((1, 1)), "the size comes from the header");
                assert!(png.is_none(), "no conversion until Kitty wants one");
            }
            other => panic!("expected an image, got {:?}", std::mem::discriminant(other)),
        }
        assert_eq!(p.need(), Some(Need::Png(jpg.clone())));
        assert!(p.image_bytes(Rect::new(0, 0, 20, 10), 1).is_none(), "nothing to ship yet");
        let png = to_png(&jpg);
        assert!(png.as_deref().and_then(|b| png_size(b)).is_some(), "PNG after conversion: {:?}", convert_to_png(&jpg).err());
        p.give_png(png);
        assert_eq!(p.need(), None);
        assert!(p.image_bytes(Rect::new(0, 0, 20, 10), 1).is_some(), "the converted image is shipped to kitty");
        p.view = ImageView::Blocks { hint: BlockHint::None };
        assert_eq!(p.need(), Some(Need::Pixels(jpg.clone(), Some((1, 1)))), "blocks want pixels of their own");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A BMP laid out as sips writes it: 24 bpp or 32 bpp bitfields, `top_down` picks the height sign.
    fn bmp(w: usize, h: usize, bpp: usize, top_down: bool, px: impl Fn(usize, usize) -> [u8; 4]) -> Vec<u8> {
        let hdr = if bpp == 4 { 124 } else { 40 };
        let off = 14 + hdr;
        let stride = (w * bpp).div_ceil(4) * 4;
        let mut v = vec![0u8; off + stride * h];
        v[0..2].copy_from_slice(b"BM");
        v[10..14].copy_from_slice(&(off as u32).to_le_bytes());
        v[14..18].copy_from_slice(&(hdr as u32).to_le_bytes());
        v[18..22].copy_from_slice(&(w as i32).to_le_bytes());
        let sh = if top_down { -(h as i32) } else { h as i32 };
        v[22..26].copy_from_slice(&sh.to_le_bytes());
        v[28..30].copy_from_slice(&((bpp * 8) as u16).to_le_bytes());
        if bpp == 4 {
            v[30..34].copy_from_slice(&3u32.to_le_bytes());
            for (i, m) in [0x00ff_0000u32, 0x0000_ff00, 0x0000_00ff, 0xff00_0000].iter().enumerate() {
                v[54 + i * 4..58 + i * 4].copy_from_slice(&m.to_le_bytes());
            }
        }
        for y in 0..h {
            let row = if top_down { y } else { h - 1 - y };
            for x in 0..w {
                let [r, g, b, a] = px(x, y);
                let at = off + row * stride + x * bpp;
                v[at..at + 3].copy_from_slice(&[b, g, r]);
                if bpp == 4 {
                    v[at + 3] = a;
                }
            }
        }
        v
    }

    #[test]
    fn bmp_parser_handles_what_sips_writes() {
        let colour = |x: usize, y: usize| [x as u8 * 10, y as u8 * 20, 7, 200];
        for (bpp, top_down) in [(3, true), (3, false), (4, true), (4, false)] {
            // Width 3 at 24 bpp leaves a padding byte per row.
            let p = parse_bmp(&bmp(3, 2, bpp, top_down, colour)).expect("parses");
            assert_eq!((p.w, p.h), (3, 2));
            let at = |x: usize, y: usize| p.rgba[(y * 3 + x) * 4..][..4].to_vec();
            let alpha = if bpp == 4 { 200 } else { 255 };
            assert_eq!(at(2, 1), vec![20, 20, 7, alpha], "bpp {bpp}, top_down {top_down}");
            assert_eq!(at(0, 0), vec![0, 0, 7, alpha]);
        }

        let good = bmp(3, 2, 3, true, colour);
        assert!(parse_bmp(&good[..good.len() - 1]).is_none(), "truncated");
        let mut min = good.clone();
        min[22..26].copy_from_slice(&i32::MIN.to_le_bytes());
        assert!(parse_bmp(&min).is_none(), "i32::MIN height");
        let mut huge = good.clone();
        huge[18..22].copy_from_slice(&i32::MAX.to_le_bytes());
        assert!(parse_bmp(&huge).is_none(), "size past the data");
        let mut rle = good.clone();
        rle[30..34].copy_from_slice(&1u32.to_le_bytes());
        assert!(parse_bmp(&rle).is_none(), "compression other than sips writes");

        let mut seed = 42u64;
        for n in 0..500 {
            let mut junk = good.clone();
            for _ in 0..4 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                let i = (seed >> 33) as usize % junk.len();
                junk[i] = (seed >> 17) as u8;
            }
            junk.truncate(junk.len() - n % junk.len());
            let _ = parse_bmp(&junk);
        }
    }

    #[test]
    fn quadrants_split_a_cell_by_brightness() {
        let red = [255, 0, 0, 255];
        let blue = [0, 0, 255, 255];
        let white = [255, 255, 255, 255];
        let black = [0, 0, 0, 255];
        // One cell holds four samples: the source is 2x2, the image area one cell.
        let p = parse_bmp(&bmp(2, 2, 4, true, |x, y| match (x, y) {
            (0, 0) => red,
            (1, 0) => blue,
            (0, 1) => white,
            _ => black,
        }))
        .unwrap();
        let th = Theme::dark();
        let area = Rect::new(0, 0, 1, 2);
        let mut buf = Buffer::empty(area);
        draw_blocks(&mut buf, area, &p, "png", None, &th);

        let cell = &buf[(0, 0)];
        // Only white is above the mean brightness, and it sits bottom left.
        assert_eq!(cell.symbol(), "\u{2596}");
        assert_eq!(cell.fg, ratatui::style::Color::Rgb(255, 255, 255));
        assert_eq!(cell.bg, ratatui::style::Color::Rgb(85, 0, 85), "mean of red, blue and black");
        assert_eq!(buf[(0, 1)].symbol(), "p", "caption below the image");
    }

    #[test]
    fn every_quadrant_mask_draws_its_own_glyph() {
        // Black and white samples put exactly the chosen quadrants above the mean.
        let black = [0, 0, 0];
        let white = [255, 255, 255];
        let expect = [
            (0b0001, "\u{2598}"),
            (0b0010, "\u{259d}"),
            (0b0011, "\u{2580}"),
            (0b0100, "\u{2596}"),
            (0b0101, "\u{258c}"),
            (0b0110, "\u{259e}"),
            (0b0111, "\u{259b}"),
            (0b1000, "\u{2597}"),
            (0b1001, "\u{259a}"),
            (0b1010, "\u{2590}"),
            (0b1011, "\u{259c}"),
            (0b1100, "\u{2584}"),
            (0b1101, "\u{2599}"),
            (0b1110, "\u{259f}"),
        ];
        for (mask, glyph) in expect {
            let quads = std::array::from_fn(|i| if mask & (1 << i) != 0 { white } else { black });
            let (symbol, fg, bg) = cell_of(quads);
            assert_eq!(symbol, glyph, "mask {mask:04b}");
            assert_eq!(fg, white, "mask {mask:04b}: the lit quadrants are the foreground");
            assert_eq!(bg, black, "mask {mask:04b}");
        }
        // Four alike leave nothing to split; a full block would need all four above their own mean.
        assert_eq!(cell_of([white; 4]).0, " ");
    }

    #[test]
    fn a_flat_cell_keeps_its_colour_and_alpha_shows_the_ground() {
        let th = Theme::dark();
        let grey = [90, 90, 90, 255];
        let flat = parse_bmp(&bmp(2, 2, 4, true, |_, _| grey)).unwrap();
        let area = Rect::new(0, 0, 1, 2);
        let mut buf = Buffer::empty(area);
        draw_blocks(&mut buf, area, &flat, "png", None, &th);
        assert_eq!(buf[(0, 0)].symbol(), " ", "nothing to split: a blank cell in that colour");
        assert_eq!(buf[(0, 0)].bg, ratatui::style::Color::Rgb(90, 90, 90));

        let clear = parse_bmp(&bmp(2, 2, 4, true, |_, _| [0, 255, 0, 0])).unwrap();
        let mut buf = Buffer::empty(area);
        draw_blocks(&mut buf, area, &clear, "png", None, &th);
        assert_eq!(buf[(0, 0)].bg, th.ground, "transparent pixels show the ground");

        // A source smaller than the sample grid still has to reach the cells: an empty
        // sample box would silently paint the ground instead.
        for (w, h) in [(0, 0), (1, 1), (1, 9), (9, 1), (2, 3), (10, 4)] {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(Rect::new(0, 0, 10, 10));
            draw_blocks(&mut buf, area, &flat, "png, 2×2", Some("hint"), &th);
            let cells = (0..10).flat_map(|y| (0..10).map(move |x| (x, y)));
            let grey = ratatui::style::Color::Rgb(90, 90, 90);
            assert_eq!(
                cells.filter(|c| buf[*c].bg == grey).count() > 0,
                w > 0 && h > 0,
                "area {w}x{h}: the image reaches the cells whenever there is room"
            );
        }
    }

    /// Cells the image painted: a flat cell is a blank with a background colour.
    fn painted(buf: &Buffer) -> usize {
        let a = buf.area;
        (a.y..a.bottom())
            .flat_map(|y| (a.x..a.right()).map(move |x| (x, y)))
            .filter(|c| matches!(buf[*c].bg, ratatui::style::Color::Rgb(..)))
            .count()
    }

    fn screen_text(buf: &Buffer) -> String {
        let a = buf.area;
        (a.y..a.bottom()).flat_map(|y| (a.x..a.right()).map(move |x| (x, y))).map(|c| buf[c].symbol().to_string()).collect()
    }

    /// A PNG with an uncompressed zlib stream: enough to make sips see palette and gray+alpha images.
    fn png_of(w: u32, h: u32, color_type: u8, plte: Option<&[u8]>, row: &[u8]) -> Vec<u8> {
        fn crc(b: &[u8]) -> u32 {
            let mut c = !0u32;
            for &x in b {
                c ^= x as u32;
                for _ in 0..8 {
                    c = if c & 1 == 1 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
                }
            }
            !c
        }
        fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
            out.extend((data.len() as u32).to_be_bytes());
            let body = [kind, data].concat();
            out.extend(&body);
            out.extend(crc(&body).to_be_bytes());
        }
        let raw: Vec<u8> = (0..h).flat_map(|_| std::iter::once(0).chain(row.iter().copied())).collect();
        let (mut a, mut b) = (1u32, 0u32);
        for &x in &raw {
            a = (a + x as u32) % 65521;
            b = (b + a) % 65521;
        }
        let mut z = vec![0x78, 0x01, 0x01];
        z.extend((raw.len() as u16).to_le_bytes());
        z.extend((!(raw.len() as u16)).to_le_bytes());
        z.extend(&raw);
        z.extend(((b << 16) | a).to_be_bytes());

        let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = w.to_be_bytes().to_vec();
        ihdr.extend(h.to_be_bytes());
        ihdr.extend([8, color_type, 0, 0, 0]);
        chunk(&mut v, b"IHDR", &ihdr);
        if let Some(p) = plte {
            chunk(&mut v, b"PLTE", p);
        }
        chunk(&mut v, b"IDAT", &z);
        chunk(&mut v, b"IEND", &[]);
        v
    }

    #[test]
    fn palette_and_gray_alpha_pngs_decode_through_sips() {
        let d = tmp("png-types");
        let pal = d.join("pal.png");
        let row: Vec<u8> = (0..64).map(|x| (x / 16 % 3) as u8).collect();
        fs::write(&pal, png_of(64, 48, 3, Some(&[255, 0, 0, 0, 255, 0, 0, 0, 255]), &row)).unwrap();
        let ga = d.join("ga.png");
        fs::write(&ga, png_of(64, 48, 4, None, &[100, 128].repeat(64))).unwrap();

        for f in [pal, ga] {
            let png = fs::read(&f).unwrap();
            let px = to_pixels(&f, png_size(&png));
            let px = px.unwrap_or_else(|| panic!("{} decodes", f.display()));
            assert_eq!((px.w, px.h), (64, 48), "{}", f.display());
        }
        rm(&d);
    }

    #[test]
    fn a_jpeg_is_drawn_in_blocks_through_sips() {
        let d = tmp("blocks");
        let png = d.join("a.png");
        fs::write(&png, png_1x1()).unwrap();
        let jpg = d.join("a.jpg");
        let ok = std::process::Command::new("/usr/bin/sips")
            .args(["-s", "format", "jpeg", "-z", "600", "900"])
            .arg(&png)
            .arg("--out")
            .arg(&jpg)
            .output()
            .is_ok_and(|o| o.status.success());
        assert!(ok, "sips must produce a jpeg on macOS");

        let mut p = Preview::load(&jpg);
        p.view = ImageView::Blocks { hint: BlockHint::Terminal };
        let Some(Need::Pixels(path, dims)) = p.need() else { panic!("blocks want pixels") };
        assert_eq!(dims, Some((900, 600)), "read from the header, before any conversion");
        p.give_pixels(to_pixels(&path, dims));
        let area = Rect::new(0, 0, 60, 20);
        let mut buf = Buffer::empty(area);
        p.draw(&mut buf, area, 0, 0, false, &Theme::dark());
        match &p.kind {
            Kind::Image { pixels: Some(Some(px)), .. } => assert_eq!((px.w, px.h), (800, 533), "a large source is capped"),
            _ => panic!("pixels decoded"),
        }
        let text = screen_text(&buf);
        assert!(painted(&buf) > 100, "the image is painted into the cells: {}", painted(&buf));
        assert!(text.contains("low resolution") && text.contains("WezTerm show full quality"), "hint shown on two lines");

        p.view = ImageView::Blocks { hint: BlockHint::Tmux };
        let mut buf = Buffer::empty(area);
        p.draw(&mut buf, area, 0, 0, false, &Theme::dark());
        let text = screen_text(&buf);
        assert!(text.contains("tmux carries no images") && text.contains("outside tmux"), "the tmux hint replaces the terminal one: {text}");

        p.view = ImageView::Off;
        let mut buf = Buffer::empty(area);
        p.draw(&mut buf, area, 0, 0, false, &Theme::dark());
        let text = screen_text(&buf);
        assert_eq!(painted(&buf), 0, "off: no image cells");
        assert!(text.contains("jpeg, 900×600"), "off: caption only: {text}");
        rm(&d);
    }

    #[test]
    fn the_image_and_its_caption_share_one_layout() {
        let area = Rect::new(0, 0, 40, 21);
        // 200x100 pixels is 200x50 cells: a fifth fits across, so ten rows of twenty are used.
        let (at, caption_y) = image_layout(200, 100, area, 1);
        assert_eq!((at.width, at.height), (40, 10));
        assert_eq!(caption_y, at.bottom(), "the caption sits straight under the image");
        assert_eq!(at.y, 5, "centred down, with the caption's row kept free: {at:?}");
        assert!(caption_y < area.bottom(), "the caption is inside the panel");

        // A tall image is centred the other way.
        let (tall, _) = image_layout(50, 400, area, 1);
        // The odd cell goes to one side; a cell off centre is centred enough.
        assert!(tall.x.abs_diff(area.width - tall.right()) <= 1, "centred across: {tall:?}");

        // Two lines of caption leave one row less for the image.
        let (two, _) = image_layout(200, 100, area, 2);
        assert!(two.bottom() <= at.bottom());

        // Nothing to draw into: no panic, nothing off screen.
        for (w, h) in [(0, 0), (1, 1), (3, 1)] {
            let (at, y) = image_layout(100, 100, Rect::new(0, 0, w, h), 1);
            assert!(at.right() <= w.max(1) && y <= h.max(1), "{at:?} {y}");
        }
    }

    #[test]
    fn png_header_gives_the_pixel_size() {
        assert_eq!(png_size(&png_1x1()), Some((1, 1)));
        assert_eq!(png_size(b"\x89PNG\r\n\x1a\n"), None, "too short");
        assert_eq!(png_size(&[0; 64]), None, "not a png");

        // Width and height are read as big-endian from the IHDR.
        let mut big = png_1x1();
        big[16..20].copy_from_slice(&600u32.to_be_bytes());
        big[20..24].copy_from_slice(&400u32.to_be_bytes());
        assert_eq!(png_size(&big), Some((600, 400)));

        // A cell is twice as tall as it is wide: 600x400 is 600x200 cells,
        // which fits in 60x30 with a factor of 0.1.
        assert_eq!(fit(600, 400, Rect::new(0, 0, 60, 30)), (60, 20));
        assert_eq!(fit(1, 1, Rect::new(0, 0, 60, 30)), (1, 1), "tiny stays tiny, not upscaled");
    }

    #[test]
    fn image_bytes_speak_the_kitty_protocol() {
        let d = tmp("png");
        let f = d.join("a.png");
        // 20 KiB of tail data, so the base64 doesn't fit in one chunk.
        let mut bytes = png_1x1();
        bytes[16..20].copy_from_slice(&64u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&64u32.to_be_bytes());
        bytes.extend(std::iter::repeat_n(b'\x00', 20 * 1024));
        fs::write(&f, &bytes).unwrap();

        let p = Preview::load(&f);
        assert!(matches!(p.kind, Kind::Image { format: ImageFormat::Png, .. }));
        let out = p.image_bytes(Rect::new(4, 2, 40, 20), 7).expect("png returns bytes");
        let s = String::from_utf8(out).expect("protocol is ascii");

        assert!(s.starts_with("\x1b_Ga=d,d=i,i=7\x1b\\"), "clear the previous one first: {:?}", &s[..24]);
        // 64x64 pixels is 64x32 cells; one row of the area goes to the caption, leaving 40x19,
        // so it lands 38x19 and is centred across: x 4+1, y 2+0, counted from one.
        assert!(s.contains("\x1b[3;6H"), "cursor to the image's own corner, one-based numbering");
        assert!(s.contains("a=T"), "transmit and display");
        assert!(s.contains("f=100"), "png sent as-is");
        assert!(s.contains("q=2"), "terminal replies are muted");
        assert!(s.contains("i=7,c=38,r=19"), "size in cells, with the caption's row kept free");
        assert!(s.ends_with("\x1b\\"), "last chunk is closed");

        let chunks: Vec<&str> = s.split("\x1b_G").skip(2).collect();
        assert!(chunks.len() > 1, "20 KiB should split into chunks");
        for c in &chunks {
            let payload = c.split_once(';').expect("has a semicolon").1;
            let payload = payload.trim_end_matches('\\').trim_end_matches('\x1b');
            assert!(payload.len() <= CHUNK, "chunk {} > {CHUNK}", payload.len());
        }
        let m: Vec<bool> = chunks.iter().map(|c| c.contains("m=1")).collect();
        assert!(m[..m.len() - 1].iter().all(|x| *x), "all but the last have m=1");
        assert!(chunks.last().expect("chunks exist").contains("m=0"), "last one has m=0");

        let j = d.join("a.jpg");
        fs::write(&j, [0xffu8, 0xd8, 0xff, 0xe0, 1, 2, 3]).unwrap();
        let jp = Preview::load(&j);
        assert!(matches!(jp.kind, Kind::Image { format: ImageFormat::Jpeg, .. }));
        assert!(jp.image_bytes(Rect::new(0, 0, 10, 10), 7).is_none());
        assert!(p.image_bytes(Rect::new(0, 0, 0, 10), 7).is_none(), "empty area - nothing to draw");
        rm(&d);
    }

    #[test]
    fn the_size_comes_from_the_header_of_every_format() {
        let mut gif = b"GIF89a".to_vec();
        gif.extend([0x20, 0x03, 0x58, 0x02]);
        assert_eq!(image_size(&gif, ImageFormat::Gif), Some((800, 600)));
        let riff = |chunk: &[u8], body: &[u8]| [b"RIFF\0\0\0\0WEBP".as_slice(), chunk, &[0; 4], body].concat();
        let vp8 = riff(b"VP8 ", &[0, 0, 0, 0x9d, 0x01, 0x2a, 0x20, 0x03, 0x58, 0x02]);
        assert_eq!(image_size(&vp8, ImageFormat::Webp), Some((800, 600)));
        // 14 bits each, minus one: 799 and 599.
        let bits: u32 = 799 | (599 << 14);
        let vp8l = riff(b"VP8L", &[[0x2f].as_slice(), &bits.to_le_bytes()].concat());
        assert_eq!(image_size(&vp8l, ImageFormat::Webp), Some((800, 600)));
        let vp8x = riff(b"VP8X", &[0, 0, 0, 0, 0x1f, 0x03, 0, 0x57, 0x02, 0]);
        assert_eq!(image_size(&vp8x, ImageFormat::Webp), Some((800, 600)));
        // An APP1 with a thumbnail comes first; the frame after it has the size.
        let mut jpg = vec![0xff, 0xd8, 0xff, 0xe1, 0, 8, 0xff, 0xc0, 0, 0, 0, 0];
        jpg.extend([0xff, 0xc2, 0, 17, 8, 0x02, 0x58, 0x03, 0x20]);
        assert_eq!(image_size(&jpg, ImageFormat::Jpeg), Some((800, 600)));
        assert_eq!(image_size(&jpg[..10], ImageFormat::Jpeg), None, "cut short");
    }

    #[test]
    fn image_placeholder_says_format_and_size() {
        let d = tmp("holder");
        let f = d.join("a.webp");
        let mut bytes = Vec::from(*b"RIFF\0\0\0\0WEBP");
        bytes.extend(std::iter::repeat_n(b'\x01', 3000));
        fs::write(&f, &bytes).unwrap();

        let mut p = Preview::load(&f);
        // Kitty asked for it, and the converter could not read this one.
        p.give_png(to_png(&f));
        let area = Rect::new(0, 0, 44, 7);
        let mut buf = Buffer::empty(area);
        p.draw(&mut buf, area, 0, 0, false, &Theme::dark());
        let all: String = (0..area.height)
            .flat_map(|y| (0..area.width).map(move |x| (x, y)))
            .map(|k| buf[k].symbol())
            .collect();
        assert!(all.contains("webp, 2 KiB"), "format and size: {all}");
        assert!(all.contains("could not convert"), "why there are no pixels: {all}");
        assert!(all.contains('┌'), "placeholder border");

        let g = d.join("a.png");
        let mut png = png_1x1();
        png[16..20].copy_from_slice(&800u32.to_be_bytes());
        png[20..24].copy_from_slice(&600u32.to_be_bytes());
        fs::write(&g, &png).unwrap();
        let p = Preview::load(&g);
        let mut buf = Buffer::empty(area);
        p.draw(&mut buf, area, 0, 0, false, &Theme::dark());
        let all: String = (0..area.height)
            .flat_map(|y| (0..area.width).map(move |x| (x, y)))
            .map(|k| buf[k].symbol())
            .collect();
        let want = format!("png, 800×600, {} B", png.len());
        assert!(all.contains(&want), "expected \"{want}\": {all}");
        assert!(!all.contains("png only"), "png actually renders");
        rm(&d);
    }

    #[test]
    fn oversized_image_is_only_a_note() {
        let d = tmp("huge");
        let f = d.join("a.png");
        let mut bytes = png_1x1();
        bytes.extend(std::iter::repeat_n(b'\x00', 5 * 1024 * 1024));
        fs::write(&f, &bytes).unwrap();
        match Preview::load(&f).kind {
            Kind::Note(s) => assert!(s.contains("more than 4 MiB"), "{s}"),
            _ => panic!("we don't read a huge image whole"),
        }
        rm(&d);
    }

    #[test]
    fn clear_sequence_names_the_image() {
        assert_eq!(clear_image_bytes(3), b"\x1b_Ga=d,d=i,i=3\x1b\\".to_vec());
    }

    #[test]
    fn base64_matches_the_known_answers() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn garbage_bytes_never_panic() {
        // Homegrown PRNG instead of a dependency, and deterministic run to run.
        let mut x = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let langs = [
            Lang::Rust, Lang::Js, Lang::Ts, Lang::Python, Lang::Shell,
            Lang::Json, Lang::Toml, Lang::Yaml, Lang::Markdown, Lang::Plain,
        ];
        let d = tmp("fuzz");
        let th = Theme::dark();

        for round in 0..200 {
            let n = (next() % 300) as usize;
            let bytes: Vec<u8> = (0..n).map(|_| (next() % 256) as u8).collect();
            // A NUL byte would make it Binary instead, skipping the text-parsing path under test.
            let bytes: Vec<u8> = bytes.into_iter().map(|b| if b == 0 { b'~' } else { b }).collect();
            let f = d.join(format!("f{round}.rs"));
            fs::write(&f, &bytes).unwrap();

            let s = String::from_utf8_lossy(&bytes);
            for lang in langs {
                let mut c = Carry::default();
                for line in s.split('\n') {
                    let toks = scan(line, lang, &mut c);
                    let joined: String = toks.iter().map(|(t, _)| *t).collect();
                    assert_eq!(joined, line, "chunks join back into the line, {lang:?}");
                }
            }

            let mut p = Preview::load(&f);
            p.goto((next() % 20) as u32);
            let area = Rect::new(0, 0, (next() % 40) as u16 + 1, (next() % 20) as u16 + 1);
            let mut buf = Buffer::empty(Rect::new(0, 0, 40, 20));
            p.draw(&mut buf, area, (next() % 50) as usize, (next() % 30) as usize, next() % 2 == 0, &th);
        }

        let f = d.join("bad.png");
        let mut bad = Vec::from(*b"\x89PNG\r\n\x1a\n");
        bad.extend((0..40).map(|_| (next() % 256) as u8));
        fs::write(&f, &bad).unwrap();
        let p = Preview::load(&f);
        let _ = p.image_bytes(Rect::new(0, 0, 10, 10), 1);
        rm(&d);
    }
}
