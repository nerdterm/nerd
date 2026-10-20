//! Terminal presets: Option as Alt on the left key, a 16 pt margin and nerd's colours, for Ghostty, kitty, WezTerm
//! and iTerm2. nerd writes files of its own and adds one marked block to the terminal's config; undo takes out exactly
//! the bytes it added. Every change is a `Step`, shown before it is made.

use std::path::{Path, PathBuf};

use ratatui::style::Color;

use crate::render::Theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Terminal {
    Ghostty,
    Kitty,
    Wezterm,
    Iterm2,
}

pub const ALL: [Terminal; 4] = [Terminal::Ghostty, Terminal::Kitty, Terminal::Wezterm, Terminal::Iterm2];

impl Terminal {
    pub fn name(self) -> &'static str {
        match self {
            Terminal::Ghostty => "ghostty",
            Terminal::Kitty => "kitty",
            Terminal::Wezterm => "wezterm",
            Terminal::Iterm2 => "iterm2",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Terminal::Ghostty => "Ghostty",
            Terminal::Kitty => "kitty",
            Terminal::Wezterm => "WezTerm",
            Terminal::Iterm2 => "iTerm2",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        ALL.into_iter().find(|t| t.name() == s.to_ascii_lowercase())
    }
}

/// One change, shown before it is made and made exactly as shown.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// A file of nerd's own, written whole.
    Write(PathBuf, String),
    /// A marked block added at the end of the terminal's config, which is created when it is not there.
    Append(PathBuf, String),
    /// That block taken out again; a file nerd made goes when nothing else is left in it.
    Unappend(PathBuf, String),
    /// nerd's one marked line, put in a WezTerm config right before the `return` that ends it.
    Insert(PathBuf, String),
    /// That line taken out again, its bytes only.
    Uninsert(PathBuf, String),
    /// A line for the human to add to a WezTerm config nerd does not edit: shown whole, never written.
    Line(PathBuf, String),
    /// A file of nerd's own, removed.
    Remove(PathBuf),
    /// iTerm2's margins, which are app-wide: set to 16 with the values before kept, or those put back.
    Margins(bool),
    /// For the human to do; printed only.
    Note(String),
}

const MARGIN: u16 = 16;

fn nerd_dir() -> PathBuf {
    crate::paths::config_path().parent().map(Path::to_path_buf).unwrap_or_default()
}

fn home() -> PathBuf {
    crate::config::home_dir()
}

#[cfg(test)]
thread_local! {
    static TEST_ENV: std::cell::RefCell<Vec<(&'static str, PathBuf)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// An absolute path from the environment; tests never read the real one, only what `set_env` gave this thread.
fn env_path(key: &str) -> Option<PathBuf> {
    #[cfg(test)]
    return TEST_ENV.with(|e| e.borrow().iter().find(|(k, _)| *k == key).map(|(_, p)| p.clone()));
    #[cfg(not(test))]
    std::env::var_os(key).map(PathBuf::from).filter(|p| p.is_absolute())
}

#[cfg(test)]
fn set_env(key: &'static str, path: PathBuf) {
    TEST_ENV.with(|e| e.borrow_mut().push((key, path)));
}

fn xdg() -> PathBuf {
    env_path("XDG_CONFIG_HOME").unwrap_or_else(|| home().join(".config"))
}

fn own(file: &str) -> PathBuf {
    nerd_dir().join(file)
}

/// The block's first line, the same whether nerd made the file or not.
fn begin(t: Terminal) -> String {
    format!("{} >>> nerd preset for {}", comment(t), t.title())
}

fn comment(t: Terminal) -> &'static str {
    if t == Terminal::Wezterm { "--" } else { "#" }
}

fn block(t: Terminal, body: &str, made: bool) -> String {
    let made = if made { ", in a file nerd made" } else { "" };
    format!("{}{made}: `nerd --preset {} --undo` takes it out\n{body}\n{} <<< nerd preset for {}\n", begin(t), t.name(), comment(t), t.title())
}

fn read(path: &Path) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn has_block(t: Terminal, path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|s| s.contains(&begin(t)))
}

fn hex(c: Color) -> String {
    match c {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        _ => "#000000".into(),
    }
}

/// nerd's colours in each appearance; a fixed theme gives the same one twice.
fn themes(cfg: &crate::config::Config) -> (Theme, Theme) {
    (crate::render::pick_theme(cfg, Some(false)).0, crate::render::pick_theme(cfg, Some(true)).0)
}

// --- Ghostty ---

/// Load order (spike, #79): XDG `config`, XDG `config.ghostty`, then the same two in Application Support.
fn ghostty_configs() -> Vec<PathBuf> {
    let support = home().join("Library/Application Support/com.mitchellh.ghostty");
    let x = xdg().join("ghostty");
    vec![x.join("config"), x.join("config.ghostty"), support.join("config"), support.join("config.ghostty")]
}

/// The key file and the two lines `nerd --install-keys` used to add to `~/.config/ghostty/config`.
fn legacy() -> (PathBuf, PathBuf, String) {
    let keys = own("ghostty-keys.conf");
    let lines = format!("{}\nconfig-file = {}\n", crate::ghostty::MARK, keys.display());
    (home().join(".config/ghostty/config"), keys, lines)
}

fn legacy_linked() -> bool {
    let (old, _, lines) = legacy();
    std::fs::read_to_string(old).is_ok_and(|s| s.contains(&lines))
}

fn ghostty_theme(t: &Theme) -> String {
    let mut s = format!("background = {}\nforeground = {}\n", hex(t.ground), hex(t.text));
    for (i, c) in t.ansi.iter().enumerate() {
        s.push_str(&format!("palette = {i}={}\n", hex(*c)));
    }
    s
}

fn ghostty(cfg: &crate::config::Config) -> Vec<Step> {
    let (light, dark) = themes(cfg);
    let (lp, dp, conf) = (own("ghostty-light.conf"), own("ghostty-dark.conf"), own("ghostty-preset.conf"));
    let text = format!(
        "# Written by nerd for `nerd --preset ghostty`; rewritten when nerd starts.\n\
         macos-option-as-alt = left\n\
         window-padding-x = {MARGIN}\nwindow-padding-y = {MARGIN}\nwindow-padding-balance = true\n\
         theme = light:{},dark:{}\n{}",
        lp.display(),
        dp.display(),
        crate::ghostty::conf()
    );
    vec![Step::Write(lp, ghostty_theme(&light)), Step::Write(dp, ghostty_theme(&dark)), Step::Write(conf, text)]
}

