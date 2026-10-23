//! "What leaves nerd" in docs/privacy-and-security.md: every door out of nerd's own code is listed below with the
//! register row it belongs to, and the rows match that register. A new door fails here until it is listed in both, in the same change.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use quote::ToTokens;
use syn::visit::Visit;

/// (file, function, kind, key prefix, register id or `local`, why). `local` doors do not leave the Mac.
const ALLOW: &[(&str, &str, &str, &str, &str, &str)] = &[
    // git-fetch: the only git allowed the network, and `fn git` lets it clone and nothing else.
    ("src/install.rs", "git", "spawn", "git_exe () ?", "git-fetch", "plugin and catalog fetches, clone only"),
    ("src/app/commands.rs", "plugin_fetch", "broker-exec", "b --window", "git-fetch", "nerd <args> --window fetches a plugin outside the sandbox"),
    // release-check: curl to GitHub's latest release, on your word or daily once you allowed it.
    ("src/release.rs", "curl", "spawn", "curl_exe ()", "release-check", "/usr/bin/curl, https only, no .curlrc and no proxy; its release branch names only that curl"),
    ("src/app/release.rs", "release_exec", "broker-exec", "b --check-update", "release-check", "nerd --check-update --window checks, or installs on the window's click, outside the sandbox"),
    // ssh: a machine in Servers…, listed and opened on your click.
    ("src/client.rs", "list_remote", "spawn", "ssh", "ssh", "lists the servers on the host you named"),
    ("src/client.rs", "spawn_relay", "spawn", "ssh", "ssh", "carries the session to the host you named"),
    // plugin-network: a plugin you approved, inside its own sandbox.
    ("src/plugins.rs", "start", "broker-exec", "broker --plugin-exec", "plugin-network", "starts a plugin through the broker"),
    ("src/plugins.rs", "exec_main", "spawn", "program", "plugin-network", "becomes the plugin once its profile is applied"),
    ("src/plugins.rs", "exec_main", "syscall", "nix :: unistd :: fork", "plugin-network", "forks the plugin off its proxy when the manifest names hosts"),
    ("src/proxy.rs", "", "import", "std :: net ::", "plugin-network", "the proxy's sockets, a plugin's or a pane's"),
    ("src/proxy.rs", "profile", "dns-socket", "", "plugin-network", "the proxy's profile lets it look up the manifest's names"),
    ("src/proxy.rs", "bind", "socket", "TcpListener", "plugin-network", "the proxy's port on both loopbacks, which only its plugin's or pane's sandbox reaches"),
    ("src/proxy.rs", "run", "socket", "TcpListener", "plugin-network", "hands that port to the accept loops"),
    ("src/proxy.rs", "serve", "socket", "TcpListener", "plugin-network", "accepts only its plugin's process group, or the processes below its pane's wrapper"),
    ("src/proxy.rs", "refuse", "socket", "TcpStream", "plugin-network", "answers a refused request"),
    ("src/proxy.rs", "answer", "socket", "TcpStream", "plugin-network", "writes a refusal's status line"),
    ("src/proxy.rs", "head", "socket", "TcpStream", "plugin-network", "reads a CONNECT request"),
    ("src/proxy.rs", "tunnel", "socket", "TcpStream", "plugin-network", "connects to a host and port the manifest names, a name only at a public address"),
    // pane-network: a pane set to "only these providers", through a proxy of its own on the same code.
    ("src/isolation.rs", "pane_exec_main", "syscall", "nix :: unistd :: fork", "pane-network", "forks the pane's program off its proxy when the pane may reach only its providers"),
    ("src/proxy.rs", "relay", "socket", "TcpStream", "plugin-network", "copies a tunnel's bytes"),
    // link: a link or a file you open.
    ("src/links.rs", "open_command", "spawn", "\"open\"", "link", "opens a link or a file in its app"),
    // clipboard: you copy.
    ("src/select.rs", "copy", "spawn", "\"pbcopy\"", "clipboard", "puts the copied text on the macOS clipboard"),
    // pane: programs in panes are yours; these are the doors nerd opens into them.
    ("src/app/lifecycle.rs", "spawn_pane", "pty-spawn", "& argv", "pane", "the pane a pane-open below names"),
    ("src/isolation.rs", "pane_exec_main", "spawn", "program", "pane", "becomes the pane's program once its profile is applied"),
    ("src/app/commands.rs", "split", "pane-open", "shell_argv ()", "pane", "a shell you split off"),
    ("src/app/commands.rs", "new_tab_in", "pane-open", "shell_argv ()", "pane", "a shell in a new tab"),
    ("src/app/commands.rs", "open_tab", "pane-open", "argv", "pane", "the viewer an open_tab below names"),
    ("src/app/commands.rs", "plugins_outcome", "pane-open", "vec ! [\"/usr/bin/less\"", "pane", "less on nerd's own fetch log"),
    ("src/app/commands.rs", "run_action", "pane-open", "vec ! [\"/usr/bin/less\"", "pane", "less on a plugin's own log"),
    ("src/app/model.rs", "open_space", "pane-open", "shell_argv ()", "pane", "a shell in a new space"),
    ("src/app/lifecycle.rs", "restore", "pty-spawn", "", "pane", "a restored pane's shell"),
    ("src/app/lifecycle.rs", "restore", "pty-write", "line", "pane", "an agent's resume line you confirmed once"),
    ("src/app/model.rs", "reshell_pane", "pty-spawn", "", "pane", "the shell back after a command leaves"),
    ("src/app/model.rs", "restart_pane", "pty-spawn", "", "pane", "the shell again when you restart a pane's program"),
    ("src/app/model.rs", "restart_pane", "pty-write", "line", "pane", "an agent's resume line you confirmed once, typed after that restart"),
    ("src/app/input.rs", "key", "pty-write", "", "pane", "your keys"),
    ("src/app/input.rs", "pane_type", "pty-write", "", "pane", "text you type through a dialog"),
    ("src/app/input.rs", "mouse", "pty-write", "", "pane", "your mouse, to a program that asked for it"),
    ("src/app/input.rs", "paste_into", "pty-write", "", "pane", "what you paste, and a message nerd types into an idle agent's pane, its frame first"),
    ("src/app/deliver.rs", "submit_typed", "pty-write", "", "pane", "the carriage return 150 ms after a typed message"),
    // local: these never leave the Mac.
    ("src/app/remote.rs", "open_outside_tmux", "pty-spawn", "", "local", "opens a terminal window for a nerd on this Mac"),
    ("src/app/commands.rs", "perform", "spawn", "\"/usr/bin/open\" -R", "local", "Show in Finder"),
    ("src/broker.rs", "start", "spawn", "exe --broker", "local", "nerd's own broker"),
    ("src/broker.rs", "exec", "spawn", "first", "local", "runs what a broker-exec above names, nothing else"),
    ("src/broker.rs", "", "import", "nix :: sys :: socket :: { recvmsg , sendmsg", "local", "passes a pane's terminal over the broker's unix socket"),
    ("src/broker.rs", "start_beside", "broker-spawn", "& argv", "local", "nerd --exec-broker beside an older broker"),
    ("src/broker.rs", "spawn", "broker-req", "Req :: Spawn", "local", "the pane request; its callers are the broker-spawn entries"),
    ("src/broker.rs", "exec", "broker-req", "Req :: Exec", "local", "the run request; its callers are the broker-exec entries"),
    ("src/pty.rs", "spawn", "broker-spawn", "& argv", "pane", "every pane, through nerd --pane-exec when it is isolated; its callers are the pty-spawn entries"),
    ("src/sandbox.rs", "", "dns-socket", "", "local", "the server's profile denies the DNS socket by name"),
    ("src/sandbox.rs", "selftest", "dns-socket", "", "local", "the selftest probes that the DNS socket is refused"),
    ("src/broker.rs", "ps_local", "spawn", "\"ps\"", "local", "reads the process table"),
    ("src/client.rs", "tmux", "spawn", "\"tmux\"", "local", "asks the tmux nerd runs in"),
    ("src/client.rs", "reload_ghostty", "spawn", "\"ps\"", "local", "finds Ghostty"),
    ("src/client.rs", "reload_ghostty", "spawn", "\"kill\"", "local", "asks Ghostty to reload its config"),
    ("src/client.rs", "walk_to_app", "spawn", "\"ps\"", "local", "finds the terminal app"),
    ("src/client.rs", "open_window", "spawn", "\"/usr/bin/open\"", "local", "opens a terminal window"),
    ("src/client.rs", "appearance_dark", "spawn", "\"/usr/bin/defaults\"", "local", "reads light or dark mode"),
    ("src/presets.rs", "pref_now", "spawn", "\"/usr/bin/defaults\"", "local", "reads iTerm2's margins and default profile before its preset changes them"),
    ("src/presets.rs", "pref_set", "spawn", "\"/usr/bin/defaults\"", "local", "sets iTerm2's margins and default profile for its preset, and puts them back on undo"),
    ("src/client.rs", "connect_or_spawn", "spawn", "exe --daemon", "local", "starts nerd's server"),
    ("src/client.rs", "new_in_place", "spawn", "exe --new", "local", "becomes nerd --new when a session took the last server first and you chose a new one"),
    ("src/client.rs", "switch_in_place", "spawn", "exe --resume", "local", "becomes nerd --resume N in the same terminal when you open another server of this Mac in this session"),
    ("src/daemon.rs", "start", "spawn", "& exe --version", "local", "asks the installed build its version"),
    ("src/daemon.rs", "handover_exec", "command", "std :: env :: current_exe ()", "local", "update in place: the new build takes over the open panes and sockets"),
    ("src/git.rs", "run_git", "spawn", "\"git\"", "local", "the file tree's git, without lazy fetch, fsmonitor or filters"),
    ("src/git.rs", "read_git", "spawn", "\"git\"", "local", "the file tree's git, without lazy fetch, fsmonitor or filters"),
    ("src/install.rs", "git_exe", "spawn", "\"/usr/bin/xcode-select\"", "local", "checks that Apple's git is installed"),
    ("src/install.rs", "git_exe", "spawn", "& exe --version", "local", "asks git its version"),
    ("src/notify.rs", "run", "spawn", "cmd", "local", "runs what a notify call below names"),
    ("src/notify.rs", "observe", "notify", "afplay", "local", "plays a system sound"),
    ("src/preview.rs", "convert_to_png", "spawn", "\"/usr/bin/sips\"", "local", "converts an image for the preview"),
    ("src/preview.rs", "to_pixels", "spawn", "\"/usr/bin/sips\"", "local", "converts an image for the preview"),
    ("src/sandbox.rs", "selftest", "socket", "", "local", "probes that the sandbox refuses the network"),
    ("src/hook.rs", "run_for", "spawn", "\"/bin/sh\"", "local", "nerd's own hook script, run for an isolated pane's relayed hook call"),
    ("src/session.rs", "exclude_from_time_machine", "spawn", "\"/usr/bin/tmutil\"", "local", "keeps saved pane text and messages out of Time Machine"),
    ("src/spawn.rs", "cmd", "command", "program", "local", "the one door every other spawn goes through"),
    ("src/trial.rs", "trial", "spawn", "exe --plugin-exec", "local", "a plugin's trial run, with no network"),
    ("src/release.rs", "install_cmd", "spawn", "\"/bin/sh\"", "local", "the embedded install.sh on the downloaded archive: release-key signature with /usr/bin/openssl, checksum, unpack, sign, rename into ~/.local/bin; with an archive it reaches no network"),
];

