//! Client-to-server: one JSON `ClientMsg` per line. Server-to-client: raw terminal bytes, written to stdout as-is.

use std::io::{BufRead, Read, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Per-message cap so a garbage stream can't exhaust the daemon's memory.
const MAX_LINE: u64 = 8 << 20;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ClientMsg {
    Hello {
        cwd: PathBuf,
        cols: u16,
        rows: u16,
        /// Set when running inside tmux/screen, which carry no graphics and turn an APC query into a pane title.
        #[serde(default)]
        mux: bool,
        /// The multiplexer is screen, not tmux; left off the wire otherwise, which keeps the v1 hello as it was.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        screen: bool,
        /// The terminal answered the Kitty graphics query; `None` from a client older than the query.
        #[serde(default)]
        graphics: Option<bool>,
        /// The terminal app showing this multiplexer, when nerd could open a window in it.
        #[serde(default)]
        terminal: Option<std::path::PathBuf>,
        /// This client was started by nerd's own offer; it never makes the offer again on its own.
        #[serde(default)]
        opened_by_nerd: bool,
        /// This client understands the daemon's request to open a window and does it itself.
        #[serde(default)]
        opens_windows: bool,
        /// The macOS appearance is dark; `None` when it could not be read, or from an older client.
        #[serde(default)]
        dark: Option<bool>,
        /// This machine's hostname; `None` from an older client.
        #[serde(default)]
        host: Option<String>,
        /// This client reads the `armed`, `bye` and `stopped` markers; only a remote client does.
        #[serde(default)]
        bye: bool,
        /// This client answers `list-remote` and `open-remote`.
        #[serde(default)]
        connects: bool,
        /// What brings this client back, when it is not `nerd --resume <instance>` on this Mac.
        #[serde(default)]
        reattach: Option<String>,
        /// The machine from this client's config that the server runs on; its window titles end in ` @ <machine>`.
        #[serde(default)]
        machine: Option<String>,
        /// A row's height in device pixels, 0 when the terminal does not say; `None` from an older client,
        /// and left off the wire then, which keeps the v1 hello as it was.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cell_px: Option<u16>,
        /// Refuse this attach when another session shows the server; an older server ignores it, so it goes only to one that answered `Probe`.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        if_free: bool,
        /// This client can switch itself to another server of this Mac when asked.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        switches: bool,
    },
    Event(crossterm::event::Event),
    /// The macOS appearance changed while attached: dark or not.
    Appearance(bool),
    /// The row height in device pixels changed while attached (another display, another font size); an older server logs it and goes on.
    CellHeight(u16),
    Detach,
    Shutdown,
    Update,
    /// Ends every connection, the screen's included; sent without Hello, answered with `ok`, the ended ones, then EOF.
    DisconnectAll,
    /// Sent without Hello: answered with one `ProbeAnswer` line, then EOF; an older server just closes.
    Probe,
    /// The answer to `list-remote`: that machine's instances or why not, and whether this client can open a window.
    Remote {
        name: String,
        result: Result<Vec<RemoteInstance>, String>,
        #[serde(default)]
        window: bool,
        /// ssh itself failed (exit 255): perhaps a prompt BatchMode refused, which a terminal can answer.
        #[serde(default)]
        ssh_failed: bool,
    },
    /// Protocol 2's first message; a v1 daemon cannot read it and closes the socket, so it fails closed.
    Connect {
        protocol: u32,
        role: String,
        #[serde(default)]
        plugin: Option<String>,
        #[serde(default)]
        events: Vec<String>,
    },
    /// From a plugin: the state of a pane whose agent is this plugin's; the plugin socket reads it as a `PluginMsg`.
    ReportState { pane: usize, state: String },
}

/// Plugin to server, each line after `Connect`: the public API, which only grows (docs/plugins.md).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum PluginMsg {
    ReportState { pane: usize, state: String },
    /// Replaces this plugin's badges on `target`: `tree` rows by `path`, or tabs by `pane`.
    Badges { target: String, items: Vec<BadgeItem> },
    Segment {
        id: String,
        text: String,
        #[serde(default)]
        role: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<String>,
    },
    SegmentClear { id: String },
    /// The answer to `preview_request` number `req`: styled lines only, and the views the plugin offers of this file.
    Preview {
        req: u64,
        lines: Vec<Vec<TextSpan>>,
        #[serde(default)]
        cut: bool,
        /// Ways to show this file (`v` and the menu go to the next one), and which one these lines are.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        modes: Vec<PreviewMode>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        mode: String,
    },
    /// Replaces panel `id`'s content: `kind` is `list`, `tree` or `detail`; a detail panel's `lines` go under its items.
    Panel {
        id: String,
        kind: String,
        #[serde(default)]
        title: String,
        #[serde(default)]
        items: Vec<PanelItem>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        lines: Vec<Vec<TextSpan>>,
    },
}

