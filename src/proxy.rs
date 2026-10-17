//! A plugin's or a pane's network: a sandboxed CONNECT proxy to a manifest's hosts and ports only.

use std::io::{Read as _, Write as _};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs as _};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::log::{info, warn};

const TUNNELS: usize = 16;
const HEAD_BYTES: usize = 8 * 1024;
const HEAD_TIME: Duration = Duration::from_secs(10);
const LOOKUP_TIME: Duration = Duration::from_secs(10);
const CONNECT_EACH: Duration = Duration::from_secs(5);
const CONNECT_ALL: Duration = Duration::from_secs(10);
const IDLE: Duration = Duration::from_secs(300);
/// Log lines a second for one proxy; past this they are counted, which slows a plugin rolling the log over.
const LINES: u32 = 5;

/// What a plugin may reach, from its approved manifest: `NERD_PLUGIN_NET` is `host,host;port,port`.
#[derive(Clone, Debug, PartialEq)]
pub struct Net {
    pub hosts: Vec<String>,
    pub ports: Vec<u16>,
}

impl Net {
    pub fn env(hosts: &[String], ports: &[u16]) -> String {
        format!("{};{}", hosts.join(","), ports.iter().map(u16::to_string).collect::<Vec<_>>().join(","))
    }

    pub fn parse(s: &str) -> Option<Net> {
        let (h, p) = s.split_once(';')?;
        let hosts: Vec<String> = h.split(',').filter(|h| !h.is_empty()).map(str::to_string).collect();
        let ports = p.split(',').map(|p| p.parse().ok().filter(|p| *p > 0)).collect::<Option<Vec<u16>>>()?;
        (!hosts.is_empty()).then_some(Net { hosts, ports })
    }
}

/// A manifest host that is an address: reached as written, never looked up or filtered.
fn address(h: &str) -> Option<IpAddr> {
    h.parse::<IpAddr>().ok().map(|a| a.to_canonical())
}

/// Whether `ip` is on the public internet; a name's addresses must be, so its DNS cannot point a plugin at this Mac or the LAN.
pub fn public(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(a) => public4(a),
        IpAddr::V6(a) => {
            let (s, b) = (a.segments(), a.octets());
            // Well-known NAT64 is judged by its IPv4; local-use 64:ff9b:1::/48 hides where it is, so it stays refused.
            if s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
                return public4(Ipv4Addr::new(b[12], b[13], b[14], b[15]));
            }
            let refused: [(u128, u32); 5] = [(0x2001 << 112, 32), (0x2001_0002 << 96, 48), (0x2001_0db8 << 96, 32), (0x2002 << 112, 16), (0x3fff << 112, 20)];
            let a = u128::from(a);
            a >> 125 == 1 && !refused.iter().any(|&(net, len)| a >> (128 - len) == net >> (128 - len))
        }
    }
}

fn public4(a: Ipv4Addr) -> bool {
    const REFUSED: [([u8; 4], u8); 15] = [
        ([0, 0, 0, 0], 8),
        ([10, 0, 0, 0], 8),
        ([100, 64, 0, 0], 10),
        ([127, 0, 0, 0], 8),
        ([169, 254, 0, 0], 16),
        ([172, 16, 0, 0], 12),
        ([192, 0, 0, 0], 24),
        ([192, 0, 2, 0], 24),
        ([192, 88, 99, 0], 24),
        ([192, 168, 0, 0], 16),
        ([198, 18, 0, 0], 15),
        ([198, 51, 100, 0], 24),
        ([203, 0, 113, 0], 24),
        ([224, 0, 0, 0], 4),
        ([240, 0, 0, 0], 4),
    ];
    let a = u32::from(a);
    !REFUSED.iter().any(|&(net, len)| a >> (32 - len) == u32::from_be_bytes(net) >> (32 - len))
}