/// Rows the register states with no door the scan sees: a setting (the sandbox off), or a unix socket (not scanned).
const STATED_ONLY: &[&str] = &["sandbox-off", "agent-inbox"];

/// Process and network calls from libc or nix that bypass `spawn::cmd` or open a socket of their own.
const SYSCALLS: &[&str] = &["fork", "vfork", "execv", "execve", "execvp", "execvpe", "execl", "execle", "execlp", "posix_spawn", "posix_spawnp", "system", "popen", "socket", "socketpair", "connect", "getaddrinfo", "gethostbyname"];
const SOCKETS: &[&str] = &["TcpStream", "UdpSocket", "TcpListener", "ToSocketAddrs"];

/// Every identifier in a token stream, groups included: a macro's body is tokens, not syntax.
fn idents(t: proc_macro2::TokenStream, out: &mut Vec<String>) {
    for tt in t {
        match tt {
            proc_macro2::TokenTree::Ident(i) => out.push(i.to_string()),
            proc_macro2::TokenTree::Group(g) => idents(g.stream(), out),
            _ => {}
        }
    }
}

#[derive(Debug)]
struct Site {
    file: String,
    func: String,
    kind: &'static str,
    key: String,
    line: usize,
}

struct Scan<'a> {
    file: &'a str,
    funcs: Vec<String>,
    bodies: Vec<String>,
    sites: Vec<Site>,
    seen_fns: BTreeSet<String>,
    test_files: Vec<PathBuf>,
    handled: BTreeSet<(usize, usize)>,
    called: BTreeSet<(usize, usize)>,
}