// --- kitty ---

fn kitty_conf() -> PathBuf {
    env_path("KITTY_CONFIG_DIRECTORY").unwrap_or_else(|| xdg().join("kitty")).join("kitty.conf")
}

/// ceiling: one appearance; kitty switches by appearance only through its own `*-theme.auto.conf` files.
fn kitty(cfg: &crate::config::Config, dark: bool) -> Vec<Step> {
    let (light, night) = themes(cfg);
    let t = if dark { night } else { light };
    let mut s = format!(
        "# Written by nerd for `nerd --preset kitty`; rewritten when nerd starts, in the colours of the {} appearance.\n\
         macos_option_as_alt left\nwindow_padding_width {MARGIN}\nbackground {}\nforeground {}\n",
        if dark { "dark" } else { "light" },
        hex(t.ground),
        hex(t.text)
    );
    for (i, c) in t.ansi.iter().enumerate() {
        s.push_str(&format!("color{i} {}\n", hex(*c)));
    }
    vec![Step::Write(own("kitty-preset.conf"), s)]
}

// --- WezTerm ---

/// WezTerm's own order: the first that exists is the one it reads.
fn wezterm_configs() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = env_path("WEZTERM_CONFIG_FILE").into_iter().collect();
    for p in [xdg().join("wezterm/wezterm.lua"), home().join(".config/wezterm/wezterm.lua"), home().join(".wezterm.lua")] {
        if !v.contains(&p) {
            v.push(p);
        }
    }
    v
}

/// `dofile` on an absolute path, not `require`: WezTerm's `package.path` follows the XDG_CONFIG_HOME it started with.
fn wezterm_load() -> String {
    format!("local ok, nerd = pcall(dofile, '{}')", own("wezterm-preset.lua").display())
}

fn lua_colors(t: &Theme) -> String {
    let list = |cs: &[Color]| cs.iter().map(|c| format!("'{}'", hex(*c))).collect::<Vec<_>>().join(", ");
    format!("{{ background = '{}', foreground = '{}', ansi = {{ {} }}, brights = {{ {} }} }}", hex(t.ground), hex(t.text), list(&t.ansi[..8]), list(&t.ansi[8..]))
}

fn wezterm(cfg: &crate::config::Config) -> Vec<Step> {
    let (light, dark) = themes(cfg);
    let text = format!(
        "-- Written by nerd for `nerd --preset wezterm`; rewritten when nerd starts.\n\
         local wezterm = require 'wezterm'\n\
         local light = {}\n\
         local dark = {}\n\
         return function(config)\n  \
           config.send_composed_key_when_left_alt_is_pressed = false\n  \
           config.send_composed_key_when_right_alt_is_pressed = true\n  \
           config.window_padding = {{ left = '{MARGIN}pt', right = '{MARGIN}pt', top = '{MARGIN}pt', bottom = '{MARGIN}pt' }}\n  \
           local appearance = wezterm.gui and wezterm.gui.get_appearance() or 'Dark'\n  \
           config.colors = appearance:find('Light') and light or dark\n  \
           return config\n\
         end\n",
        lua_colors(&light),
        lua_colors(&dark)
    );
    vec![Step::Write(own("wezterm-preset.lua"), text)]
}

/// Marks nerd's line in a config of the user's, at its end.
const WEZTERM_MARK: &str = "-- nerd preset for WezTerm: `nerd --preset wezterm --undo` takes it out";

/// The line that hands the table `name` to nerd's file.
fn wezterm_call(name: &str) -> String {
    format!("{} if ok then nerd({name}) end", wezterm_load())
}

/// Where nerd's line goes in a config that ends with `return <name>`, `name` a `local` the file builds with
/// `wezterm.config_builder()` or `{`: the start of that return line, and the marked line with the file's line ending.
fn wezterm_return(s: &str) -> Option<(usize, String)> {
    let body = s.trim_end();
    let at = body.rfind('\n').map_or(0, |i| i + 1);
    let name = body[at..].trim().strip_prefix("return ")?.trim();
    if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let built = s.lines().any(|l| {
        let value = l.trim().strip_prefix("local ").and_then(|l| l.trim_start().strip_prefix(name)).and_then(|l| l.trim_start().strip_prefix('='));
        value.map(str::trim_start).is_some_and(|v| v.starts_with('{') || v.starts_with("wezterm.config_builder()"))
    });
    let eol = if s.contains("\r\n") { "\r\n" } else { "\n" };
    built.then(|| (at, format!("{} {WEZTERM_MARK}{eol}", wezterm_call(name))))
}

/// nerd's line as it sits in `s`, line ending included, rebuilt from the name it calls, so a hand edit does not match.
fn wezterm_inserted(s: &str) -> Option<String> {
    let line = s.split_inclusive('\n').find(|l| l.contains(WEZTERM_MARK))?;
    let name = line.split("if ok then nerd(").nth(1)?.split(')').next()?;
    let eol = if line.ends_with("\r\n") { "\r\n" } else { "\n" };
    Some(format!("{} {WEZTERM_MARK}{eol}", wezterm_call(name)))
}

/// What a person reads about the line that a config nerd does not edit needs.
fn wezterm_needs(path: &Path) -> String {
    format!(
        "WezTerm needs one line from you: nerd edits {} only when it ends with `return <table>` of a table built in it. \
         Put this line right before the line that returns your config table, with that table's name in place of config. \
         A file that is `return {{ ... }}`: make it `local config = {{ ... }}` and `return config`, then apply again, and nerd adds the line itself.",
        crate::paths::shown(path)
    )
}

/// The line a person must add, when the steps carry one.
pub fn needs_line(steps: &[Step]) -> Option<(&Path, String)> {
    steps.iter().find_map(|s| match s {
        Step::Line(p, line) => Some((p.as_path(), line.clone())),
        _ => None,
    })
}

// --- iTerm2 ---

fn iterm_profile() -> PathBuf {
    home().join("Library/Application Support/iTerm2/DynamicProfiles/nerd.json")
}