/// One way a plugin can show a file: `id` comes back in `preview_request`'s `mode`, `label` is shown.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct PreviewMode {
    pub id: String,
    pub label: String,
}

/// A panel's row; `children` only in a tree.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct PanelItem {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub role: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<PanelItem>,
    /// Chosen, it keeps the panel open for the plugin's next content instead of closing it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub opens: bool,
}

/// A run of text in one theme role.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct TextSpan {
    pub text: String,
    #[serde(default)]
    pub role: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct BadgeItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<usize>,
    pub text: String,
    #[serde(default)]
    pub role: String,
}

impl PluginMsg {
    pub fn name(&self) -> &'static str {
        match self {
            PluginMsg::ReportState { .. } => "ReportState",
            PluginMsg::Badges { .. } => "Badges",
            PluginMsg::Segment { .. } => "Segment",
            PluginMsg::SegmentClear { .. } => "SegmentClear",
            PluginMsg::Preview { .. } => "Preview",
            PluginMsg::Panel { .. } => "Panel",
        }
    }
}

/// One JSON line, capped at `MAX_LINE`; `None` at EOF.
pub fn read_json<T: serde::de::DeserializeOwned>(r: &mut impl BufRead) -> std::io::Result<Option<T>> {
    let mut line = String::new();
    if r.by_ref().take(MAX_LINE).read_line(&mut line)? == 0 {
        return Ok(None);
    }
    if !line.ends_with('\n') && line.len() as u64 >= MAX_LINE {
        return Err(std::io::Error::other("message longer than the limit"));
    }
    let msg = serde_json::from_str(line.trim_end()).map_err(std::io::Error::other)?;
    Ok(Some(msg))
}

/// The protocol a `Connect` is answered with; it only grows.
pub const PROTOCOL: u32 = 2;

/// What `nerd --list --json` prints; `relay` is the version of `--relay` this nerd speaks.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Listing {
    pub relay: u32,
    pub instances: Vec<RemoteInstance>,
}

pub const RELAY_VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct RemoteInstance {
    pub id: String,
    pub label: String,
    pub running: bool,
    #[serde(default)]
    pub spaces: usize,
    /// A session shows it; `None` from an older nerd, or from a running server that did not say.
    /// Left off the wire when unknown, as are the two below, which keeps the v1 `Remote` as it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shown: Option<bool>,
    /// When a session last attached, in seconds since the epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used: Option<u64>,
    /// Its spaces' folders, as that Mac spells them; text from another machine, escaped before it is shown.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub folders: Vec<String>,
}

/// A server's answer to `Probe`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ProbeAnswer {
    pub shown: bool,
    pub version: String,
    pub spaces: usize,
}

/// The `refused:` line an `if_free` Hello gets when another session shows the server.
pub const TAKEN: &str = "refused: this server is open in another session";