/// This Mac's addresses and netmasks, read on every request: they change as networks come and go.
pub fn interfaces() -> std::io::Result<Vec<(IpAddr, IpAddr)>> {
    use nix::libc::{AF_INET, AF_INET6, freeifaddrs, getifaddrs, ifaddrs, sockaddr, sockaddr_in, sockaddr_in6};
    // A netmask may come short and without a family: read as many bytes as it has, as the address's family.
    fn ip(s: *const sockaddr, family: i32) -> Option<IpAddr> {
        if s.is_null() {
            return None;
        }
        // SAFETY: the kernel's sockaddr is at least `sa_len` bytes; at most the struct's own size is copied.
        unsafe {
            let len = usize::from((*s).sa_len);
            match family {
                AF_INET => {
                    let mut v: sockaddr_in = std::mem::zeroed();
                    std::ptr::copy_nonoverlapping(s.cast::<u8>(), (&raw mut v).cast::<u8>(), len.min(size_of::<sockaddr_in>()));
                    Some(Ipv4Addr::from(u32::from_be(v.sin_addr.s_addr)).into())
                }
                AF_INET6 => {
                    let mut v: sockaddr_in6 = std::mem::zeroed();
                    std::ptr::copy_nonoverlapping(s.cast::<u8>(), (&raw mut v).cast::<u8>(), len.min(size_of::<sockaddr_in6>()));
                    Some(Ipv6Addr::from(v.sin6_addr.s6_addr).into())
                }
                _ => None,
            }
        }
    }
    let mut head: *mut ifaddrs = std::ptr::null_mut();
    // SAFETY: the list getifaddrs fills is walked once and freed below.
    if unsafe { getifaddrs(&mut head) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut out = Vec::new();
    let mut i = head;
    while let Some(a) = unsafe { i.as_ref() } {
        let family = if a.ifa_addr.is_null() { 0 } else { i32::from(unsafe { (*a.ifa_addr).sa_family }) };
        if let (Some(addr), Some(mask)) = (ip(a.ifa_addr, family), ip(a.ifa_netmask, family)) {
            out.push((addr, mask));
        }
        i = a.ifa_next;
    }
    unsafe { freeifaddrs(head) };
    if out.is_empty() { Err(std::io::Error::other("no address of this Mac")) } else { Ok(out) }
}

/// Whether `ip` is one of this Mac's addresses or on a network one of them is on; a NAT64 address by its IPv4 too.
pub fn local(ip: IpAddr, nets: &[(IpAddr, IpAddr)]) -> bool {
    let bits = |a: IpAddr| match a {
        IpAddr::V4(v) => (4, u128::from(u32::from(v))),
        IpAddr::V6(v) => (6, u128::from(v)),
    };
    let ip = ip.to_canonical();
    let nat64 = match ip {
        IpAddr::V6(v) if v.segments()[..6] == [0x64, 0xff9b, 0, 0, 0, 0] => Some(IpAddr::V4(Ipv4Addr::from(v.octets()[12..].try_into().map(u32::from_be_bytes).unwrap_or(0)))),
        _ => None,
    };
    [Some(ip), nat64].into_iter().flatten().any(|ip| {
        let (f, x) = bits(ip);
        nets.iter().any(|&(a, m)| {
            let ((fa, a), (_, m)) = (bits(a.to_canonical()), bits(m));
            // A zero mask would take in the whole internet: such an entry counts as its own address only.
            fa == f && (x == a || (m != 0 && x & m == a & m))
        })
    })
}

pub fn lookup(name: &str, port: u16) -> std::io::Result<Vec<SocketAddr>> {
    Ok((name, port).to_socket_addrs()?.collect())
}

const PROC_PIDFDSOCKETINFO: i32 = 3;
const SOCKINFO_TCP: i32 = 2;
const INI_IPV4: u8 = 1;
/// Offsets in `struct socket_fdinfo` (sys/proc_info.h): `soi_kind`, then `in_sockinfo`'s ports, `insi_vflag`, addresses.
const KIND: usize = 256;
const INI: usize = 264;

/// Whether a process in this proxy's group (the plugin and its children, never `setsid` ones) holds `peer` → `local`.
pub fn in_group(peer: SocketAddr, local: SocketAddr) -> bool {
    let mut pids = vec![0i32; 4096];
    // SAFETY: the buffer is as long as the size passed.
    let n = unsafe { nix::libc::proc_listpgrppids(nix::unistd::getpgrp().as_raw(), pids.as_mut_ptr().cast(), (pids.len() * 4) as i32) };
    pids.truncate(n.max(0) as usize);
    holds(peer, local, pids)
}

/// Whether a process below this pane's wrapper holds `peer` → `local`: the pane's programs, whatever group they are in.
pub fn in_pane(peer: SocketAddr, local: SocketAddr) -> bool {
    holds(peer, local, crate::transport::descendants(nix::unistd::getpid().as_raw(), 4096))
}

fn holds(peer: SocketAddr, local: SocketAddr, candidates: Vec<i32>) -> bool {
    candidates.into_iter().any(|pid| pid > 0 && holds_one(pid, peer, local))
}

fn holds_one(pid: i32, peer: SocketAddr, local: SocketAddr) -> bool {
    use nix::libc::{PROC_PIDLISTFDS, PROX_FDTYPE_SOCKET, proc_fdinfo, proc_pidfdinfo, proc_pidinfo};
    let size = std::mem::size_of::<proc_fdinfo>();
    // SAFETY: a null buffer asks only for the size; then the buffer is as long as the size passed.
    let need = unsafe { proc_pidinfo(pid, PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
    if need <= 0 {
        return false;
    }
    let mut fds: Vec<proc_fdinfo> = (0..need as usize / size + 16).map(|_| proc_fdinfo { proc_fd: 0, proc_fdtype: 0 }).collect();
    let n = unsafe { proc_pidinfo(pid, PROC_PIDLISTFDS, 0, fds.as_mut_ptr().cast(), (fds.len() * size) as i32) };
    fds.truncate(n.max(0) as usize / size);
    fds.iter().filter(|f| f.proc_fdtype == PROX_FDTYPE_SOCKET as u32).any(|f| {
        let mut buf = [0u64; 128];
        // SAFETY: as above.
        let n = unsafe { proc_pidfdinfo(pid, f.proc_fd, PROC_PIDFDSOCKETINFO, buf.as_mut_ptr().cast(), 1024) };
        // SAFETY: plain bytes of a buffer we own.
        let b: &[u8; 1024] = unsafe { &*buf.as_ptr().cast() };
        n as usize >= INI + 64 && tcp_ends(b) == Some((peer, local))
    })
}

/// A TCP socket's (local, remote) ends from `socket_fdinfo` bytes.
fn tcp_ends(b: &[u8]) -> Option<(SocketAddr, SocketAddr)> {
    let int = |at: usize| i32::from_ne_bytes(b[at..at + 4].try_into().unwrap());
    if int(KIND) != SOCKINFO_TCP {
        return None;
    }
    let v4 = b[INI + 24] & INI_IPV4 != 0;
    let ip = |at: usize| -> IpAddr {
        if v4 { Ipv4Addr::from(<[u8; 4]>::try_from(&b[at + 12..at + 16]).unwrap()).into() } else { Ipv6Addr::from(<[u8; 16]>::try_from(&b[at..at + 16]).unwrap()).to_canonical() }
    };
    let port = |at: usize| u16::from_be(int(at) as u16);
    Some((SocketAddr::new(ip(INI + 48), port(INI + 4)), SocketAddr::new(ip(INI + 32), port(INI))))
}

/// Both loopbacks on one port: Seatbelt's `localhost:<port>` admits that port on every address of this Mac.
pub fn bind() -> std::io::Result<(TcpListener, TcpListener)> {
    let mut last = None;
    for _ in 0..8 {
        let v4 = TcpListener::bind("127.0.0.1:0")?;
        match TcpListener::bind(("::1", v4.local_addr()?.port())) {
            Ok(v6) => return Ok((v4, v6)),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("no port free on both loopbacks")))
}

/// The proxy's own Seatbelt profile: reads as a plugin does, writes only the log, reaches only the manifest's ports.
pub fn profile(net: &Net, port: u16) -> Result<String, String> {
    use crate::plugins::{canon, quote};
    let home = crate::paths::home().unwrap_or_default();
    if !home.is_absolute() || home == std::path::Path::new("/") {
        return Err("nerd has no home folder, so it cannot keep the proxy out of ~/.ssh".into());
    }
    let secret = crate::plugins::secret_filters(&home)?;
    let log = canon(&crate::session::state_dir().join("daemon.log"));
    let ports: String = net.ports.iter().map(|p| format!(" (remote ip \"*:{p}\")")).collect();
    Ok(format!(
        "(version 1)\n(deny default)\n(allow file-read*)\n(deny file-read-data{secret})\n{}\
         (allow file-write* (literal {}) (literal {}))\n\
         (allow network-outbound{ports})\n(allow network-outbound (literal \"/private/var/run/mDNSResponder\"))\n\
         (allow network-inbound (local ip \"localhost:{port}\"))\n\
         (allow sysctl-read)\n(allow signal (target self))\n",
        crate::plugins::TERMINALS,
        quote(&log)?,
        quote(&log.with_extension("log.old"))?,
    ))
}

pub struct Proxy {
    /// `plugin <name>` or `pane <id>`, as its log lines start.
    pub owner: String,
    pub net: Net,
    pub resolve: fn(&str, u16) -> std::io::Result<Vec<SocketAddr>>,
    pub public: fn(IpAddr) -> bool,
    pub interfaces: fn() -> std::io::Result<Vec<(IpAddr, IpAddr)>>,
    pub peer_ok: fn(SocketAddr, SocketAddr) -> bool,
    /// A tunnel closes when neither direction has carried a byte for this long.
    pub idle: Duration,
    live: AtomicUsize,
    /// (this second's start, lines in it, lines not written: allowed, closed, refused or failed)
    lines: Mutex<(Instant, u32, [u64; 3])>,
}

impl Proxy {
    pub fn new(owner: String, net: Net) -> Proxy {
        Proxy { owner, net, resolve: lookup, public, peer_ok: in_group, interfaces, idle: IDLE, live: AtomicUsize::new(0), lines: Mutex::new((Instant::now(), 0, [0; 3])) }
    }

    /// `kind`: 0 allowed, 1 closed, 2 refused or failed (a warning).
    fn line(&self, kind: usize, text: &str) {
        let mut l = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        if l.0.elapsed() >= Duration::from_secs(1) {
            (l.0, l.1) = (Instant::now(), 0);
        }
        l.1 += 1;
        if l.1 > LINES {
            l.2[kind] += 1;
            return;
        }
        let held = std::mem::take(&mut l.2);
        drop(l);
        let text = format!("{} proxy: {text}{}", self.owner, held_words(held).map(|h| format!(" ({h} before)")).unwrap_or_default());
        if kind == 2 { warn(&text) } else { info(&text) }
    }

    /// Lines a flood kept from the log, written before the proxy goes.
    pub fn flush(&self) {
        let held = std::mem::take(&mut self.lines.lock().unwrap_or_else(|e| e.into_inner()).2);
        if let Some(h) = held_words(held) {
            warn(&format!("{} proxy: {h}", self.owner));
        }
    }

    fn refuse(&self, c: &TcpStream, status: &str, what: &str, why: &str) {
        self.line(2, &format!("{what} {}: {why}", if status.starts_with("502") { "failed" } else { "refused" }));
        answer(c, status);
    }
}

fn answer(mut c: &TcpStream, status: &str) {
    let _ = c.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes());
    let _ = c.shutdown(Shutdown::Write);
}

fn held_words(h: [u64; 3]) -> Option<String> {
    let n: u64 = h.iter().sum();
    (n > 0).then(|| format!("{n} more lines not written: {} allowed, {} closed, {} refused or failed", h[0], h[1], h[2]))
}

/// One of the `TUNNELS`, given back when dropped; a lookup holds it until it returns.
struct Slot(Arc<Proxy>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.live.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Accepts until the process ends; the peer check comes before a slot or a thread is taken.
pub fn serve(listener: TcpListener, p: Arc<Proxy>) {
    loop {
        let c = match listener.accept() {
            Ok((c, _)) => c,
            Err(_) => {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
        };
        let (Ok(peer), Ok(local)) = (c.peer_addr(), c.local_addr()) else { continue };
        // Outsiders get a budget of their own: another plugin's flood must not crowd out this plugin's lines.
        if !(p.peer_ok)(peer, local) {
            crate::log::warn_limited("plugin proxy outsider", &format!("{} proxy: a connection from {peer} refused: peer outside {}", p.owner, p.owner));
            answer(&c, "403 Forbidden");
            continue;
        }
        if p.live.fetch_add(1, Ordering::SeqCst) >= TUNNELS {
            p.live.fetch_sub(1, Ordering::SeqCst);
            p.refuse(&c, "503 Service Unavailable", &format!("a connection from {peer}"), &format!("{TUNNELS} tunnels already open"));
            continue;
        }
        let slot = Slot(p.clone());
        let p = p.clone();
        let _ = std::thread::Builder::new().spawn(move || tunnel(&p, c, slot));
    }
}

enum Host {
    Name(String),
    Addr(IpAddr),
}

/// `host:port` of a CONNECT: `[v6]`, dotted IPv4, or a name, lowercased and without one trailing dot.
fn target(t: &str) -> Option<(Host, u16)> {
    let (h, port) = t.rsplit_once(':')?;
    let port = Some(port).filter(|p| !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit()))?.parse::<u16>().ok().filter(|p| *p > 0)?;
    if let Some(v6) = h.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        return Some((Host::Addr(v6.parse::<Ipv6Addr>().ok()?.to_canonical()), port));
    }
    if let Ok(a) = h.parse::<Ipv4Addr>() {
        return Some((Host::Addr(a.into()), port));
    }
    Some((Host::Name(h.strip_suffix('.').unwrap_or(h).to_ascii_lowercase()), port))
}

/// The request head and whatever came after it, within `HEAD_TIME` in all and `HEAD_BYTES`.
fn head(c: &mut TcpStream) -> Result<(String, Vec<u8>), &'static str> {
    let end = Instant::now() + HEAD_TIME;
    let mut buf = Vec::new();
    loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let rest = buf.split_off(i + 4);
            return String::from_utf8(buf).map(|h| (h, rest)).map_err(|_| "a request head that is not UTF-8");
        }
        if buf.len() >= HEAD_BYTES {
            return Err("a request head over 8 KiB");
        }
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() || c.set_read_timeout(Some(left)).is_err() {
            return Err("no request head within 10 seconds");
        }
        let mut chunk = [0u8; 1024];
        let room = (HEAD_BYTES - buf.len()).min(chunk.len());
        match c.read(&mut chunk[..room]) {
            Ok(0) => return Err("closed before a request"),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => return Err("no request head within 10 seconds"),
            Err(_) => return Err("closed before a request"),
        }
    }
}