fn iterm(cfg: &crate::config::Config) -> Vec<Step> {
    let (light, dark) = themes(cfg);
    let color = |c: Color| {
        let Color::Rgb(r, g, b) = c else { return serde_json::Value::Null };
        let f = |v: u8| serde_json::Value::from(f64::from(v) / 255.0);
        serde_json::json!({ "Red Component": f(r), "Green Component": f(g), "Blue Component": f(b), "Alpha Component": 1, "Color Space": "sRGB" })
    };
    let mut p = serde_json::Map::new();
    p.insert("Name".into(), "nerd".into());
    p.insert("Guid".into(), "nerd-preset".into());
    p.insert("Option Key Sends".into(), 2.into());
    p.insert("Right Option Key Sends".into(), 0.into());
    p.insert("Use Separate Colors for Light and Dark Mode".into(), true.into());
    for (mode, t) in [("Light", &light), ("Dark", &dark)] {
        p.insert(format!("Background Color ({mode})"), color(t.ground));
        p.insert(format!("Foreground Color ({mode})"), color(t.text));
        for (i, c) in t.ansi.iter().enumerate() {
            p.insert(format!("Ansi {i} Color ({mode})"), color(*c));
        }
    }
    let text = serde_json::to_string_pretty(&serde_json::json!({ "Profiles": [p] })).unwrap_or_default() + "\n";
    vec![Step::Write(iterm_profile(), text)]
}

const ITERM_KEYS: [&str; 2] = ["TerminalMargin", "TerminalVMargin"];

fn margins_record() -> PathBuf {
    own("iterm2-margins.json")
}

/// The account's own home, from the user database: iTerm2's real settings are written only under it.
fn account_home() -> Option<PathBuf> {
    // SAFETY: a zeroed passwd is a valid value; getpwuid_r writes into it and `buf` only, and `out` points at `pw` or is null.
    let mut pw: nix::libc::passwd = unsafe { std::mem::zeroed() };
    let mut buf = vec![0 as nix::libc::c_char; 4096];
    let mut out: *mut nix::libc::passwd = std::ptr::null_mut();
    let r = unsafe { nix::libc::getpwuid_r(nix::libc::getuid(), &mut pw, buf.as_mut_ptr(), buf.len(), &mut out) };
    if r != 0 || out.is_null() {
        return None;
    }
    let dir = unsafe { std::ffi::CStr::from_ptr(pw.pw_dir) };
    Some(PathBuf::from(<std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(dir.to_bytes())))
}

/// iTerm2 reads the account's settings whatever HOME says, so they are changed only when HOME is the account's.
fn own_account() -> bool {
    cfg!(test) || account_home().is_some_and(|h| h == home())
}

/// Tests keep the two values in a file of the test home and never run `defaults`.
fn test_prefs() -> PathBuf {
    home().join("iterm2-prefs.json")
}

fn margin_now(key: &str) -> Option<i64> {
    if cfg!(test) {
        let all: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(test_prefs()).ok()?).ok()?;
        return all.get(key)?.as_i64();
    }
    let out = crate::spawn::cmd("/usr/bin/defaults").args(["read", "com.googlecode.iterm2", key]).stderr(std::process::Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().parse().ok()).flatten()
}

/// `None` takes the key out.
fn margin_set(key: &str, v: Option<i64>) -> std::io::Result<()> {
    if cfg!(test) {
        let mut all: serde_json::Map<String, serde_json::Value> = std::fs::read_to_string(test_prefs()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        match v {
            Some(v) => all.insert(key.into(), v.into()),
            None => all.remove(key),
        };
        return std::fs::write(test_prefs(), serde_json::to_string(&all).map_err(std::io::Error::other)?);
    }
    let v = v.map(|v| v.to_string());
    let args: Vec<&str> = match &v {
        Some(v) => vec!["write", "com.googlecode.iterm2", key, "-int", v],
        None => vec!["delete", "com.googlecode.iterm2", key],
    };
    let out = crate::spawn::cmd("/usr/bin/defaults").args(&args).stderr(std::process::Stdio::piped()).output()?;
    // taking out a key that was never there fails, and that is the state wanted
    if out.status.success() || v.is_none() { Ok(()) } else { Err(std::io::Error::other(format!("defaults: {}", String::from_utf8_lossy(&out.stderr).trim()))) }
}

fn set_margins(on: bool) -> std::io::Result<()> {
    let record = margins_record();
    if on {
        // the values before the first apply; a second apply keeps them
        if !record.exists() {
            let before: serde_json::Map<String, serde_json::Value> = ITERM_KEYS.iter().map(|k| (k.to_string(), margin_now(k).into())).collect();
            std::fs::create_dir_all(nerd_dir())?;
            std::fs::write(&record, serde_json::to_string(&before).map_err(std::io::Error::other)?)?;
        }
        for k in ITERM_KEYS {
            margin_set(k, Some(i64::from(MARGIN)))?;
        }
        return Ok(());
    }
    let Some(text) = read(&record)? else { return Ok(()) };
    let before: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&text).map_err(std::io::Error::other)?;
    for k in ITERM_KEYS {
        margin_set(k, before.get(k).and_then(serde_json::Value::as_i64))?;
    }
    std::fs::remove_file(record)
}

// --- the plans ---

/// Whether the terminal's config carries nerd's preset.
pub fn applied(t: Terminal) -> bool {
    match t {
        Terminal::Ghostty => ghostty_configs().iter().any(|p| has_block(t, p)),
        Terminal::Kitty => has_block(t, &kitty_conf()),
        // nerd's file is there and the config WezTerm reads holds the line that loads it
        Terminal::Wezterm => {
            own("wezterm-preset.lua").exists()
                && wezterm_configs().iter().find(|p| p.exists()).and_then(|p| std::fs::read_to_string(p).ok()).is_some_and(|s| s.contains(&wezterm_load()))
        }
        Terminal::Iterm2 => iterm_profile().exists(),
    }
}