/// Parsed once from the Hello; a handover carries it flat under the v1 names, so either build reads the other's.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(default)]
pub struct ClientCaps {
    /// Inside tmux/screen, which mangle Kitty graphics; the handover carries it in its `client` tuple, not here.
    #[serde(skip)]
    pub mux: bool,
    /// The multiplexer is screen, not tmux.
    pub screen: bool,
    /// What the terminal answered to the Kitty graphics query; `None` from a client older than the
    /// query, and a guess must never be handed on as its answer.
    pub graphics: Option<bool>,
    /// The terminal app showing this client's multiplexer, if nerd can open a window in it.
    pub terminal: Option<PathBuf>,
    /// This client came from nerd's own offer.
    pub opened_by_nerd: bool,
    /// The client opens the window itself when asked: it is a fresh process in the user's
    /// session, where macOS's launch services always answer, and it passes its own environment on.
    pub opens_windows: bool,
    /// The client's macOS appearance is dark; `None` when it did not say.
    pub dark: Option<bool>,
    /// It reads the `armed`, `bye` and `stopped` markers.
    pub bye: bool,
    /// It answers `list-remote` and `open-remote`.
    pub connects: bool,
    pub reattach: Option<String>,
    /// The machine name the client knows this server by, for ` @ <machine>` in its window title.
    pub machine: Option<String>,
    /// A row's height in device pixels; 0 when unknown.
    pub cell_px: u16,
    /// It switches itself to another server when asked.
    pub switches: bool,
}

impl ClientCaps {
    /// `None` for any message but Hello. The way back and the machine name reach the screen, so they are checked here.
    pub fn from_v1_hello(msg: &ClientMsg) -> Option<Self> {
        let ClientMsg::Hello { mux, screen, graphics, terminal, opened_by_nerd, opens_windows, dark, bye, connects, reattach, machine, cell_px, switches, .. } = msg else {
            return None;
        };
        Some(Self {
            mux: *mux,
            screen: *screen,
            graphics: *graphics,
            terminal: terminal.clone(),
            opened_by_nerd: *opened_by_nerd,
            opens_windows: *opens_windows,
            dark: *dark,
            bye: *bye,
            connects: *connects,
            reattach: reattach.clone().filter(|r| !r.is_empty() && !r.chars().any(char::is_control)),
            // The same check as a machine name in config.json: it goes into the window title.
            machine: machine.clone().filter(|m| crate::config::RemoteEntry { name: m.clone(), host: "h".into() }.check().is_ok()),
            cell_px: cell_px.unwrap_or(0),
            switches: *switches,
        })
    }
}

/// No answer (a client or handover from a build before the query): blocks, which every terminal draws.
/// Assuming graphics instead would spray the image's base64 across a terminal that cannot read it.
pub fn graphics_or_guess(reported: Option<bool>) -> bool {
    reported.unwrap_or(false)
}

impl ClientMsg {
    pub fn write_to(&self, w: &mut impl Write) -> std::io::Result<()> {
        serde_json::to_writer(&mut *w, self).map_err(std::io::Error::other)?;
        w.write_all(b"\n")?;
        w.flush()
    }

    pub fn read_from(r: &mut impl BufRead) -> std::io::Result<Option<Self>> {
        read_json(r)
    }
}

/// Asks the client to open a window: an APC the daemon writes into the byte stream, sent only
/// to a client that said it understands it. The daemon's own `open` cannot always reach macOS's
/// launch services, a fresh client always can.
pub const OPEN_WINDOW: &[u8] = b"\x1b_nerd;open-window\x1b\\";

/// Asks the client to have the terminal reread its config. The daemon writes the key file when
/// a new build takes over, and only the client sits under the terminal to signal it.
pub const RELOAD_KEYS: &[u8] = b"\x1b_nerd;reload-keys\x1b\\";

/// To a remote client, before the first frame: from now on an end without `BYE` is a loss.
pub const ARMED: &[u8] = b"\x1b_nerd;armed\x1b\\";

/// To a remote client, before a close the server makes on purpose.
pub const BYE: &[u8] = b"\x1b_nerd;bye\x1b\\";

const MARK: &[u8] = b"\x1b_nerd;";
const MARK_END: &[u8] = b"\x1b\\";
/// The longest marker body the pump waits for; anything longer is not ours and reaches the screen.
const MARK_MAX: usize = 256;

/// Control characters would end the marker early or reach the screen.
fn marker(body: &str) -> Vec<u8> {
    let body: String = body.chars().filter(|c| !c.is_control()).take(MARK_MAX).collect();
    [MARK, body.as_bytes(), MARK_END].concat()
}

/// To a remote client, from the server's own shutdown: why it stopped.
pub fn stopped_marker(reason: &str) -> Vec<u8> {
    marker(&format!("stopped:{reason}"))
}

/// Asks the client for the instances on a machine from its config.json.
pub fn list_remote_marker(name: &str) -> Vec<u8> {
    marker(&format!("list-remote:{name}"))
}