/// The plugin's own words as a log line shows them: cut at 100 characters.
fn short(s: &str) -> String {
    match s.char_indices().nth(100) {
        Some((at, _)) => format!("{}… ({} bytes)", &s[..at], s.len()),
        None => s.to_string(),
    }
}

fn tunnel(p: &Arc<Proxy>, mut c: TcpStream, slot: Slot) {
    let peer = c.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    let from = format!("a connection from {peer}");
    let (head, rest) = match head(&mut c) {
        Ok(h) => h,
        Err(why) => return p.refuse(&c, "400 Bad Request", &from, why),
    };
    let line = head.lines().next().unwrap_or_default();
    let mut words = line.split(' ');
    let (Some("CONNECT"), Some(t), Some(v), None) = (words.next(), words.next(), words.next(), words.next()) else {
        return p.refuse(&c, "400 Bad Request", &from, &format!("{:?} is not a CONNECT request", short(line)));
    };
    let Some((host, port)) = target(t).filter(|_| v.len() == 8 && v.starts_with("HTTP/1.")) else {
        return p.refuse(&c, "400 Bad Request", &from, &format!("{:?} is not a CONNECT request", short(line)));
    };
    let t = short(t);
    let t = t.as_str();
    let listed = match &host {
        Host::Name(n) => p.net.hosts.iter().any(|h| h == n && address(h).is_none()),
        Host::Addr(a) => p.net.hosts.iter().any(|h| address(h) == Some(*a)),
    };
    if !listed || !p.net.ports.contains(&port) {
        return p.refuse(&c, "403 Forbidden", t, "not in the manifest");
    }
    let (addrs, slot) = match host {
        Host::Addr(a) => (vec![SocketAddr::new(a, port)], slot),
        Host::Name(n) => {
            let (tx, rx) = std::sync::mpsc::channel();
            let resolve = p.resolve;
            // The slot goes with the lookup: one that outlives its deadline still counts against the 16.
            let spawned = std::thread::Builder::new().spawn(move || {
                let r = resolve(&n, port);
                let _ = tx.send((r, slot));
            });
            if spawned.is_err() {
                return p.refuse(&c, "503 Service Unavailable", t, "no thread for the name lookup");
            }
            let (r, slot) = match rx.recv_timeout(LOOKUP_TIME) {
                Ok(x) => x,
                Err(_) => return p.refuse(&c, "502 Bad Gateway", t, "no answer from the name lookup within 10 seconds"),
            };
            let all = match r {
                Ok(all) if !all.is_empty() => all,
                Ok(_) => return p.refuse(&c, "502 Bad Gateway", t, "the name has no address"),
                Err(e) => return p.refuse(&c, "502 Bad Gateway", t, &format!("name lookup: {e}")),
            };
            let Ok(nets) = (p.interfaces)() else { return p.refuse(&c, "403 Forbidden", t, "this Mac's own addresses could not be read") };
            let (ok, not): (Vec<SocketAddr>, Vec<SocketAddr>) = all.into_iter().partition(|a| (p.public)(a.ip()) && !local(a.ip(), &nets));
            if ok.is_empty() {
                let not = not.iter().map(|a| a.ip().to_string()).collect::<Vec<_>>().join(", ");
                return p.refuse(&c, "403 Forbidden", t, &format!("non-public address ({not})"));
            }
            (ok, slot)
        }
    };
    let start = Instant::now();
    let mut up = Err(std::io::Error::other("no address"));
    for a in &addrs {
        let left = CONNECT_ALL.saturating_sub(start.elapsed());
        if left.is_zero() {
            break;
        }
        up = TcpStream::connect_timeout(a, left.min(CONNECT_EACH)).map(|s| (s, *a));
        if up.is_ok() {
            break;
        }
    }
    let (up, at) = match up {
        Ok(x) => x,
        Err(e) => return p.refuse(&c, "502 Bad Gateway", t, &format!("no connection: {e}")),
    };
    let last = Arc::new(Mutex::new(Instant::now()));
    let (idle, l2) = (p.idle, last.clone());
    // What came after the head goes first; a failed write ends the relays at once.
    let _ = (&up).write_all(&rest);
    let other = match c.try_clone().and_then(|c2| Ok((c2, up.try_clone()?))).and_then(|(c2, up2)| std::thread::Builder::new().spawn(move || relay(c2, up2, idle, &l2))) {
        Ok(t) => t,
        Err(e) => return p.refuse(&c, "503 Service Unavailable", t, &format!("no thread for the tunnel: {e}")),
    };
    p.line(0, &format!("{t} allowed ({})", at.ip()));
    let _ = c.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n");
    let received = relay(up, c, idle, &last);
    let sent = other.join().unwrap_or(0) + rest.len() as u64;
    p.line(1, &format!("{t} closed, {sent} sent, {received} received"));
    drop(slot);
}