fn text(t: &impl ToTokens) -> String {
    t.to_token_stream().to_string()
}

/// Only code that exists in test builds alone: `cfg(test)` or `cfg(all(test, ..))`. `cfg(any(test, ..))` is in releases too.
fn is_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| a.path().is_ident("cfg") && matches!(&a.meta, syn::Meta::List(l) if { let t = l.tokens.to_string(); t == "test" || t.starts_with("all (test ,") }))
}

fn path_ends(p: &syn::Path, tail: &[&str]) -> bool {
    let segs: Vec<String> = p.segments.iter().map(|s| s.ident.to_string()).collect();
    segs.len() >= tail.len() && segs[segs.len() - tail.len()..].iter().zip(tail).all(|(a, b)| a == b)
}

fn call_path(e: &syn::Expr) -> Option<&syn::Path> {
    match e {
        syn::Expr::Call(c) => match &*c.func {
            syn::Expr::Path(p) => Some(&p.path),
            _ => None,
        },
        syn::Expr::Paren(p) => call_path(&p.expr),
        _ => None,
    }
}

fn lit(e: &syn::Expr) -> Option<String> {
    match e {
        syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) => Some(s.value()),
        _ => None,
    }
}

impl<'a> Scan<'a> {
    fn new(file: &'a str) -> Self {
        Scan { file, funcs: Vec::new(), bodies: Vec::new(), sites: Vec::new(), seen_fns: BTreeSet::new(), test_files: Vec::new(), handled: BTreeSet::new(), called: BTreeSet::new() }
    }
    /// The first `--flag` literal in the enclosing function: what tells nerd's own spawns of itself apart.
    fn flag(&self) -> String {
        let body = self.bodies.last().map(String::as_str).unwrap_or("");
        body.split("\"--").nth(1).and_then(|r| r.split('"').next()).map(|f| format!("--{f}")).unwrap_or_else(|| "-".into())
    }
    fn add(&mut self, kind: &'static str, key: String, at: proc_macro2::Span) {
        let func = self.funcs.last().cloned().unwrap_or_default();
        self.sites.push(Site { file: self.file.to_string(), func, kind, key, line: at.start().line });
    }
}