/// Asks the client to open a terminal window with `nerd --remote <name> [<id>]`.
pub fn open_remote_marker(name: &str, id: Option<&str>) -> Vec<u8> {
    marker(&format!("open-remote:{name}{}", id.map(|i| format!(":{i}")).unwrap_or_default()))
}

/// What the daemon asked of the client in one read, and what the terminal should see.
#[derive(Default, Debug, PartialEq)]
pub struct Requests {
    pub out: Vec<u8>,
    pub open: usize,
    pub reload: bool,
    pub armed: bool,
    pub bye: bool,
    pub stopped: Option<String>,
    pub list: Vec<String>,
    pub connect: Vec<(String, Option<String>)>,
}

impl Requests {
    fn add(&mut self, body: &str) {
        match body.split_once(':') {
            None => match body {
                "open-window" => self.open += 1,
                "reload-keys" => self.reload = true,
                "armed" => self.armed = true,
                "bye" => self.bye = true,
                _ => {}
            },
            Some(("stopped", why)) => self.stopped = Some(why.to_string()),
            Some(("list-remote", name)) => self.list.push(name.to_string()),
            Some(("open-remote", rest)) => self.connect.push(match rest.split_once(':') {
                Some((name, id)) => (name.to_string(), Some(id.to_string())),
                None => (rest.to_string(), None),
            }),
            _ => {}
        }
    }
}