/// Copies one direction: an end passes on as a half close; an error, or `idle` with no byte either way (`last`), ends both.
fn relay(mut from: TcpStream, mut to: TcpStream, idle: Duration, last: &Mutex<Instant>) -> u64 {
    let mut buf = [0u8; 16 * 1024];
    let mut n = 0u64;
    // After the other side's half close nothing else would end a write the plugin never reads.
    let _ = to.set_write_timeout(Some(idle));
    loop {
        let left = idle.saturating_sub(last.lock().unwrap_or_else(|e| e.into_inner()).elapsed());
        if left.is_zero() || from.set_read_timeout(Some(left)).is_err() {
            break;
        }
        match from.read(&mut buf) {
            Ok(0) => {
                let _ = to.shutdown(Shutdown::Write);
                return n;
            }
            Ok(k) => {
                *last.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
                if to.write_all(&buf[..k]).is_err() {
                    break;
                }
                n += k as u64;
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => break,
        }
    }
    let _ = (from.shutdown(Shutdown::Both), to.shutdown(Shutdown::Both));
    n
}

/// The wrapper after the fork: serves until `child` ends, then leaves as it did. A plugin's leads the group `child` is in;
/// a pane's (`pane`) is above the pane's program, whose wrapper set its signals.
pub fn run(child: nix::unistd::Pid, net: Net, listeners: (TcpListener, TcpListener), pane: Option<usize>) -> std::io::Result<()> {
    use nix::sys::signal::{SigHandler, Signal, kill, raise, signal};
    use nix::sys::wait::{WaitStatus, waitpid};
    crate::log::set_role("proxy");
    let (owner, program, stopped) = match pane {
        Some(id) => (format!("pane {id}"), "the pane's program", "the pane's program stopped"),
        None => (format!("plugin {}", std::env::var("NERD_PLUGIN").unwrap_or_else(|_| "?".into())), "the plugin", "plugin stopped"),
    };
    if pane.is_none() {
        // SAFETY: setting dispositions installs no handler. SIGTERM and SIGHUP reach the plugin through the group; the proxy waits for it.
        for s in [Signal::SIGPIPE, Signal::SIGTERM, Signal::SIGHUP] {
            let _ = unsafe { signal(s, SigHandler::SigIgn) };
        }
        // The peer check trusts the group, so it must be this process's own.
        if nix::unistd::getpgrp() != nix::unistd::getpid() {
            let _ = kill(child, Signal::SIGKILL);
            crate::log::error(&format!("{owner} proxy: not the leader of its own process group, plugin stopped"));
            std::process::exit(1);
        }
    }
    let port = listeners.0.local_addr()?.port();
    // A pane's wrapper passes a hangup on to the pane's program, which is in a group of its own.
    let own = if pane.is_some() { "(allow signal (target children))\n" } else { "" };
    if let Err(e) = profile(&net, port).and_then(|p| crate::sandbox::apply_text(&(p + own))) {
        let _ = kill(child, Signal::SIGKILL);
        crate::log::error(&format!("{owner} proxy: sandbox not applied, {stopped}: {e}"));
        std::process::exit(1);
    }
    let mut p = Proxy::new(owner, net);
    if pane.is_some() {
        p.peer_ok = in_pane;
    }
    let p = Arc::new(p);
    for l in [listeners.0, listeners.1] {
        let p = p.clone();
        std::thread::spawn(move || serve(l, p));
    }
    let status = loop {
        match waitpid(child, None) {
            Err(nix::errno::Errno::EINTR) => continue,
            other => break other,
        }
    };
    p.flush();
    match status {
        Ok(WaitStatus::Exited(_, code)) => std::process::exit(code),
        // Re-raised, a crash signal would make macOS file a crash report against nerd.
        Ok(WaitStatus::Signaled(_, sig, _)) if dumps_core(sig) => {
            crate::log::warn(&format!("{} proxy: {program} died by {}", p.owner, sig.as_str()));
            std::process::exit(128 + sig as i32)
        }
        Ok(WaitStatus::Signaled(_, sig, _)) => {
            // SAFETY: as above.
            let _ = unsafe { signal(sig, SigHandler::SigDfl) };
            let _ = raise(sig);
            std::process::exit(128 + sig as i32)
        }
        _ => std::process::exit(1),
    }
}

fn dumps_core(sig: nix::sys::signal::Signal) -> bool {
    use nix::sys::signal::Signal;
    matches!(sig, Signal::SIGSEGV | Signal::SIGBUS | Signal::SIGILL | Signal::SIGABRT | Signal::SIGFPE | Signal::SIGTRAP | Signal::SIGSYS | Signal::SIGQUIT | Signal::SIGEMT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proxy(hosts: &[&str], ports: &[u16]) -> Proxy {
        let mut p = Proxy::new("plugin t".into(), Net { hosts: hosts.iter().map(|h| h.to_string()).collect(), ports: ports.to_vec() });
        p.resolve = |name, port| if name == "echo.example" { Ok(vec![SocketAddr::from(([127, 0, 0, 1], port))]) } else { Ok(vec![SocketAddr::from(([10, 0, 0, 1], port))]) };
        p.public = |_| true;
        p.interfaces = || Ok(Vec::new());
        p.peer_ok = |_, _| true;
        p
    }

    /// A proxy on its own port; the returned closure opens a connection to it and sends `req`.
    fn start(p: Proxy) -> impl Fn(&str) -> TcpStream {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let at = l.local_addr().unwrap();
        let p = Arc::new(p);
        std::thread::spawn(move || serve(l, p));
        move |req: &str| {
            let mut c = TcpStream::connect(at).unwrap();
            c.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
            c.write_all(req.as_bytes()).unwrap();
            c
        }
    }

    fn status(c: &mut TcpStream) -> String {
        let mut got = Vec::new();
        let mut b = [0u8; 1];
        while !got.ends_with(b"\r\n\r\n") && c.read(&mut b).is_ok_and(|n| n == 1) {
            got.push(b[0]);
        }
        String::from_utf8_lossy(&got).lines().next().unwrap_or_default().to_string()
    }

    fn echo() -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for c in l.incoming().flatten() {
                std::thread::spawn(move || {
                    let _ = std::io::copy(&mut &c, &mut &c);
                });
            }
        });
        port
    }

    #[test]
    fn an_allowed_host_and_port_carry_bytes_and_everything_else_is_refused() {
        let e = echo();
        let conn = start(proxy(&["echo.example", "127.0.0.1"], &[e]));
        for req in [format!("CONNECT echo.example:{e} HTTP/1.1\r\nHost: x\r\n\r\nhello"), format!("CONNECT ECHO.example.:{e} HTTP/1.0\r\n\r\nhello"), format!("CONNECT 127.0.0.1:{e} HTTP/1.1\r\n\r\nhello")] {
            let mut c = conn(&req);
            assert_eq!(status(&mut c), "HTTP/1.1 200 Connection established", "{req}");
            let mut got = [0u8; 5];
            c.read_exact(&mut got).unwrap();
            assert_eq!(&got, b"hello", "the bytes after the head go through: {req}");
            c.write_all(b" again").unwrap();
            let mut got = [0u8; 6];
            c.read_exact(&mut got).unwrap();
            assert_eq!(&got, b" again");
        }
        for (req, want) in [
            (format!("CONNECT other.example:{e} HTTP/1.1\r\n\r\n"), "403"),
            (format!("CONNECT echo.example:{} HTTP/1.1\r\n\r\n", e.wrapping_add(1)), "403"),
            (format!("CONNECT echo.example.evil:{e} HTTP/1.1\r\n\r\n"), "403"),
            (format!("CONNECT [::1]:{e} HTTP/1.1\r\n\r\n"), "403"),
            (format!("CONNECT 127.0.0.2:{e} HTTP/1.1\r\n\r\n"), "403"),
            (format!("CONNECT echo.example:+{e} HTTP/1.1\r\n\r\n"), "400"),
            (format!("GET http://echo.example:{e}/ HTTP/1.1\r\n\r\n"), "400"),
            (format!("CONNECT echo.example:{e} HTTP/2.0\r\n\r\n"), "400"),
            // Exactly the limit, so nothing is left unread to turn the close into a reset.
            ("x".repeat(HEAD_BYTES), "400"),
        ] {
            assert!(status(&mut conn(&req)).starts_with(&format!("HTTP/1.1 {want}")), "{}", &req[..req.len().min(60)]);
        }
    }

    #[test]
    fn a_name_is_refused_unless_every_address_it_reaches_is_public_and_an_address_is_trusted_as_written() {
        let e = echo();
        let mut p = proxy(&["lan.example", "echo.example", "127.0.0.1"], &[e]);
        p.public = public;
        let conn = start(p);
        assert!(status(&mut conn(&format!("CONNECT lan.example:{e} HTTP/1.1\r\n\r\n"))).starts_with("HTTP/1.1 403"), "a name pointing at the LAN");
        assert!(status(&mut conn(&format!("CONNECT echo.example:{e} HTTP/1.1\r\n\r\n"))).starts_with("HTTP/1.1 403"), "a name pointing at this Mac");
        assert!(status(&mut conn(&format!("CONNECT 127.0.0.1:{e} HTTP/1.1\r\n\r\n"))).starts_with("HTTP/1.1 200"), "the address the user approved, no lookup");
    }

    #[test]
    fn a_one_way_download_outlives_the_idle_time_and_a_silent_tunnel_does_not() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            for _ in 0..10 {
                let _ = c.write_all(b"x");
                std::thread::sleep(Duration::from_millis(300));
            }
            std::thread::sleep(Duration::from_secs(30));
        });
        let mut p = proxy(&["127.0.0.1"], &[port]);
        p.idle = Duration::from_secs(1);
        let mut c = start(p)(&format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n"));
        assert!(status(&mut c).starts_with("HTTP/1.1 200"));
        let t = Instant::now();
        let mut got = Vec::new();
        let _ = c.read_to_end(&mut got);
        assert_eq!(got, b"xxxxxxxxxx", "the client never spoke, yet the download went on past 1 s");
        assert!((3..6).contains(&t.elapsed().as_secs()), "closed a second after the last byte: {:?}", t.elapsed());
    }

    #[test]
    fn the_seventeenth_connection_and_a_stranger_are_refused() {
        let conn = start(proxy(&["echo.example"], &[1]));
        let held: Vec<TcpStream> = (0..TUNNELS).map(|_| conn("")).collect();
        assert!(status(&mut conn("")).starts_with("HTTP/1.1 503"));
        drop(held);
        let mut p = proxy(&["echo.example"], &[1]);
        p.peer_ok = |_, _| false;
        assert!(status(&mut start(p)("")).starts_with("HTTP/1.1 403"));
    }

    #[test]
    fn a_head_sent_a_byte_at_a_time_is_cut_off_after_ten_seconds() {
        let conn = start(proxy(&["echo.example"], &[1]));
        let mut c = conn("C");
        let mut w = c.try_clone().unwrap();
        std::thread::spawn(move || {
            while w.write_all(b"O").is_ok() {
                std::thread::sleep(Duration::from_millis(500));
            }
        });
        let t = Instant::now();
        assert!(status(&mut c).starts_with("HTTP/1.1 400"));
        assert!((9..12).contains(&t.elapsed().as_secs()), "{:?}", t.elapsed());
    }

    #[test]
    fn a_burst_of_tunnels_writes_a_few_lines_and_counts_the_rest() {
        crate::session::tests::with_temp_state_dir("proxy-log", || {
            let mut p = proxy(&["wttr.in"], &[443]);
            p.owner = "plugin burst".into();
            p.line(2, "example.com:443 refused: not in the manifest");
            for _ in 0..10 {
                p.line(0, "wttr.in:443 allowed (flood)");
                p.line(1, "wttr.in:443 closed (flood)");
                p.line(2, "x:1 refused: flood");
            }
            p.flush();
            let log = std::fs::read_to_string(crate::session::state_dir().join("daemon.log")).unwrap();
            assert!(log.contains("plugin burst proxy: example.com:443 refused: not in the manifest"), "{log}");
            assert_eq!(log.lines().filter(|l| l.contains("flood")).count(), LINES as usize - 1, "{log}");
            // Written: allowed, closed, refused, allowed; held: 8 allowed, 9 closed, 9 refused.
            assert!(log.contains("plugin burst proxy: 26 more lines not written: 8 allowed, 9 closed, 9 refused or failed"), "{log}");
            assert_eq!(log.lines().filter(|l| l.contains("plugin burst proxy:")).count(), LINES as usize + 1, "{log}");
        });
    }

    #[test]
    fn a_plugins_long_words_and_a_strangers_flood_leave_its_own_lines_short_and_written() {
        crate::session::tests::with_temp_state_dir("proxy-short", || {
            let mut p = proxy(&["wttr.in"], &[443]);
            p.owner = "plugin words".into();
            let conn = start(p);
            for req in [format!("CONNECT {}.example:443 HTTP/1.1\r\n\r\n", "a".repeat(4096)), format!("GET /{} HTTP/1.1\r\n\r\n", "b".repeat(4096))] {
                assert!(status(&mut conn(&req)).starts_with("HTTP/1.1 4"));
            }
            let mut p = proxy(&["wttr.in"], &[443]);
            p.owner = "plugin victim".into();
            p.peer_ok = |_, _| false;
            let p = Arc::new(p);
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let at = l.local_addr().unwrap();
            let q = p.clone();
            std::thread::spawn(move || serve(l, q));
            for _ in 0..30 {
                let mut c = TcpStream::connect(at).unwrap();
                assert!(status(&mut c).starts_with("HTTP/1.1 403"));
            }
            for _ in 0..LINES {
                p.line(0, "wttr.in:443 allowed (mine)");
            }
            let log = std::fs::read_to_string(crate::session::state_dir().join("daemon.log")).unwrap();
            let words: Vec<&str> = log.lines().filter(|l| l.contains("plugin words proxy:")).collect();
            assert!(words.len() == 2 && words.iter().all(|l| l.len() < 300 && l.contains("… (")), "{words:#?}");
            assert_eq!(log.matches("wttr.in:443 allowed (mine)").count(), LINES as usize, "{log}");
        });
    }

    #[test]
    fn a_tunnel_whose_plugin_stops_reading_after_its_request_ends_after_the_idle_time() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let _ = c.read_to_end(&mut Vec::new());
            while c.write_all(&[b'x'; 64 * 1024]).is_ok() {}
            let _ = tx.send(());
        });
        let mut p = proxy(&["127.0.0.1"], &[port]);
        p.idle = Duration::from_secs(1);
        let mut c = start(p)(&format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n"));
        assert!(status(&mut c).starts_with("HTTP/1.1 200"));
        c.shutdown(Shutdown::Write).unwrap();
        assert!(rx.recv_timeout(Duration::from_secs(4)).is_ok(), "the tunnel did not end soon after its idle second");
        drop(c);
    }

    #[test]
    fn a_name_that_answers_with_an_address_of_this_mac_or_its_network_is_refused() {
        let nets = [("192.168.0.79".parse().unwrap(), "255.255.255.0".parse().unwrap()), ("2a02:8308:30f:2200::fcd7".parse().unwrap(), "ffff:ffff:ffff:ffff::".parse().unwrap()), ("100.66.105.6".parse().unwrap(), "255.255.255.255".parse().unwrap()), ("9.9.9.9".parse().unwrap(), "0.0.0.0".parse().unwrap())];
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        for yes in ["192.168.0.79", "192.168.0.1", "2a02:8308:30f:2200::1", "2a02:8308:30f:2200:c2f:d4a4:5b38:3c57", "::ffff:192.168.0.5", "100.66.105.6", "64:ff9b::c0a8:1", "9.9.9.9"] {
            assert!(local(ip(yes), &nets), "{yes}");
        }
        for no in ["192.168.1.1", "2a02:8308:30f:2201::1", "100.66.105.7", "93.184.215.14", "9.9.9.10"] {
            assert!(!local(ip(no), &nets), "{no}");
        }
        let e = echo();
        let mut p = proxy(&["echo.example"], &[e]);
        p.interfaces = || Ok(vec![("127.0.0.1".parse().unwrap(), "255.0.0.0".parse().unwrap())]);
        assert!(status(&mut start(p)(&format!("CONNECT echo.example:{e} HTTP/1.1\r\n\r\n"))).starts_with("HTTP/1.1 403"), "public by the table, but this Mac's");
        let mut p = proxy(&["echo.example"], &[e]);
        p.interfaces = || Err(std::io::Error::other("no"));
        assert!(status(&mut start(p)(&format!("CONNECT echo.example:{e} HTTP/1.1\r\n\r\n"))).starts_with("HTTP/1.1 403"), "unknown addresses of this Mac refuse");
        let real = interfaces().unwrap();
        assert!(real.iter().any(|(a, _)| *a == ip("127.0.0.1")) && real.iter().any(|(a, m)| *a == ip("::1") && *m == ip("ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff")), "{real:?}");
    }

    #[test]
    fn a_client_that_ends_its_request_still_reads_the_answer() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut req = Vec::new();
            c.read_to_end(&mut req).unwrap();
            c.write_all(&[b"got ".as_slice(), &req].concat()).unwrap();
        });
        let mut c = start(proxy(&["127.0.0.1"], &[port]))(&format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n"));
        assert!(status(&mut c).starts_with("HTTP/1.1 200"));
        c.write_all(b"question").unwrap();
        c.shutdown(Shutdown::Write).unwrap();
        let mut got = String::new();
        c.read_to_string(&mut got).unwrap();
        assert_eq!(got, "got question");
    }

    #[test]
    fn only_a_crash_signal_is_kept_from_being_raised_again() {
        use nix::sys::signal::Signal::*;
        assert!([SIGSEGV, SIGBUS, SIGILL, SIGABRT, SIGFPE, SIGTRAP, SIGSYS].into_iter().all(dumps_core));
        assert!(![SIGTERM, SIGKILL, SIGINT, SIGHUP, SIGUSR1, SIGPIPE].into_iter().any(dumps_core));
    }

    #[test]
    fn only_public_unicast_passes_the_address_filter() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        for ok in ["93.184.215.14", "1.1.1.1", "8.8.8.8", "2606:4700::1111", "2a00:1450::1", "::ffff:93.184.215.14", "64:ff9b::5db8:d70e"] {
            assert!(public(ip(ok)), "{ok}");
        }
        for no in [
            "0.0.0.1",
            "10.1.2.3",
            "100.64.0.1",
            "100.127.255.255",
            "127.0.0.1",
            "169.254.169.254",
            "172.16.0.1",
            "172.31.255.255",
            "192.0.0.8",
            "192.0.2.1",
            "192.88.99.1",
            "192.168.1.5",
            "198.18.0.1",
            "198.19.255.255",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "239.255.255.250",
            "240.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "fe80::1",
            "fc00::1",
            "fd12::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::127.0.0.1",
            "64:ff9b::a00:1",
            "64:ff9b::7f00:1",
            "64:ff9b::a9fe:a9fe",
            "64:ff9b:1:a00:1:0:5db8:d70e",
            "64:ff9b:1::a00:1",
            "64:ff9b:1::5db8:d70e",
            "2001::1",
            "2001:0:4136:e378::1",
            "2001:db8::1",
            "2001:2::1",
            "2001:2:0:ffff::1",
            "2002:a00:1::1",
            "2002:5db8:d70e::1",
            "3fff::1",
            "3fff:fff::1",
            "4000::1",
        ] {
            assert!(!public(ip(no)), "{no}");
        }
        assert!(public(ip("3fff:1000::1")) && public(ip("2001:1::1")) && public(ip("2001:2:1::1")) && public(ip("192.88.100.1")), "the refused prefixes end where they end");
    }

    #[test]
    fn the_peer_check_finds_sockets_of_this_group_only() {
        for (bind, host) in [("127.0.0.1:0", "127.0.0.1"), ("[::1]:0", "::1")] {
            let l = TcpListener::bind(bind).unwrap();
            let port = l.local_addr().unwrap().port().to_string();
            let _mine = TcpStream::connect(l.local_addr().unwrap()).unwrap();
            let (c, _) = l.accept().unwrap();
            assert!(in_group(c.peer_addr().unwrap(), c.local_addr().unwrap()), "this process is in its own group");
            use std::os::unix::process::CommandExt as _;
            let mut nc = std::process::Command::new("/usr/bin/nc").args([host, &port]).stdin(std::process::Stdio::piped()).process_group(0).spawn().unwrap();
            let (c, _) = l.accept().unwrap();
            assert!(!in_group(c.peer_addr().unwrap(), c.local_addr().unwrap()), "a process in another group");
            let _ = nc.kill();
            let _ = nc.wait();
        }
    }

    #[test]
    fn a_pane_proxy_admits_its_descendants_only() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port().to_string();
        let _mine = TcpStream::connect(l.local_addr().unwrap()).unwrap();
        let (c, _) = l.accept().unwrap();
        assert!(!in_pane(c.peer_addr().unwrap(), c.local_addr().unwrap()), "the proxy itself is no program of its pane");
        use std::os::unix::process::CommandExt as _;
        // In a group of its own, as a pane's shell puts its jobs: still below the wrapper.
        let mut nc = std::process::Command::new("/usr/bin/nc").args(["127.0.0.1", &port]).stdin(std::process::Stdio::piped()).process_group(0).spawn().unwrap();
        let (c, _) = l.accept().unwrap();
        assert!(in_pane(c.peer_addr().unwrap(), c.local_addr().unwrap()), "a process below it");
        let _ = nc.kill();
        let _ = nc.wait();
        // Left by its parent at once, so launchd adopts it.
        let out = std::process::Command::new("/bin/sh").args(["-c", &format!("/usr/bin/nc 127.0.0.1 {port} </dev/zero >/dev/null 2>&1 & echo $!")]).output().unwrap();
        let orphan: i32 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
        let (c, _) = l.accept().unwrap();
        assert!(!in_pane(c.peer_addr().unwrap(), c.local_addr().unwrap()), "a process that left the pane's tree");
        let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(orphan), nix::sys::signal::Signal::SIGKILL);
    }

    #[test]
    fn the_net_variable_carries_the_manifest() {
        let n = Net { hosts: vec!["wttr.in".into(), "192.168.1.5".into(), "::1".into()], ports: vec![80, 443] };
        assert_eq!(Net::env(&n.hosts, &n.ports), "wttr.in,192.168.1.5,::1;80,443");
        assert_eq!(Net::parse(&Net::env(&n.hosts, &n.ports)), Some(n));
        assert_eq!(Net::parse("wttr.in;0"), None);
        assert_eq!(Net::parse(";443"), None);
    }
}