/// Applied or not; for Ghostty also the link `--install-keys` made, which applying replaces; for WezTerm also
/// nerd's file written while the user's config still lacks the line.
pub fn status(t: Terminal) -> &'static str {
    if applied(t) {
        "applied"
    } else if t == Terminal::Ghostty && legacy_linked() {
        "old keys link"
    } else if t == Terminal::Wezterm && own("wezterm-preset.lua").exists() {
        "needs your line"
    } else {
        "not applied"
    }
}

/// What `nerd --preset <t>` does, in order. `dark`: the appearance now, for kitty, which keeps one.
pub fn apply_steps(t: Terminal, cfg: &crate::config::Config, dark: bool) -> std::io::Result<Vec<Step>> {
    let mut steps = match t {
        Terminal::Ghostty => ghostty(cfg),
        Terminal::Kitty => kitty(cfg, dark),
        Terminal::Wezterm => wezterm(cfg),
        Terminal::Iterm2 => iterm(cfg),
    };
    match t {
        Terminal::Ghostty => {
            let (old, keys, lines) = legacy();
            if read(&old)?.is_some_and(|s| s.contains(&lines)) {
                steps.push(Step::Unappend(old, lines));
            }
            if keys.exists() {
                steps.push(Step::Remove(keys));
            }
            let configs = ghostty_configs();
            if !configs.iter().any(|p| has_block(t, p)) {
                let last = configs.iter().rev().find(|p| p.exists());
                let target = last.cloned().unwrap_or_else(|| configs[1].clone());
                let body = format!("config-file = ?{}", own("ghostty-preset.conf").display());
                steps.push(Step::Append(target.clone(), block(t, &body, last.is_none())));
            }
            steps.push(Step::Note("Ghostty reads it on its next start, or when it reloads its config. The preset's lines win over the same settings in your config.".into()));
        }
        Terminal::Kitty => {
            let conf = kitty_conf();
            if !has_block(t, &conf) {
                let made = read(&conf)?.is_none();
                steps.push(Step::Append(conf, block(t, &format!("include {}", own("kitty-preset.conf").display()), made)));
            }
            steps.push(Step::Note("kitty reads it on its next start. Lines after the block in kitty.conf win over the preset's.".into()));
        }
        Terminal::Wezterm => {
            let configs = wezterm_configs();
            match configs.iter().find(|p| p.exists()) {
                None => {
                    let body = format!("local config = {{}}\n{} if ok then nerd(config) end\nreturn config", wezterm_load());
                    steps.push(Step::Append(home().join(".wezterm.lua"), block(t, &body, true)));
                }
                Some(p) if applied(t) => steps.push(Step::Note(format!("{} already loads the preset.", crate::paths::shown(p)))),
                Some(p) => match wezterm_return(&read(p)?.unwrap_or_default()) {
                    Some((_, line)) => steps.push(Step::Insert(p.clone(), line)),
                    None => steps.push(Step::Line(p.clone(), wezterm_call("config"))),
                },
            }
            steps.push(Step::Note("WezTerm reloads its config by itself.".into()));
        }
        Terminal::Iterm2 => {
            if own_account() {
                steps.push(Step::Margins(true));
            } else {
                steps.push(Step::Note("iTerm2's margins are left as they are: HOME is not this account's home, and iTerm2 reads the account's settings.".into()));
            }
            steps.push(Step::Note(
                "The preset is a profile named nerd. To use it everywhere: iTerm2 → Settings → Profiles → nerd → Other Actions → Set as Default. iTerm2 takes the margins on its next start."
                    .into(),
            ));
        }
    }
    Ok(steps)
}

/// What `nerd --preset <t> --undo` does: every block taken out, every file of nerd's removed.
pub fn undo_steps(t: Terminal) -> std::io::Result<Vec<Step>> {
    let mut steps = Vec::new();
    let unappend = |steps: &mut Vec<Step>, path: &Path, body: &str| -> std::io::Result<()> {
        if read(path)?.is_some_and(|s| s.contains(&begin(t))) {
            steps.push(Step::Unappend(path.to_path_buf(), block(t, body, false)));
        }
        Ok(())
    };
    let remove = |steps: &mut Vec<Step>, files: &[&str]| {
        steps.extend(files.iter().map(|f| own(f)).filter(|p| p.exists()).map(Step::Remove));
    };
    match t {
        Terminal::Ghostty => {
            let body = format!("config-file = ?{}", own("ghostty-preset.conf").display());
            for p in ghostty_configs() {
                unappend(&mut steps, &p, &body)?;
            }
            let (old, _, lines) = legacy();
            if read(&old)?.is_some_and(|s| s.contains(&lines)) {
                steps.push(Step::Unappend(old, lines));
            }
            remove(&mut steps, &["ghostty-preset.conf", "ghostty-light.conf", "ghostty-dark.conf", "ghostty-keys.conf"]);
        }
        Terminal::Kitty => {
            unappend(&mut steps, &kitty_conf(), &format!("include {}", own("kitty-preset.conf").display()))?;
            remove(&mut steps, &["kitty-preset.conf"]);
        }
        Terminal::Wezterm => {
            let body = format!("local config = {{}}\n{} if ok then nerd(config) end\nreturn config", wezterm_load());
            let path = own("wezterm-preset.lua").display().to_string();
            for p in wezterm_configs() {
                match read(&p)? {
                    Some(s) if s.contains(&begin(t)) => unappend(&mut steps, &p, &body)?,
                    Some(s) if s.contains(WEZTERM_MARK) => match wezterm_inserted(&s) {
                        Some(line) if s.contains(&line) => steps.push(Step::Uninsert(p.clone(), line)),
                        _ => return Err(std::io::Error::other(format!("{}: nerd's line was changed by hand, so it is left as it is", crate::paths::shown(&p)))),
                    },
                    Some(s) if s.contains(&path) => steps.push(Step::Note(format!(
                        "The nerd lines in {} do nothing once the preset's file is gone; take them out when you like.",
                        crate::paths::shown(&p)
                    ))),
                    _ => {}
                }
            }
            remove(&mut steps, &["wezterm-preset.lua"]);
        }
        Terminal::Iterm2 => {
            if iterm_profile().exists() {
                steps.push(Step::Remove(iterm_profile()));
            }
            if margins_record().exists() {
                steps.push(Step::Margins(false));
            }
        }
    }
    Ok(steps)
}