/// Splits the stream into the terminal's bytes and the daemon's requests; `carry` keeps a partial marker.
pub fn take_requests(data: &[u8], carry: &mut Vec<u8>) -> Requests {
    let mut r = Requests { out: Vec::with_capacity(data.len()), ..Requests::default() };
    let mut i = 0;
    while i < data.len() {
        let rest = &data[i..];
        if rest.starts_with(MARK) {
            let body = &rest[MARK.len()..];
            match body.windows(MARK_END.len()).position(|w| w == MARK_END) {
                Some(end) if end <= MARK_MAX => {
                    r.add(&String::from_utf8_lossy(&body[..end]));
                    i += MARK.len() + end + MARK_END.len();
                    continue;
                }
                None if body.len() < MARK_MAX + MARK_END.len() => {
                    carry.extend_from_slice(rest);
                    break;
                }
                _ => {}
            }
        } else if rest.len() < MARK.len() && MARK.starts_with(rest) {
            carry.extend_from_slice(rest);
            break;
        }
        r.out.push(data[i]);
        i += 1;
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn screen_rides_the_hello_only_when_it_is_true() {
        let hello = |screen| ClientMsg::Hello {
            cwd: "/tmp".into(), cols: 80, rows: 24, mux: true, screen, graphics: None, terminal: None, opened_by_nerd: false, opens_windows: false,
            dark: None, host: None, bye: false, connects: false, reattach: None, machine: None, cell_px: None, if_free: false, switches: false,
        };
        let json = |m: &ClientMsg| serde_json::to_string(m).unwrap();
        assert!(!json(&hello(false)).contains("screen"), "the v1 hello is unchanged for tmux");
        let on = json(&hello(true));
        assert!(on.contains(r#""screen":true"#));
        assert!(ClientCaps::from_v1_hello(&serde_json::from_str(&on).unwrap()).unwrap().screen);
        assert!(!ClientCaps::from_v1_hello(&serde_json::from_str(&json(&hello(false))).unwrap()).unwrap().screen);
    }

    #[test]
    fn messages_round_trip_one_per_line() {
        let mut buf = Vec::new();
        ClientMsg::Hello {
            cwd: "/tmp".into(),
            cols: 120,
            rows: 40,
            mux: false,
            screen: false,
            graphics: Some(true),
            terminal: Some("/Applications/Ghostty.app".into()),
            opened_by_nerd: true,
            opens_windows: true,
            dark: Some(true),
            host: Some("MacBook-Air".into()),
            bye: true,
            connects: false,
            reattach: Some("nerd --remote studio 1".into()),
            machine: Some("studio".into()),
            cell_px: Some(35),
            if_free: false,
            switches: false,
        }
            .write_to(&mut buf)
            .unwrap();
        ClientMsg::Event(Event::Key(KeyEvent::new(KeyCode::Char('ж'), KeyModifiers::CONTROL)))
            .write_to(&mut buf)
            .unwrap();
        ClientMsg::Appearance(false).write_to(&mut buf).unwrap();
        ClientMsg::Detach.write_to(&mut buf).unwrap();
        let one = RemoteInstance { id: "1".into(), label: "main".into(), running: true, spaces: 2, ..Default::default() };
        ClientMsg::Remote { name: "studio".into(), result: Ok(vec![one.clone()]), window: true, ssh_failed: false }.write_to(&mut buf).unwrap();

        let mut r = std::io::Cursor::new(buf);
        let Some(ClientMsg::Hello { cols: 120, host, bye, connects, reattach, machine, cell_px, .. }) = ClientMsg::read_from(&mut r).unwrap() else { panic!("hello expected") };
        assert_eq!(machine.as_deref(), Some("studio"));
        assert_eq!(cell_px, Some(35));
        assert_eq!(host.as_deref(), Some("MacBook-Air"));
        assert_eq!((bye, connects, reattach.as_deref()), (true, false, Some("nerd --remote studio 1")));
        assert!(matches!(ClientMsg::read_from(&mut r).unwrap(), Some(ClientMsg::Event(Event::Key(_)))));
        assert!(matches!(ClientMsg::read_from(&mut r).unwrap(), Some(ClientMsg::Appearance(false))));
        assert!(matches!(ClientMsg::read_from(&mut r).unwrap(), Some(ClientMsg::Detach)));
        let Some(ClientMsg::Remote { name, result: Ok(list), window: true, .. }) = ClientMsg::read_from(&mut r).unwrap() else { panic!("remote expected") };
        assert_eq!((name.as_str(), list), ("studio", vec![one]));
        assert!(ClientMsg::read_from(&mut r).unwrap().is_none());
    }

    #[test]
    fn a_hello_from_an_older_client_keeps_images_outside_tmux() {
        let old = br#"{"Hello":{"cwd":"/tmp","cols":80,"rows":24,"mux":false}}"#.to_vec();
        let mut r = std::io::Cursor::new([old, b"\n".to_vec()].concat());
        let Some(ClientMsg::Hello { graphics, mux, terminal, opened_by_nerd, opens_windows, host, bye, connects, reattach, .. }) = ClientMsg::read_from(&mut r).unwrap() else {
            panic!("hello expected");
        };
        assert_eq!(host, None, "an older client names no host");
        assert_eq!((bye, connects, reattach), (false, false, None), "an older client gets no markers and no connect requests");
        assert_eq!((graphics, mux), (None, false));
        assert_eq!(
            (terminal, opened_by_nerd, opens_windows),
            (None, false, false),
            "an older client says nothing about its terminal and cannot open windows"
        );
        assert!(!graphics_or_guess(graphics), "no answer: blocks, never bytes the terminal may print");
        assert!(graphics_or_guess(Some(true)), "a real answer wins");
        assert!(!graphics_or_guess(Some(false)));
    }

    #[test]
    fn caps_are_read_from_a_v1_hello_and_checked_once() {
        let hello = r#"{"Hello":{"cwd":"/tmp","cols":120,"rows":40,"mux":true,"graphics":true,"terminal":"/Applications/Ghostty.app","opened_by_nerd":true,"opens_windows":true,"dark":false,"host":"MacBook-Air","bye":true,"connects":true,"reattach":"nerd --remote studio 1","machine":"studio","cell_px":35}}"#;
        let caps = ClientCaps::from_v1_hello(&serde_json::from_str(hello).unwrap()).unwrap();
        assert_eq!(
            caps,
            ClientCaps {
                mux: true,
                screen: false,
                graphics: Some(true),
                terminal: Some("/Applications/Ghostty.app".into()),
                opened_by_nerd: true,
                opens_windows: true,
                dark: Some(false),
                bye: true,
                connects: true,
                reattach: Some("nerd --remote studio 1".into()),
                machine: Some("studio".into()),
                cell_px: 35,
                switches: false,
            }
        );
        let old = r#"{"Hello":{"cwd":"/tmp","cols":80,"rows":24,"reattach":"a\u001bb","machine":"no spaces allowed"}}"#;
        assert_eq!(ClientCaps::from_v1_hello(&serde_json::from_str(old).unwrap()).unwrap(), ClientCaps::default(), "an older client says nothing; bad text is dropped");
        assert_eq!(ClientCaps::from_v1_hello(&ClientMsg::Detach), None);
    }

    /// The v1 wire as 128de23 speaks it: a client or daemon across an Update must still agree.
    #[test]
    fn the_v1_wire_is_pinned() {
        assert_eq!(RELAY_VERSION, 1);
        let lines = [
            r#"{"Hello":{"cwd":"/tmp","cols":120,"rows":40,"mux":false,"graphics":true,"terminal":"/Applications/Ghostty.app","opened_by_nerd":true,"opens_windows":true,"dark":true,"host":"MacBook-Air","bye":true,"connects":false,"reattach":"nerd --remote studio 1","machine":"studio"}}"#,
            r#"{"Event":{"Key":{"code":{"Char":"a"},"modifiers":"CONTROL | ALT","kind":"Press","state":""}}}"#,
            r#"{"Event":{"Key":{"code":{"F":1},"modifiers":"","kind":"Press","state":""}}}"#,
            r#"{"Event":{"Key":{"code":"Enter","modifiers":"SHIFT","kind":"Press","state":""}}}"#,
            r#"{"Event":{"Mouse":{"kind":{"Down":"Left"},"column":10,"row":5,"modifiers":""}}}"#,
            r#"{"Event":{"Mouse":{"kind":"ScrollUp","column":1,"row":2,"modifiers":"SHIFT"}}}"#,
            r#"{"Event":{"Paste":"hi\nthere"}}"#,
            r#"{"Event":{"Resize":[120,40]}}"#,
            r#"{"Event":"FocusGained"}"#,
            r#"{"Appearance":false}"#,
            r#""Detach""#,
            r#""Shutdown""#,
            r#""Update""#,
            r#""DisconnectAll""#,
            r#"{"Remote":{"name":"studio","result":{"Ok":[{"id":"1","label":"main","running":true,"spaces":2}]},"window":true,"ssh_failed":false}}"#,
            r#"{"Remote":{"name":"studio","result":{"Err":"no route"},"window":false,"ssh_failed":true}}"#,
        ];
        for line in lines {
            let msg: ClientMsg = serde_json::from_str(line).unwrap_or_else(|e| panic!("{line}: {e}"));
            assert_eq!(serde_json::to_string(&msg).unwrap(), line);
        }
    }

    /// What a newer client adds to the v1 wire; an older daemon ignores the field and logs the message.
    #[test]
    fn the_row_height_is_pinned() {
        for line in [r#"{"CellHeight":35}"#, r#"{"Hello":{"cwd":"/tmp","cols":80,"rows":24,"mux":false,"graphics":true,"terminal":null,"opened_by_nerd":false,"opens_windows":false,"dark":null,"host":null,"bye":false,"connects":true,"reattach":null,"machine":null,"cell_px":35}}"#] {
            let msg: ClientMsg = serde_json::from_str(line).unwrap_or_else(|e| panic!("{line}: {e}"));
            assert_eq!(serde_json::to_string(&msg).unwrap(), line);
        }
    }

    /// What this build adds for Servers…: an older server closes on `Probe` and ignores the two Hello fields.
    #[test]
    fn the_probe_and_the_attach_asks_are_pinned() {
        for line in [r#""Probe""#, r#"{"Hello":{"cwd":"/tmp","cols":80,"rows":24,"mux":false,"graphics":null,"terminal":null,"opened_by_nerd":false,"opens_windows":false,"dark":null,"host":null,"bye":false,"connects":true,"reattach":null,"machine":null,"if_free":true,"switches":true}}"#] {
            let msg: ClientMsg = serde_json::from_str(line).unwrap_or_else(|e| panic!("{line}: {e}"));
            assert_eq!(serde_json::to_string(&msg).unwrap(), line);
        }
        let ClientMsg::Hello { if_free, switches, .. } = serde_json::from_str(r#"{"Hello":{"cwd":"/tmp","cols":80,"rows":24}}"#).unwrap() else { panic!() };
        assert!(!if_free && !switches, "an older client asks for neither");
        let a = ProbeAnswer { shown: true, version: "nerd 0.16.0 (abc1234, 2026-10-22 12:00)".into(), spaces: 2 };
        assert_eq!(serde_json::to_string(&a).unwrap(), r#"{"shown":true,"version":"nerd 0.16.0 (abc1234, 2026-10-22 12:00)","spaces":2}"#);
    }

    #[test]
    fn a_listing_reads_across_builds_both_ways() {
        let old = r#"{"relay":1,"instances":[{"id":"1","label":"main","running":true,"spaces":2}]}"#;
        let l: Listing = serde_json::from_str(old).unwrap();
        assert_eq!(l.instances[0], RemoteInstance { id: "1".into(), label: "main".into(), running: true, spaces: 2, shown: None, last_used: None, folders: vec![] });
        #[derive(Deserialize, Debug, PartialEq)]
        struct OldInstance {
            id: String,
            label: String,
            running: bool,
            #[serde(default)]
            spaces: usize,
        }
        #[derive(Deserialize)]
        struct OldListing {
            relay: u32,
            instances: Vec<OldInstance>,
        }
        let new = Listing { relay: RELAY_VERSION, instances: vec![RemoteInstance { id: "2".into(), label: "work".into(), running: true, spaces: 1, shown: Some(true), last_used: Some(1_790_000_000), folders: vec!["/Users/me/proj".into()] }] };
        let json = serde_json::to_string(&new).unwrap();
        let back: OldListing = serde_json::from_str(&json).unwrap();
        assert_eq!((back.relay, &back.instances[0]), (1, &OldInstance { id: "2".into(), label: "work".into(), running: true, spaces: 1 }));
        assert_eq!(serde_json::from_str::<Listing>(&json).unwrap().instances, new.instances);
    }

    #[test]
    fn the_markers_are_pinned() {
        assert_eq!(OPEN_WINDOW, b"\x1b_nerd;open-window\x1b\\");
        assert_eq!(RELOAD_KEYS, b"\x1b_nerd;reload-keys\x1b\\");
        assert_eq!(ARMED, b"\x1b_nerd;armed\x1b\\");
        assert_eq!(BYE, b"\x1b_nerd;bye\x1b\\");
        assert_eq!(stopped_marker("--stop"), b"\x1b_nerd;stopped:--stop\x1b\\");
        assert_eq!(list_remote_marker("studio"), b"\x1b_nerd;list-remote:studio\x1b\\");
        assert_eq!(open_remote_marker("studio", Some("1")), b"\x1b_nerd;open-remote:studio:1\x1b\\");
        assert_eq!(open_remote_marker("studio", None), b"\x1b_nerd;open-remote:studio\x1b\\");
    }

    /// The broker outlives an Update, so the daemon on either side of it speaks this to the same broker.
    #[test]
    fn the_broker_wire_is_pinned() {
        use crate::broker::{Rep, Req};
        for line in [
            r#"{"Spawn":{"id":1,"argv":["zsh"],"cwd":"/tmp","cols":80,"rows":24,"env":[["A","b"]]}}"#,
            r#"{"Resize":{"id":1,"cols":80,"rows":24}}"#,
            r#"{"Kill":{"id":1}}"#,
            r#""Ps""#,
        ] {
            let req: Req = serde_json::from_str(line).unwrap_or_else(|e| panic!("{line}: {e}"));
            assert_eq!(serde_json::to_string(&req).unwrap(), line);
        }
        for line in [r#"{"Spawned":{"id":1,"pid":42}}"#, r#"{"Exited":{"id":1}}"#, r#"{"Exited":{"id":1,"how":"signal 9 (SIGKILL)"}}"#, r#"{"Error":{"id":1,"message":"no"}}"#, r#"{"Ps":{"text":"x"}}"#] {
            let rep: Rep = serde_json::from_str(line).unwrap_or_else(|e| panic!("{line}: {e}"));
            assert_eq!(serde_json::to_string(&rep).unwrap(), line);
        }
    }

    #[test]
    fn a_line_over_the_limit_is_an_error_not_a_hang() {
        let junk = vec![b'x'; (MAX_LINE + 10) as usize];
        let mut r = std::io::Cursor::new(junk);
        assert!(ClientMsg::read_from(&mut r).is_err());
    }

    #[test]
    fn socket_path_honours_override() {
        // Under the environment lock: servers' tests set NERD_SOCKET too.
        crate::session::tests::with_temp_state_dir("socket-override", || {
            unsafe { std::env::set_var("NERD_SOCKET", "/tmp/x.sock") };
            assert_eq!(crate::session::socket_path(), PathBuf::from("/tmp/x.sock"));
            unsafe { std::env::remove_var("NERD_SOCKET") };
        });
    }
}

#[cfg(test)]
mod open_tests {
    use super::{take_requests, ARMED, BYE, OPEN_WINDOW, RELOAD_KEYS};

    #[test]
    fn the_marker_is_taken_out_of_the_stream_even_when_it_is_split() {
        let mut carry = Vec::new();
        let mut data = b"before".to_vec();
        data.extend_from_slice(OPEN_WINDOW);
        data.extend_from_slice(b"after");
        let r = take_requests(&data, &mut carry);
        assert_eq!((r.out.as_slice(), r.open, r.reload), (b"beforeafter".as_ref(), 1, false));
        assert!(carry.is_empty());

        // Split anywhere: the tail waits for the rest instead of reaching the terminal.
        let stopped = super::stopped_marker("SIGTERM");
        let list = super::list_remote_marker("studio");
        let open = super::open_remote_marker("studio", Some("1"));
        for marker in [OPEN_WINDOW, RELOAD_KEYS, ARMED, BYE, &stopped, &list, &open] {
            for cut in 1..marker.len() {
                let mut carry = Vec::new();
                let r = take_requests(&marker[..cut], &mut carry);
                assert_eq!(r, super::Requests::default(), "cut {cut}");
                assert_eq!(carry.as_slice(), &marker[..cut]);
                let mut rest = std::mem::take(&mut carry);
                rest.extend_from_slice(&marker[cut..]);
                let r = take_requests(&rest, &mut carry);
                assert_eq!(r.out.len(), 0, "cut {cut}");
                assert!(r.open == 1 || r.reload || r.armed || r.bye || r.stopped.is_some() || !r.list.is_empty() || !r.connect.is_empty(), "cut {cut}");
            }
        }

        // Two asks and a reload in one read, and an escape that is not ours passes through.
        let mut data = OPEN_WINDOW.to_vec();
        data.extend_from_slice(b"\x1b]2;title\x07");
        data.extend_from_slice(OPEN_WINDOW);
        data.extend_from_slice(RELOAD_KEYS);
        let mut carry = Vec::new();
        let r = take_requests(&data, &mut carry);
        assert_eq!((r.open, r.reload), (2, true));
        assert_eq!(r.out.as_slice(), b"\x1b]2;title\x07".as_ref());
    }

    #[test]
    fn remote_markers_carry_their_words() {
        let mut data = ARMED.to_vec();
        data.extend_from_slice(b"frame");
        data.extend_from_slice(&super::stopped_marker("panic"));
        data.extend_from_slice(&super::list_remote_marker("studio"));
        data.extend_from_slice(&super::open_remote_marker("studio", Some("2")));
        data.extend_from_slice(&super::open_remote_marker("mini", None));
        data.extend_from_slice(BYE);
        let r = take_requests(&data, &mut Vec::new());
        assert_eq!(r.out.as_slice(), b"frame".as_ref());
        assert!(r.armed && r.bye);
        assert_eq!(r.stopped.as_deref(), Some("panic"));
        assert_eq!(r.list, vec!["studio".to_string()]);
        assert_eq!(r.connect, vec![("studio".to_string(), Some("2".to_string())), ("mini".to_string(), None)]);
        assert_eq!(super::stopped_marker("a\x1b\\b"), super::stopped_marker("a\\b"), "an escape cannot end a marker early");

        // An unterminated `nerd;` run longer than any marker is not ours: it reaches the screen.
        let mut long = b"\x1b_nerd;".to_vec();
        long.extend(std::iter::repeat_n(b'x', 400));
        let mut carry = Vec::new();
        let r = take_requests(&long, &mut carry);
        assert_eq!(r.out, long);
        assert!(carry.is_empty());
    }
}