impl<'ast> Visit<'ast> for Scan<'_> {
    fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
        if is_test(&m.attrs) {
            if m.content.is_none() {
                let here = Path::new(self.file);
                let dir = if here.file_name().is_some_and(|n| n == "mod.rs" || n == "main.rs" || n == "lib.rs") { here.parent().unwrap().to_path_buf() } else { here.with_extension("") };
                self.test_files.push(dir.join(format!("{}.rs", m.ident)));
            }
            return;
        }
        syn::visit::visit_item_mod(self, m);
    }
    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        if is_test(&f.attrs) {
            return;
        }
        self.seen_fns.insert(f.sig.ident.to_string());
        self.funcs.push(f.sig.ident.to_string());
        self.bodies.push(text(&f.block));
        syn::visit::visit_item_fn(self, f);
        self.funcs.pop();
        self.bodies.pop();
    }
    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        if is_test(&f.attrs) {
            return;
        }
        self.seen_fns.insert(f.sig.ident.to_string());
        self.funcs.push(f.sig.ident.to_string());
        self.bodies.push(text(&f.block));
        syn::visit::visit_impl_item_fn(self, f);
        self.funcs.pop();
        self.bodies.pop();
    }
    fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
        if !is_test(&i.attrs) {
            syn::visit::visit_item_impl(self, i);
        }
    }
    fn visit_item_use(&mut self, u: &'ast syn::ItemUse) {
        if is_test(&u.attrs) {
            return;
        }
        let s = text(&u.tree);
        let mut words = Vec::new();
        idents(u.tree.to_token_stream(), &mut words);
        let low = words.iter().any(|w| w == "libc" || w == "nix") && (s.contains('*') || words.iter().any(|w| SYSCALLS.contains(&w.as_str())));
        // A door imported under a short name is a door the call scan cannot name.
        if s.contains("Command as") || s.contains("spawn :: cmd") || words.iter().any(|w| SOCKETS.contains(&w.as_str())) || low {
            self.add("import", s, u.use_token.span);
        }
    }
    fn visit_stmt(&mut self, s: &'ast syn::Stmt) {
        let attrs = match s {
            syn::Stmt::Local(l) => &l.attrs[..],
            syn::Stmt::Macro(m) => &m.attrs[..],
            syn::Stmt::Item(_) | syn::Stmt::Expr(..) => &[][..],
        };
        if !is_test(attrs) {
            syn::visit::visit_stmt(self, s);
        }
    }
    fn visit_expr_method_call(&mut self, m: &'ast syn::ExprMethodCall) {
        let method = m.method.to_string();
        let recv = text(&m.receiver);
        if method == "arg"
            && let Some(p) = call_path(&m.receiver)
            && path_ends(p, &["spawn", "cmd"])
            && let syn::Expr::Call(c) = &*m.receiver
        {
            let first = m.args.first().and_then(lit).unwrap_or_else(|| "-".into());
            let key = format!("{} {}", c.args.first().map(text).unwrap_or_default(), first);
            let at = c.func.to_token_stream().into_iter().next().map(|t| t.span()).unwrap_or_else(proc_macro2::Span::call_site);
            self.handled.insert((at.start().line, at.start().column));
            self.add("spawn", key, at);
        }
        if method == "write" && (recv == "pty" || recv.ends_with(". pty")) {
            self.add("pty-write", m.args.first().map(text).unwrap_or_default(), m.method.span());
        }
        if method == "run" && recv.ends_with("sink") {
            self.add("notify", m.args.first().map(|a| lit(a).unwrap_or_else(|| text(a))).unwrap_or_default(), m.method.span());
        }
        if method == "spawn_pane" || method == "open_tab" {
            self.add("pane-open", m.args.iter().nth(1).map(text).unwrap_or_default(), m.method.span());
        }
        if method == "spawn" && m.args.len() == 6 {
            self.add("broker-spawn", m.args.iter().nth(1).map(text).unwrap_or_default(), m.method.span());
        }
        if method == "exec" && m.args.len() == 4 {
            let key = format!("{recv} {}", self.flag());
            self.add("broker-exec", key, m.method.span());
        }
        syn::visit::visit_expr_method_call(self, m);
    }
    fn visit_expr_call(&mut self, c: &'ast syn::ExprCall) {
        if let syn::Expr::Path(p) = &*c.func {
            let at = c.func.to_token_stream().into_iter().next().map(|t| t.span()).unwrap_or_else(proc_macro2::Span::call_site);
            self.called.insert((at.start().line, at.start().column));
            let arg = |i: usize| c.args.iter().nth(i).map(|a| lit(a).unwrap_or_else(|| text(a))).unwrap_or_default();
            if path_ends(&p.path, &["spawn", "cmd"]) && !self.handled.contains(&(at.start().line, at.start().column)) {
                let program = text(&c.args[0]);
                let key = if program.contains("exe") { format!("{program} {}", self.flag()) } else { format!("{program} -") };
                self.add("spawn", key, at);
            } else if path_ends(&p.path, &["Command", "new"]) {
                self.add("command", arg(0), at);
            } else if path_ends(&p.path, &["Pty", "spawn"]) {
                self.add("pty-spawn", text(&c.args[1]), at);
            }
            let first = p.path.segments.first().map(|s| s.ident.to_string()).unwrap_or_default();
            let last = p.path.segments.last().map(|s| s.ident.to_string()).unwrap_or_default();
            if (first == "libc" || first == "nix") && SYSCALLS.contains(&last.as_str()) {
                self.add("syscall", text(&p.path), at);
            }
        }
        syn::visit::visit_expr_call(self, c);
    }
    fn visit_expr_path(&mut self, e: &'ast syn::ExprPath) {
        let at = e.to_token_stream().into_iter().next().map(|t| t.span()).unwrap_or_else(proc_macro2::Span::call_site);
        // A door taken as a value (`let mk = Command::new;`) is a door the call scan never sees called.
        if !self.called.contains(&(at.start().line, at.start().column)) && [&["Command", "new"][..], &["spawn", "cmd"], &["Pty", "spawn"]].iter().any(|t| path_ends(&e.path, t)) {
            self.add("door-value", text(&e.path), at);
        }
        syn::visit::visit_expr_path(self, e);
    }
    fn visit_item_type(&mut self, t: &'ast syn::ItemType) {
        if is_test(&t.attrs) {
            return;
        }
        let mut words = Vec::new();
        idents(t.ty.to_token_stream(), &mut words);
        if words.iter().any(|w| w == "Command" || SOCKETS.contains(&w.as_str())) {
            self.add("alias", text(&t.ty), t.ident.span());
        }
        syn::visit::visit_item_type(self, t);
    }
    fn visit_expr_struct(&mut self, e: &'ast syn::ExprStruct) {
        if path_ends(&e.path, &["Req", "Spawn"]) || path_ends(&e.path, &["Req", "Exec"]) {
            self.add("broker-req", text(&e.path), e.path.segments[0].ident.span());
        }
        syn::visit::visit_expr_struct(self, e);
    }
    fn visit_lit_str(&mut self, l: &'ast syn::LitStr) {
        if l.value().contains("mDNSResponder") {
            self.add("dns-socket", "mDNSResponder".into(), l.span());
        }
    }
    fn visit_path(&mut self, p: &'ast syn::Path) {
        if let Some(s) = p.segments.iter().find(|s| SOCKETS.contains(&s.ident.to_string().as_str())) {
            self.add("socket", s.ident.to_string(), s.ident.span());
        }
        syn::visit::visit_path(self, p);
    }
    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        let t = m.tokens.to_string();
        let mut words = Vec::new();
        idents(m.tokens.clone(), &mut words);
        let calls = SYSCALLS.iter().filter(|c| t.contains(&format!("libc :: {c} (")) || t.contains(&format!("nix :: {c} ("))).map(|c| c.to_string());
        let doors = ["Command :: new", "spawn :: cmd", "Pty :: spawn"].iter().filter(|d| t.contains(*d)).map(|d| d.to_string());
        let sockets = words.iter().filter(|w| SOCKETS.contains(&w.as_str())).cloned();
        for door in doors.chain(calls).chain(sockets).collect::<BTreeSet<_>>() {
            self.add("macro", door, m.path.segments[0].ident.span());
        }
        // A macro's string literals are tokens too: `format!` builds sandbox profiles.
        if t.contains("mDNSResponder") {
            self.add("dns-socket", "mDNSResponder".into(), m.path.segments[0].ident.span());
        }
        syn::visit::visit_macro(self, m);
    }
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