/// The steps as a person reads them before saying yes; `full` adds what nerd's own files hold, which a window
/// has no room for.
pub fn describe(steps: &[Step], full: bool) -> Vec<String> {
    let indent = |s: &str| s.lines().map(|l| format!("    {l}")).collect::<Vec<_>>();
    let mut out = Vec::new();
    for s in steps {
        match s {
            Step::Write(p, text) if full => {
                out.push(format!("write {}:", crate::paths::shown(p)));
                out.extend(indent(text));
            }
            Step::Write(p, _) => out.push(format!("write {}, a file of nerd's", crate::paths::shown(p))),
            Step::Append(p, text) => {
                let new = if p.exists() { "" } else { ", a new file" };
                out.push(format!("add to the end of {}{new}:", crate::paths::shown(p)));
                out.extend(indent(text));
            }
            Step::Unappend(p, _) => out.push(format!("take nerd's lines out of {}", crate::paths::shown(p))),
            Step::Insert(p, line) => {
                out.push(format!("add one line to {}, right before its return:", crate::paths::shown(p)));
                out.extend(indent(line.trim_end()));
            }
            Step::Uninsert(p, _) => out.push(format!("take nerd's line out of {}", crate::paths::shown(p))),
            Step::Line(p, line) => {
                out.push(wezterm_needs(p));
                out.extend(indent(line));
            }
            Step::Remove(p) => out.push(format!("remove {}", crate::paths::shown(p))),
            Step::Margins(true) => out.push(format!(
                "set iTerm2's margins, which every profile shares, to {MARGIN} (now {} and {}); undo puts them back",
                margin_now(ITERM_KEYS[0]).map_or("unset".into(), |v| v.to_string()),
                margin_now(ITERM_KEYS[1]).map_or("unset".into(), |v| v.to_string())
            )),
            Step::Margins(false) => out.push("put iTerm2's margins back as they were".into()),
            Step::Note(n) => out.push(n.clone()),
        }
    }
    out
}

/// Makes the steps, in order; the first failure stops them.
pub fn run(steps: &[Step]) -> std::io::Result<()> {
    for s in steps {
        match s {
            Step::Write(p, text) => write(p, text)?,
            Step::Append(p, text) => {
                let have = read(p)?.unwrap_or_default();
                let sep = if have.is_empty() { "" } else { "\n" };
                write(p, &format!("{have}{sep}{text}"))?;
            }
            Step::Unappend(p, text) => unappend(p, text)?,
            Step::Insert(p, line) => {
                let have = read(p)?.unwrap_or_default();
                let Some((at, _)) = wezterm_return(&have).filter(|(_, l)| l == line) else {
                    return Err(std::io::Error::other(format!("{} changed after it was read; nothing was added", crate::paths::shown(p))));
                };
                write(p, &format!("{}{line}{}", &have[..at], &have[at..]))?;
            }
            Step::Uninsert(p, line) => {
                let have = read(p)?.unwrap_or_default();
                if let Some(at) = have.find(line.as_str()) {
                    write(p, &format!("{}{}", &have[..at], &have[at + line.len()..]))?;
                }
            }
            Step::Remove(p) => match std::fs::remove_file(p) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
                _ => {}
            },
            Step::Margins(on) => set_margins(*on)?,
            Step::Note(_) | Step::Line(..) => {}
        }
    }
    Ok(())
}

fn write(p: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(p, text)
}

/// Takes the block out with the newline apply put before it; refuses a block changed by hand.
fn unappend(p: &Path, text: &str) -> std::io::Result<()> {
    let Some(have) = read(p)? else { return Ok(()) };
    let made = text.replacen(": `nerd --preset", ", in a file nerd made: `nerd --preset", 1);
    let first = |t: &str| t.lines().next().unwrap_or_default().to_string();
    let (at, len, made) = match (have.find(text), have.find(&made)) {
        (Some(i), _) => (i, text.len(), false),
        (None, Some(i)) => (i, made.len(), true),
        (None, None) if have.contains(&first(text)) || have.contains(&first(&made)) => {
            return Err(std::io::Error::other(format!("{}: nerd's lines were changed by hand, so they are left as they are", crate::paths::shown(p))));
        }
        (None, None) => return Ok(()),
    };
    let from = if at > 0 && have.as_bytes()[at - 1] == b'\n' { at - 1 } else { at };
    let rest = format!("{}{}", &have[..from], &have[at + len..]);
    if made && rest.is_empty() {
        return std::fs::remove_file(p);
    }
    std::fs::write(p, rest)
}

/// Rewrites nerd's own files of every applied preset, so the keys and the colours follow this build and config.
/// Says whether Ghostty's changed, for a reload.
pub fn refresh(cfg: &crate::config::Config, dark: impl Fn() -> bool) -> std::io::Result<bool> {
    let mut ghostty_changed = false;
    for t in ALL.into_iter().filter(|t| applied(*t)) {
        let files = match t {
            Terminal::Ghostty => ghostty(cfg),
            Terminal::Kitty => kitty(cfg, dark()),
            Terminal::Wezterm => wezterm(cfg),
            Terminal::Iterm2 => iterm(cfg),
        };
        for s in files {
            if let Step::Write(p, text) = s
                && read(&p)?.as_deref() != Some(text.as_str())
            {
                write(&p, &text)?;
                ghostty_changed |= t == Terminal::Ghostty;
            }
        }
    }
    // until the old link is replaced, Ghostty loads the key file it names
    let (_, keys, _) = legacy();
    if !applied(Terminal::Ghostty) && legacy_linked() && read(&keys)?.as_deref() != Some(crate::ghostty::conf().as_str()) {
        write(&keys, &crate::ghostty::conf())?;
        ghostty_changed = true;
    }
    Ok(ghostty_changed)
}

/// What the steps changed, for the log line each apply and undo writes.
pub fn touched(steps: &[Step]) -> String {
    let names: Vec<String> = steps
        .iter()
        .filter_map(|s| match s {
            Step::Write(p, _) | Step::Append(p, _) | Step::Unappend(p, _) | Step::Insert(p, _) | Step::Uninsert(p, _) | Step::Remove(p) => Some(crate::paths::shown(p)),
            Step::Margins(_) => Some("iTerm2's margins".into()),
            Step::Note(_) | Step::Line(..) => None,
        })
        .collect();
    names.join(", ")
}

