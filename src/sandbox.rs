//! The daemon's sandbox: Seatbelt with no network, inherited by every child, so anything network-bound must launch before `apply()`.

use std::ffi::{c_char, c_int, CString};

/// `(deny network*)` also blocks unix-domain sockets, so they're allowed back explicitly or the client couldn't connect;
/// mDNSResponder's socket stays denied by name since `getaddrinfo` uses it to reach DNS.
pub const PROFILE: &str = concat!(
    "(version 1)\n",
    "(allow default)\n",
    "(deny network*)\n",
    "(allow network* (local unix-socket) (remote unix-socket))\n",
    "(deny network-outbound (literal \"/private/var/run/mDNSResponder\"))\n",
);

unsafe extern "C" {
    fn sandbox_init(profile: *const c_char, flags: u64, errorbuf: *mut *mut c_char) -> c_int;
    fn sandbox_free_error(errorbuf: *mut c_char);
}

pub fn apply() -> Result<(), String> {
    apply_text(PROFILE)
}

/// Seatbelt for this process and everything it execs, from `profile`.
pub fn apply_text(profile: &str) -> Result<(), String> {
    let profile = CString::new(profile).map_err(|e| e.to_string())?;
    let mut err: *mut c_char = std::ptr::null_mut();
    let rc = unsafe { sandbox_init(profile.as_ptr(), 0, &mut err) };
    if rc == 0 {
        return Ok(());
    }
    let msg = if err.is_null() {
        format!("sandbox_init returned {rc}")
    } else {
        let s = unsafe { std::ffi::CStr::from_ptr(err) }.to_string_lossy().into_owned();
        unsafe { sandbox_free_error(err) };
        s
    };
    Err(msg)
}

/// Fails unless the profile applied and the system refused both probes, so an offline machine cannot pass it.
pub fn selftest() -> std::io::Result<()> {
    // Reached before the profile, so a refusal after it can only be the sandbox's.
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    std::net::TcpStream::connect(addr)?;
    let udp = std::net::UdpSocket::bind("0.0.0.0:0")?;
    // TEST-NET-1 (RFC 5737): routed nowhere, so without the sandbox the answer is a timeout or unreachable, never EPERM.
    let far: std::net::SocketAddr = "192.0.2.1:80".parse().unwrap();
    let mut held = match apply() {
        Ok(()) => {
            println!("profile applied");
            true
        }
        Err(e) => {
            println!("profile NOT applied: {e}");
            false
        }
    };
    held &= refused(&format!("tcp {addr}"), std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(2)).map(drop));
    held &= refused(&format!("tcp {far}"), std::net::TcpStream::connect_timeout(&far, std::time::Duration::from_secs(2)).map(drop));
    held &= refused(&format!("udp {far}"), udp.send_to(b"nerd", far).map(drop));
    held &= refused("dns (mDNSResponder)", std::os::unix::net::UnixStream::connect("/private/var/run/mDNSResponder").map(drop));
    let dir = std::env::temp_dir().join(format!("nerd-selftest-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let sock = dir.join("s.sock");
    match std::os::unix::net::UnixListener::bind(&sock) {
        Ok(l) => {
            std::thread::spawn(move || {
                let _ = l.accept();
            });
            match std::os::unix::net::UnixStream::connect(&sock) {
                Ok(_) => println!("unix socket - works"),
                Err(e) => println!("unix socket - BROKEN on connect: {e}"),
            }
        }
        Err(e) => println!("unix socket - BROKEN on bind: {e}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
    if held { Ok(()) } else { Err(std::io::Error::other("the sandbox does not deny the network")) }
}

/// Only the system's own refusal counts: a timeout or an unreachable host would also say "no" with the sandbox off.
fn refused(what: &str, r: std::io::Result<()>) -> bool {
    match r {
        Ok(()) => println!("{what} - GOT THROUGH (sandbox is not holding)"),
        Err(e) if e.raw_os_error() == Some(nix::libc::EPERM) => {
            println!("{what} - denied by the sandbox");
            return true;
        }
        Err(e) => println!("{what} - failed, but not as a sandbox refusal: {e}"),
    }
    false
}