fn scan() -> (Vec<Site>, BTreeMap<String, BTreeSet<String>>) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    files.sort();
    let mut sites = Vec::new();
    let mut fns = BTreeMap::new();
    let mut test_files: Vec<PathBuf> = Vec::new();
    let mut parsed = Vec::new();
    for f in &files {
        let rel = f.strip_prefix(root).unwrap().to_string_lossy().into_owned();
        let ast = syn::parse_file(&std::fs::read_to_string(f).unwrap()).unwrap_or_else(|e| panic!("{rel}: {e}"));
        let mut s = Scan::new(&rel);
        s.visit_file(&ast);
        test_files.extend(s.test_files);
        parsed.push((rel.clone(), s.sites, s.seen_fns));
    }
    for (rel, s, f) in parsed {
        if test_files.iter().any(|t| t == Path::new(&rel)) {
            continue;
        }
        sites.extend(s);
        fns.insert(rel, f);
    }
    (sites, fns)
}

fn register_ids() -> BTreeSet<String> {
    let doc = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/privacy-and-security.md")).unwrap();
    doc.split("<!-- egress:").skip(1).filter_map(|r| r.split(" -->").next()).map(str::to_string).collect()
}

#[test]
fn every_door_out_of_nerd_is_listed_with_its_register_row() {
    let (sites, fns) = scan();
    // The scan must see production code that sits after a file's first test module.
    assert!(fns.get("src/plugins.rs").is_some_and(|f| f.contains("profile") && f.contains("start")), "the scan missed plugins.rs");
    assert!(fns.get("src/app/mod.rs").is_some_and(|f| f.len() >= 5), "the scan missed app/mod.rs");

    let mut used = vec![false; ALLOW.len()];
    let mut unlisted = Vec::new();
    for s in &sites {
        match ALLOW.iter().position(|a| a.0 == s.file && a.1 == s.func && a.2 == s.kind && s.key.starts_with(a.3)) {
            Some(i) => used[i] = true,
            None => unlisted.push(format!("  {}:{} in {}: {} `{}`", s.file, s.line, s.func, s.kind, s.key)),
        }
    }
    let stale: Vec<String> = ALLOW.iter().zip(&used).filter(|(_, u)| !**u).map(|(a, _)| format!("  {a:?}")).collect();
    assert!(
        unlisted.is_empty() && stale.is_empty(),
        "doors out of nerd and the list disagree.\nA new door goes into \"What leaves nerd\" in docs/privacy-and-security.md and into ALLOW in tests/egress.rs, in the same change.\nUnlisted:\n{}\nListed but gone:\n{}",
        unlisted.join("\n"),
        stale.join("\n")
    );

    let register = register_ids();
    let listed: BTreeSet<String> = ALLOW.iter().map(|a| a.4.to_string()).filter(|i| i != "local").collect();
    let missing: Vec<&String> = listed.difference(&register).collect();
    let stated: BTreeSet<String> = STATED_ONLY.iter().map(|s| s.to_string()).collect();
    let undoored: Vec<&String> = register.iter().filter(|i| !listed.contains(*i) && !stated.contains(*i)).collect();
    assert!(missing.is_empty(), "rows used in ALLOW but missing from What leaves nerd in docs/privacy-and-security.md: {missing:?}");
    assert!(undoored.is_empty(), "register rows with no door in ALLOW and not in STATED_ONLY: {undoored:?}");
}