/// Everything a preset left taken back, applied or not (the old key link, iTerm2's margins), for `nerd --uninstall`.
pub fn undo_all() -> Vec<String> {
    let mut failed = Vec::new();
    for t in ALL {
        match undo_steps(t).and_then(|s| run(&s).map(|()| s)) {
            Ok(s) if s.iter().all(|s| matches!(s, Step::Note(_))) => {}
            Ok(s) => crate::log::info(&format!("preset for {} undone by nerd --uninstall: {}", t.title(), touched(&s))),
            Err(e) => failed.push(format!("{}: {e}", t.title())),
        }
    }
    failed
}

/// `nerd --preset [terminal] [--undo]`: lists them, or shows every change and asks before making it.
pub fn main(args: &[String]) -> std::io::Result<()> {
    let usage = || std::io::Error::other(format!("usage: nerd --preset [{}] [--undo]", ALL.map(Terminal::name).join("|")));
    let undo = args.iter().any(|a| a == "--undo");
    let mut names = args.iter().filter(|a| *a != "--undo");
    let (Some(name), None) = (names.next(), names.next()) else {
        if undo || args.len() > 1 {
            return Err(usage());
        }
        println!("Option as Alt on the left key, a {MARGIN} pt margin and nerd's colours, per terminal:");
        for t in ALL {
            println!("  {:<8} {}", t.name(), status(t));
        }
        println!("nerd --preset <terminal> shows the changes and asks first; --undo takes them back.");
        return Ok(());
    };
    let t = Terminal::parse(name).ok_or_else(usage)?;
    let cfg = crate::config::load_or_init();
    let steps = if undo { undo_steps(t)? } else { apply_steps(t, &cfg, crate::client::appearance_dark().unwrap_or(true))? };
    if !steps.iter().any(|s| !matches!(s, Step::Note(_))) {
        println!("Nothing to do for {}.", t.title());
        return Ok(());
    }
    println!("{} the {} preset will:", if undo { "Undoing" } else { "Applying" }, t.title());
    for line in describe(&steps, true) {
        println!("  {line}");
    }
    print!("{}? [y/N] ", if undo { "Undo" } else { "Apply" });
    std::io::Write::flush(&mut std::io::stdout())?;
    if !crate::install::answered_yes(std::io::stdin().lock())? {
        println!("Nothing changed");
        return Ok(());
    }
    run(&steps)?;
    if let Some((p, _)) = needs_line(&steps) {
        crate::log::info(&format!("preset for {} needs the user's line in {}, from the command line: {}", t.title(), crate::paths::shown(p), touched(&steps)));
        println!("Not applied yet: WezTerm needs the line above in {}.", crate::paths::shown(p));
        return Ok(());
    }
    crate::log::info(&format!("preset for {} {} from the command line: {}", t.title(), if undo { "undone" } else { "applied" }, touched(&steps)));
    println!("Done.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::with_temp_home;
    use crate::config::Config;

    fn files(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    out.push((p.clone(), std::fs::read(&p).unwrap()));
                }
            }
        }
        out.sort();
        out
    }

    fn apply(t: Terminal) {
        run(&apply_steps(t, &Config::default(), true).unwrap()).unwrap();
    }

    fn undo(t: Terminal) {
        run(&undo_steps(t).unwrap()).unwrap();
    }

    /// Apply then undo leaves every file under HOME as it was, for each config the terminal could have had.
    fn round_trip(t: Terminal, config: &Path, before: Option<&[u8]>) {
        with_temp_home(|| {
            let home = home();
            let config = home.join(config);
            if let Some(b) = before {
                std::fs::create_dir_all(config.parent().unwrap()).unwrap();
                std::fs::write(&config, b).unwrap();
            }
            let was = files(&home);
            apply(t);
            assert!(applied(t), "{t:?} applied");
            apply(t);
            let configs = match t {
                Terminal::Ghostty => ghostty_configs(),
                Terminal::Kitty => vec![kitty_conf()],
                Terminal::Wezterm => wezterm_configs(),
                Terminal::Iterm2 => vec![],
            };
            let blocks: usize = configs
                .iter()
                .map(|p| std::fs::read_to_string(p).unwrap_or_default())
                .map(|s| s.matches(&begin(t)).count() + s.matches(WEZTERM_MARK).count())
                .sum();
            assert_eq!(blocks, usize::from(t != Terminal::Iterm2), "{t:?}: one block however often it is applied");
            undo(t);
            assert!(!applied(t), "{t:?} undone");
            // iTerm2's settings come back by value (iterm_gets_a_profile_and_its_margins_back), not by bytes
            let mut now = files(&home);
            now.retain(|(p, _)| !p.ends_with("config.json") && *p != test_prefs());
            assert_eq!(now, was, "{t:?} with {before:?}: undo is byte-identical");
        });
    }

    #[test]
    fn apply_then_undo_is_byte_identical() {
        let cases: [Option<&[u8]>; 5] = [None, Some(b""), Some(b"font-size = 13\n"), Some(b"font-size = 13"), Some(b"a = 1\r\nb = 2\r\n")];
        for before in cases {
            round_trip(Terminal::Ghostty, Path::new(".config/ghostty/config"), before);
            round_trip(Terminal::Kitty, Path::new(".config/kitty/kitty.conf"), before);
            round_trip(Terminal::Iterm2, Path::new("Library/Preferences/x"), before);
        }
        round_trip(Terminal::Wezterm, Path::new(".wezterm.lua"), None);
        for (path, before) in [
            (".wezterm.lua", "local wezterm = require 'wezterm'\nlocal config = wezterm.config_builder()\nconfig.window_padding = { left = 2 }\nreturn config\n"),
            (".config/wezterm/wezterm.lua", "local c = {\n  font_size = 13,\n}\n\nreturn c\n\n"),
            (".wezterm.lua", "local config = {}\r\nconfig.font_size = 13\r\nreturn config\r\n"),
            (".wezterm.lua", "local config = {}\nreturn config"),
        ] {
            round_trip(Terminal::Wezterm, Path::new(path), Some(before.as_bytes()));
        }
    }

    #[test]
    fn wezterm_puts_its_line_right_before_the_return_of_a_config_it_recognises() {
        with_temp_home(|| {
            let mine = home().join(".wezterm.lua");
            let crlf = "local wezterm = require 'wezterm'\r\nlocal config = wezterm.config_builder()\r\nreturn config";
            std::fs::write(&mine, crlf).unwrap();
            apply(Terminal::Wezterm);
            let line = format!("{} if ok then nerd(config) end {WEZTERM_MARK}", wezterm_load());
            assert_eq!(std::fs::read_to_string(&mine).unwrap(), format!("local wezterm = require 'wezterm'\r\nlocal config = wezterm.config_builder()\r\n{line}\r\nreturn config"));
            assert_eq!(status(Terminal::Wezterm), "applied");
        });
    }

    #[test]
    fn wezterm_undo_keeps_a_user_edit_and_refuses_its_line_changed_by_hand() {
        with_temp_home(|| {
            let mine = home().join(".wezterm.lua");
            std::fs::write(&mine, "local config = {}\nreturn config\n").unwrap();
            apply(Terminal::Wezterm);
            let s = std::fs::read_to_string(&mine).unwrap().replacen("local config = {}\n", "local config = {}\nconfig.font_size = 13\n", 1);
            std::fs::write(&mine, &s).unwrap();
            undo(Terminal::Wezterm);
            assert_eq!(std::fs::read_to_string(&mine).unwrap(), "local config = {}\nconfig.font_size = 13\nreturn config\n");

            apply(Terminal::Wezterm);
            let s = std::fs::read_to_string(&mine).unwrap().replace("if ok then", "if  ok then");
            std::fs::write(&mine, &s).unwrap();
            assert!(undo_steps(Terminal::Wezterm).and_then(|s| run(&s)).is_err(), "said, not guessed");
            assert_eq!(std::fs::read_to_string(&mine).unwrap(), s);
        });
    }

    #[test]
    fn a_user_edit_after_apply_survives_undo() {
        with_temp_home(|| {
            let conf = home().join(".config/kitty/kitty.conf");
            std::fs::create_dir_all(conf.parent().unwrap()).unwrap();
            std::fs::write(&conf, "font_size 13\n").unwrap();
            apply(Terminal::Kitty);
            let mut s = std::fs::read_to_string(&conf).unwrap();
            s.insert_str(0, "# mine\n");
            s.push_str("cursor_shape beam\n");
            std::fs::write(&conf, &s).unwrap();
            undo(Terminal::Kitty);
            assert_eq!(std::fs::read_to_string(&conf).unwrap(), "# mine\nfont_size 13\ncursor_shape beam\n");
        });
    }

    #[test]
    fn a_block_changed_by_hand_is_left_alone() {
        with_temp_home(|| {
            apply(Terminal::Kitty);
            let conf = kitty_conf();
            let s = std::fs::read_to_string(&conf).unwrap().replace("include ", "include  ");
            std::fs::write(&conf, &s).unwrap();
            assert!(run(&undo_steps(Terminal::Kitty).unwrap()).is_err(), "said, not guessed");
            assert_eq!(std::fs::read_to_string(&conf).unwrap(), s);
        });
    }

    #[test]
    fn undo_without_the_mark_writes_nothing() {
        with_temp_home(|| {
            let conf = kitty_conf();
            std::fs::create_dir_all(conf.parent().unwrap()).unwrap();
            std::fs::write(&conf, "include nerd.conf\n").unwrap();
            assert!(undo_steps(Terminal::Kitty).unwrap().is_empty());
        });
    }

    #[test]
    fn ghostty_appends_to_the_file_it_loads_last_and_creates_config_ghostty_only_without_one() {
        with_temp_home(|| {
            apply(Terminal::Ghostty);
            assert!(has_block(Terminal::Ghostty, &home().join(".config/ghostty/config.ghostty")), "none: config.ghostty in XDG");
            undo(Terminal::Ghostty);
            assert!(!home().join(".config/ghostty").join("config.ghostty").exists());

            let xdg = home().join(".config/ghostty/config");
            let support = home().join("Library/Application Support/com.mitchellh.ghostty/config");
            for p in [&xdg, &support] {
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(p, "font-size = 13\n").unwrap();
            }
            apply(Terminal::Ghostty);
            assert!(has_block(Terminal::Ghostty, &support) && !has_block(Terminal::Ghostty, &xdg), "the one loaded last");
            let conf = std::fs::read_to_string(own("ghostty-preset.conf")).unwrap();
            for line in ["macos-option-as-alt = left", "window-padding-x = 16", "window-padding-balance = true", "keybind = alt+key_t=esc:t"] {
                assert!(conf.contains(line), "{line}");
            }
            assert!(conf.contains(&format!("theme = light:{},dark:{}", own("ghostty-light.conf").display(), own("ghostty-dark.conf").display())));
            let dark = std::fs::read_to_string(own("ghostty-dark.conf")).unwrap();
            assert!(dark.contains(&format!("background = {}", hex(Theme::dark().ground))) && dark.contains("palette = 15="), "{dark}");
        });
    }

    #[test]
    fn ghostty_takes_the_old_install_keys_link_out() {
        with_temp_home(|| {
            let (old, keys, lines) = legacy();
            std::fs::create_dir_all(old.parent().unwrap()).unwrap();
            std::fs::write(&old, format!("font-size = 13\n\n{lines}")).unwrap();
            std::fs::create_dir_all(keys.parent().unwrap()).unwrap();
            std::fs::write(&keys, "keybind = x\n").unwrap();
            assert!(!applied(Terminal::Ghostty), "the old link is not the preset");
            assert_eq!(status(Terminal::Ghostty), "old keys link");
            assert!(refresh(&Config::default(), || true).unwrap(), "Ghostty reloads the key file it loads");
            assert_eq!(std::fs::read_to_string(&keys).unwrap(), crate::ghostty::conf(), "the old link's keys follow this build");
            assert!(!own("ghostty-preset.conf").exists(), "nothing written that the old link does not load");
            assert!(!refresh(&Config::default(), || true).unwrap());
            let steps = apply_steps(Terminal::Ghostty, &Config::default(), true).unwrap();
            assert!(steps.iter().any(|s| matches!(s, Step::Unappend(p, _) if *p == old)) && steps.iter().any(|s| matches!(s, Step::Append(..))), "apply replaces it");
            apply(Terminal::Ghostty);
            let s = std::fs::read_to_string(&old).unwrap();
            assert!(!s.contains(&lines) && s.starts_with("font-size = 13\n") && s.contains(&begin(Terminal::Ghostty)), "{s}");
            assert!(!keys.exists());
            undo(Terminal::Ghostty);
            assert_eq!(std::fs::read_to_string(&old).unwrap(), "font-size = 13\n");
        });
    }

    #[test]
    fn the_old_link_is_looked_for_where_install_keys_wrote_it_and_uninstall_takes_it_out() {
        with_temp_home(|| {
            set_env("XDG_CONFIG_HOME", home().join("elsewhere"));
            let (old, keys, lines) = legacy();
            assert_eq!(old, home().join(".config/ghostty/config"), "--install-keys never followed XDG_CONFIG_HOME");
            std::fs::create_dir_all(old.parent().unwrap()).unwrap();
            std::fs::write(&old, format!("font-size = 13\n\n{lines}")).unwrap();
            std::fs::create_dir_all(keys.parent().unwrap()).unwrap();
            std::fs::write(&keys, "keybind = x\n").unwrap();
            assert!(undo_all().is_empty());
            assert_eq!((std::fs::read_to_string(&old).unwrap().as_str(), keys.exists()), ("font-size = 13\n", false), "nerd --uninstall takes the old link out too");
        });
    }

    #[test]
    fn wezterm_says_it_needs_your_line_for_a_config_it_does_not_recognise() {
        with_temp_home(|| {
            let mine = home().join(".config/wezterm/wezterm.lua");
            std::fs::create_dir_all(mine.parent().unwrap()).unwrap();
            std::fs::write(&mine, "return {}\n").unwrap();
            let steps = apply_steps(Terminal::Wezterm, &Config::default(), true).unwrap();
            assert!(!steps.iter().any(|s| matches!(s, Step::Append(..) | Step::Insert(..))), "a config nerd does not recognise is not edited");
            let (_, line) = needs_line(&steps).expect("the line to add");
            assert_eq!(line, format!("{} if ok then nerd(config) end", wezterm_load()));
            let text = describe(&steps, true).join("\n");
            assert!(text.contains("WezTerm needs one line from you") && text.contains(&line) && text.contains("local config = {"), "{text}");
            run(&steps).unwrap();
            assert_eq!(std::fs::read_to_string(&mine).unwrap(), "return {}\n");
            assert!(!applied(Terminal::Wezterm), "not until the line is there");
            assert_eq!(status(Terminal::Wezterm), "needs your line");
            std::fs::write(&mine, format!("local config = {{}}\n{line}\nreturn config\n")).unwrap();
            assert!(applied(Terminal::Wezterm));
            undo(Terminal::Wezterm);
            assert!(!own("wezterm-preset.lua").exists());
            assert_eq!(status(Terminal::Wezterm), "not applied");
        });
    }

    /// Both printed shapes, run by a real Lua with a stub `wezterm` module, when one is installed.
    #[test]
    fn the_wezterm_file_changes_the_table_and_returns_it() {
        let Some(lua) = ["/opt/homebrew/bin/lua", "/usr/local/bin/lua"].into_iter().find(|p| Path::new(p).exists()) else { return };
        with_temp_home(|| {
            run(&wezterm(&Config::default())).unwrap();
            let load = wezterm_load();
            let stub = "package.preload['wezterm'] = function() return {} end\n";
            for (shape, check) in [
                (format!("local config = {{}}\n{load} if ok then nerd(config) end\n"), "config"),
                (format!("{load} if not ok then nerd = function(c) return c end end\nlocal r = nerd({{ font_size = 9 }})\n"), "r"),
            ] {
                let script = format!("{stub}{shape}assert({check}.window_padding.left == '16pt' and {check}.colors.background == '{}')\n", hex(Theme::dark().ground));
                let out = std::process::Command::new(lua).arg("-e").arg(&script).output().unwrap();
                assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            }
            let mine = home().join(".wezterm.lua");
            std::fs::write(&mine, "local wezterm = require 'wezterm'\nlocal config = wezterm.config_builder()\nconfig.font_size = 9\nreturn config\n").unwrap();
            apply(Terminal::Wezterm);
            let stub = "package.preload['wezterm'] = function() return { config_builder = function() return {} end } end\n";
            let script = format!("{stub}local c = dofile('{}')\nassert(c.window_padding.left == '16pt' and c.font_size == 9)\n", mine.display());
            let out = std::process::Command::new(lua).arg("-e").arg(&script).output().unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        });
    }

    #[test]
    fn iterm_gets_a_profile_and_its_margins_back() {
        with_temp_home(|| {
            margin_set("TerminalMargin", Some(5)).unwrap();
            apply(Terminal::Iterm2);
            assert_eq!((margin_now("TerminalMargin"), margin_now("TerminalVMargin")), (Some(16), Some(16)));
            let profile: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(iterm_profile()).unwrap()).unwrap();
            let p = &profile["Profiles"][0];
            assert_eq!((p["Option Key Sends"].as_i64(), p["Right Option Key Sends"].as_i64()), (Some(2), Some(0)), "left Option is Alt, right types");
            assert!(p["Ansi 15 Color (Light)"]["Red Component"].is_f64());
            undo(Terminal::Iterm2);
            assert_eq!((margin_now("TerminalMargin"), margin_now("TerminalVMargin")), (Some(5), None), "as they were, absent included");
        });
    }

    #[test]
    fn refresh_rewrites_only_applied_presets() {
        with_temp_home(|| {
            assert!(!refresh(&Config::default(), || true).unwrap());
            assert!(!own("ghostty-preset.conf").exists(), "nothing applied, nothing written");
            apply(Terminal::Ghostty);
            let mut cfg = Config::default();
            cfg.colors.insert("ground".into(), "#102030".into());
            assert!(refresh(&cfg, || true).unwrap(), "the colours changed");
            assert!(std::fs::read_to_string(own("ghostty-dark.conf")).unwrap().contains("background = #102030"));
            assert!(!refresh(&cfg, || true).unwrap(), "and the same again changes nothing");
        });
    }
}