#[test]
fn the_link_row_names_the_help_page_opened_from_search() {
    let doc = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/privacy-and-security.md")).unwrap();
    let row = doc.lines().find(|l| l.contains("<!-- egress:link -->")).expect("the egress:link row");
    assert!(row.contains("Search"), "a help page opened from Search is a link door: {row}");
}

/// The scan's own coverage: each shape a door was once hidden in must still be reported.
#[test]
fn every_known_hiding_place_is_seen() {
    let fixture = r#"
        use crate::spawn::cmd;
        use std::net::ToSocketAddrs;
        use std::net::TcpStream as Conn;
        type C = std::process::Command;
        #[cfg(any(test, target_os = "macos"))]
        fn any_test() { let _ = std::process::Command::new("curl"); }
        fn value() { let mk = std::process::Command::new; }
        fn sys() { unsafe { nix::libc::system(std::ptr::null()); } }
        fn hidden() { dbg!(std::net::TcpStream::connect("x")); }
        fn notify(&mut self) { self.sink.run("curl", &[]); }
        fn tab(&mut self) { self.open_tab("x".into(), vec!["/usr/bin/curl".into()]); }
        fn broker(b: &Broker) { b.spawn(1, &argv, &cwd, 1, 1, env); let _ = Req::Exec { id, argv, env, cwd }; }
        fn dns() { let _ = std::os::unix::net::UnixStream::connect("/private/var/run/mDNSResponder"); }
        fn dns_in_profile() { let _ = format!("(allow network-outbound (literal \"/private/var/run/mDNSResponder\"))"); }
        #[cfg(test)]
        fn only_tests() { let _ = std::process::Command::new("fine"); }
    "#;
    let mut s = Scan::new("src/fixture.rs");
    s.visit_file(&syn::parse_file(fixture).unwrap());
    let kinds: BTreeSet<&str> = s.sites.iter().map(|x| x.kind).collect();
    for k in ["import", "alias", "command", "door-value", "syscall", "macro", "notify", "pane-open", "broker-spawn", "broker-req", "dns-socket"] {
        assert!(kinds.contains(k), "the scan no longer sees a {k}: {:?}", s.sites);
    }
    assert!(!s.sites.iter().any(|x| x.func == "only_tests"), "test-only code is not a door");
    assert!(s.sites.iter().any(|x| x.func == "dns_in_profile" && x.kind == "dns-socket"), "the DNS socket named inside a macro");
    assert!(s.sites.iter().any(|x| x.func == "any_test" && x.kind == "command"), "cfg(any(test, ..)) is in releases");
}
