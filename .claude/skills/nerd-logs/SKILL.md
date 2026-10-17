---
name: nerd-logs
description: Read nerd's logs to answer "what happened to nerd" - why a server stopped, why a window closed or lost its session, whether it crashed, what a user's report file says. Use when a server is gone, a client dropped, a pane misbehaved, or someone hands over a nerd-report file.
---

# Reading nerd's logs

Answer from the log lines, not from theory. Every line below comes from a `crate::log` call in `src/`;
if a line you see is not here, grep `src/` for its text before interpreting it.

## Step one: the report

Run `nerd --export-logs <hours>` (default 24) and read the file it names. It needs no running server.
It holds every log line of the period from the main log and each server's log
(`== main`, `== server N`), then a summary of each crash report of the period: nerd's own, and those of
the terminal apps its attach lines name (`== crash <file>`), then macOS's memory-pressure kills (`== jetsam <file>`). The menu's
`Export logs…` writes the same file and offers to show it in Finder. A report a user sends is this format too.

Redacted in the report: the home folder (`~`), any other user folder (`/Users/user`), the host name
(`host`), remote machines (`remote`), other Macs that connected (`machine`), IPv4 addresses and long
token-shaped strings (`[redacted]`; not in crash summaries, where Rust symbols look like tokens).
Folder and file names are not redacted.

## Where the logs live

- State root: `~/Library/Application Support/nerd/`; `NERD_STATE_DIR` replaces it, and a different
  `HOME` moves it.
- `daemon.log` in the root: processes with no server (a `--remote` client, commands), and any process whose
  `NERD_INSTANCE` names a folder that does not exist (a pane's variable carried under another `HOME`).
- `instances/<id>/daemon.log`: that server and its local clients.
- `reports/`: the report files, `nerd-report-<date>-<time>.log`.
- Each log rolls into `daemon.log.old` past 1 MiB, so the oldest lines of a busy week are gone.

## The line format

```
2026-09-26T17:30:56.851+02:00 INFO  server[9618] stopped by SIGTERM: from pid 812 uid 501 /bin/kill, parent 700 /bin/zsh
```

- Time: RFC 3339 in local time with milliseconds and the UTC offset, so it lines up with `log show`.
- Level, padded to five: `ERROR` something failed; `WARN` degraded but running; `INFO` lifecycle;
  `DEBUG` tracing only (`NERD_TRACE=1`).
- Role and pid: `server`, `broker`, `client` (a window, `--remote`) or `cli` (commands such as `--export-logs`).
- Control characters in the text are escaped, so one line is one event.
- An older build writes `<unix seconds> <text>` into the same file (an update in place leaves both);
  every reader accepts both. `date -r <seconds>` turns one into local time. In a report such a line
  appears converted, with level `INFO` and role `?`.

## The lines that matter

Search by level first: `grep -E ' (ERROR|WARN) ' daemon.log`.

ERROR:
- `stopped by error: ...` / `stopped by panic: ...` - the loop failed; after a panic the process aborts and a crash report follows.
- `broker failed to start: ...` / `broker socket did not carry over: ...` - panes cannot launch.
- `session snapshot did not restore: ...` - the user sees an empty desktop; the snapshot file is the suspect.
- `session save did not answer within 5 s` - the last snapshot may be stale.
- `update failed: ...` - the old build keeps running.
- `client writer did not start: ...` - a window could not be served.
- `left: error: ...` (role client) - the window ended on an error.
  `left: error: not a terminal: ...` - `nerd` was run with no terminal (an agent's tool shell, a script); it refused before
  starting a server. Lands in the main log, or in the instance log a pane's `NERD_INSTANCE` names.
- `left: panic: <message> (<file:line:col>)`, `left: panic in the screen thread: ...` (role client) - the window crashed; a crash report may follow.
  `panic in thread <name>: ...` - a side thread of the client panicked; the window may have kept running.
- `server pid P did not start: <e>` - a server that never ran (`server already running` is a second one started by mistake; the pid tells it from the running one).
- `screens: directory failed: ...`, `screens: write failed for pane ...`, `sandbox status file failed: ...`.
- `plugin <name>: 5 crashes within 180 s, disabled until Reload plugins and agents` - it stays off (the menu says so) until Reload plugins and agents.
- `plugin socket failed: ..., plugins cannot connect` - `nerd.psock` next to the main socket could not be bound.
- `pane N: reading it failed`, `pane N: writing to it failed`, `pane N: the shell would not start ...`.
- `connect: <machine> did not list: ...`, `open outside tmux failed: ...`, `open outside tmux: ... started no client ...`.
- `worker <name> failed: ...` / `worker <name> did not start: ...` - a background worker panicked or never ran.

WARN:
- `sandbox: did NOT enable (...), running without it` - the server has network.
- `config.json failed to parse, using defaults`.
- `handover: no pane came over`.
- `client N lost: ...`, `frame did not go out: ...`, `client gone: ...` - the link to a window broke; routine when an ssh link drops.
- `broker exited (code N | signal N (SIGNAME))` (`not seen how: ...` if it could not be waited on) and `broker ended; new panes cannot launch`,
  in either order - the process that starts panes is gone; restart the server. A broker adopted across an update writes only `broker ended`.
- `client backlog passed N MiB` - a window is not reading; its frames wait.
- `refused a connection from uid ...` - another user tried the socket.
- `refused <stop|update|disconnect|attach> from pid P, inside a pane` - a program in one of this server's panes (an agent's
  tool shell) asked for it; nothing happened. `allow_from_panes` in config.json lets it through.
- `refused nerd <--stop|--update|--disconnect> with no name, inside a pane` (role cli) - the no-name form was run in some nerd's pane and acted on nothing. A pane inherits `NERD_INSTANCE`, so the line lands in that server's `daemon.log`; `allow_from_panes` lets it through.
- `client N: unrecognized message: ...` - a client from another build, or not a nerd client.
- `pane N: not reading its input, replies dropped`, `terminal reply swallowed: N chars`.
- `pane N: <agent> session <id> held by another process, so not resumed; typed: <line>` - a restored pane started its agent fresh.
- `pane N: <agent> session <id> not resumed: its resume line changed since it was confirmed; typed: <line>` - the manifest's
  resume line differs from the one confirmed at install (`hook-confirmed.json` in the state root); the agent hooks setting asks again.
- `agent sessions not written to <path>` / `confirmed resume lines not written to <path>` - a user's agent keeps no session, or the confirmation is asked again.
- `agent manifest skipped: <path>: <why>` - a manifest in the plugins folder broke a rule (`... is a built-in agent and cannot be replaced`,
  `... is not a hook word`, `config path ...`, `does not parse`); the agent is recognised by name only. Read at start and on Reload plugins and agents.
- `connect: dropped an answer about ... nobody waits for`, `tree <what>: ... in <dir>` (a file-tree walk was cut short).
- `plugin <name>: exited on its own (<how>)` - it left without nerd asking (before or after `Connect`); `<how>` is `code N`,
  `signal N (SIGNAME)` or `an older broker does not say how`; `plugin <name>: ended with the broker for plugins` when its broker went; its own output is in
  `<state root>/plugin-data/<name>/output.log`. `plugin <name>: did not start: ...` - the broker could not launch it (counted too).
- `plugin <name>: no Connect within 5 s, stopped` - it started but never said `Connect` (counted too).
- `plugin <name>: crash N of 5 within 180 s` - follows each of the three lines above; at 5 the ERROR line follows.
- `refused a plugin claim from pid P: <why>` - a `Connect` with role `plugin` that the broker did not launch
  (`plugin "x" was not started by this server`), a second one for a connected plugin (`plugin "x" is already connected`),
  anything but `Connect` on the plugin socket (`the plugin socket serves only Connect`), `Connect` on the main socket
  (`a plugin connects on the plugin socket`), another role, or an old protocol; the process was told `refused: <why>`.
- `refused a connection to the plugin socket from uid ...` - another user tried it.
- `plugin <name>: report for pane N dropped: not from its current connection` - from a second or stale connection of that plugin.
- `plugin <name>: report for pane N refused: <why>` - `report_state` for a pane whose agent is not this plugin's, or not a state word.
- `plugin <name>: unrecognized message: ...`, `plugin <name>: pane N state not written: ...`.
- `plugins off: the broker for plugins did not start: ...`, `... did not connect; restart the server to enable plugins`,
  `... ended; restart the server to enable plugins`, `plugins off: the older broker could not start the broker for plugins: ...` -
  plugins stay off until the server restarts; the menu says so when a plugin command is chosen. After an update in place from a
  build whose broker cannot launch plugins, INFO `plugins: the broker is from an older build; starting one of this build beside it` comes first.
- `agent manifest skipped: <path>: <why>` also covers a plugin's executable half: `exec command ... was not found`,
  `... may not be a plugin's command` (open, osascript, launchctl), `write path ... would reach nerd's own files`, `needs nerd X`.

INFO:
- `server started, id <id>` - a server began (fresh or after an update in place).
- `reaped N finished child processes left from before` - after an update in place, the zombies the old build left
  (sounds, opened links) were cleared; the pid stays the same across the update, and so did they.
- `sandbox: <status>` / `sandbox: inherited from the previous build`.
- `stopped by SIGTERM: from pid P uid U <path>, parent Q <path>` (or `SIGHUP`) - the signal's sender,
  looked up when it arrived. `gone`: the sender had already exited; `unknown`: it could not be read;
  `from the kernel`: pid 0. Sender `/sbin/launchd` (pid 1): logout, restart, or the LaunchAgent being
  unloaded or removed. A shell or `kill`: someone or some script ran it; follow the parent.
- `stopped by --stop` - `nerd --stop` asked over the socket.
- No `stopped by` line after a server's last lines: SIGKILL, a crash, or power loss. Check crash reports and `log show`.
- `handover: N panes, exec <path>` (old build) then `handover: client N carried over, ...` (new build).
- `agent manifest loaded: <name> from <path>` - a user's agent manifest, at start and on Reload plugins and agents.
- `plugin <name>: starting <program>`, `plugin <name>: started, pid P`, `plugin <name>: connected, events <list>` - a lazy start,
  on its menu command or a subscribed event. `plugin <name>: stopped` - nerd stopped it (Reload plugins and agents, handshake timeout).
  Before an update in place every plugin is stopped and no line follows; the new build starts them again on first use.
- `pane N: <agent> session <id> resumed; typed: <line>` (or `not resumable`: the id is not of that agent's kind, or it does not resume) - what a restored pane typed.
- `first hook install confirmed: ...` / `first hook install declined: ...` - the Install the hook dialog, naming each config and resume line it showed;
  `first hook install from the command line confirmed|declined: ...` (role cli) - the same asked by `nerd --install-hook`.
- `key file rewritten for this build`, `screens: N restored, M removed`, `report saved: N hours`.
- Messages between panes (never the text): `message <id> <from>(<pane>,uid <u>) -> <to>(<pane>,uid <u>) <n> B hop <h> via <via>: <state>`
  is one transition of one message; `<state>` is `queued`, `delivered`, `read`, `answered`, `dropped`, `held (<why>)`, `refused (<why>)` or `expired`,
  and `via` is `inbox`, `hook`, `extension` or `typed`. `held (...)` and `refused (...)` lines are written at most once a second each, with
  `(and N more like it before)`: a flood shows as that count. The full records are in `messages.jsonl`.
  `policy.json changed outside nerd; ignored` - the file was edited by something other than nerd's own window; the running copy stays.
  `policy.json does not parse: …` - messaging is off until the owner fixes it in the Messages window.
  `pane N: a typed message was not submitted` - nerd typed a message into an idle agent and the frame line is still the last line on screen after the check delay (written once per pane).
  `pane N: its recorded inbox is not its own; delivering by hook` - Claude Code's reported inbox socket has no listener among that pane's processes (written once per pane).
  `pane N: socket mode reports no agent` - the pi or OpenCode extension, in socket mode, asked for mail in a pane where no agent has reported (no state file).
  `refused a message connection from uid N` (or `from pid P, uid N: <why>`, `from pane N: it holds 8`, `: N are open …`) - a stranger, or a pane over its cap.
  `<dir> is not nerd's own …` (`/tmp/nerd-<uid>`) - the shared door's directory has the wrong owner or mode; an isolated pane of an admitted account does not start.
  `pane N proxy: …` - a pane set to Only these providers: each connection and refusal of its proxy, in the daemon.log of the account the pane runs as.
- `client N attached: WxH from <cwd>, graphics=, mux=, bye=, connects=, client pid P under <name> pid T[, first window after 7h 4m]` -
  `bye=true` is a remote window. `<name> pid T` is the client's top ancestor short of launchd: the terminal app (`ghostty`, `iTerm2`),
  `tmux`, or `sshd-session` for a window over ssh; match it with the terminal's crash reports. `client pid P` alone: the client has no ancestor short of launchd; `client pid unknown`: the peer could not be read.
  `first window after …`: how long the server ran with no window before this one (wall clock, sleep included).
- `handover: client N carried over, K panes, graphics=…, client pid P under <name> pid T` - the window kept across an update in place.
- `client N detached: <how>; client pid P <state>, <name> pid T <state>` - the window's socket ended. `<how>`: `asked to detach`
  (the client said so), `socket closed (EOF)`, `socket error: ...`. `<state>` at that moment: `running`, `exiting` (dead, not yet reaped)
  or `gone` (also for a pid another process took since the attach: each start time is read at the attach and compared); without a terminal part when the attach line had none. A terminal `gone` with its client: the terminal app died or was quit; read its crash reports and Jetsam events in the report.
- `client M taken over by client N` - another window attached; M's end has no line of its own. A window whose socket had
  already closed when the next one attached is not taken over: its `client M detached: ...` line follows N's `attached` line.
- `no window now; K panes keep running` - written once when the last window goes, whatever took it; the server runs on.
- `detached by the user` - the user chose Detach (menu, mouse or key). `the last space closed; detached` - the last pane's process left or the user closed the last space.
- `pane N closed: its process left (<how>)` - the pane's shell ended (`code N`, `signal N (SIGNAME)`, `an older broker does not say how`,
  `it left during the update, how is not known`). `pane N: the command left (<how>), the shell is back` - a command pane gave its shell back;
  without `(<how>)` the user closed it or its command did not launch.
- `pane N closed by the user`, `tab closed by the user with N panes`, `space closed by the user with N panes`,
  `group closed by the user: N spaces with M panes`, `worktree checkout deleted by the user; its space closed with N panes` - from the menu.
- `client N: disconnect everyone, from ...` / `disconnect everyone, from the menu`, then one `ended: ...` per connection.
- Role client: `started in <TERM_PROGRAM>, mux=<bool>` / `started for <machine> ...` (remote), then
  `left: <why>` - `the server stopped (X)`, `the server ended this session (...)`,
  `the connection closed (a local client is not told why; the server's line says)`, or a signal:
  `SIGHUP from the kernel (the terminal hung up)`, `SIGHUP from pid P uid U <path>, parent Q <path>` (tmux closing its window
  sends its own), `SIGTERM from pid ...` (someone ran `kill`; follow the parent). A signal wins over the error it caused.
  A client of this build writes exactly one `left` line. A `started` with no `left` from the same pid: SIGKILL, or an older
  build; the server's `detached` line still says how its socket ended and whether its terminal was gone.
- Role client on the far Mac of a remote window, in that server's log: `started as a relay over ssh` (`nerd --relay`), then one
  `left: the server closed the connection` (the server's line says why), `left: ssh's input ended (the client left or the link dropped), then the server closed`,
  `left: ssh stopped taking the output (...)`, `left: socket error: ...`, `left: the server is not running`, or a signal as above.
  A relay given a bad or unknown server name writes nothing: it exits 5 or 4, and the `--remote` client says why.
- `connect: ...`, `open outside tmux: ...`, `inside a multiplexer: ...`.

DEBUG (only with `NERD_TRACE=1` in the server's environment): `trace key ...`, `trace paste N chars`,
`trace frames: ...`, `trace pane N OSC: ...`. Never what was typed.

## An agent shows no state

The hook is installed (Settings, agent hooks: installed) but the pane shows no state: the agent has not run the hook yet.
Codex runs no new hook until it is approved at its "Hooks need review" prompt ("Trust all and continue"); every agent
first asks to trust the folder. `NERD_HOOK_DUMP` below shows whether a hook ran at all: no file, no call.

## Recording an agent's hook payloads

Start the server with `NERD_HOOK_DUMP=<absolute dir>` in its environment (a pane inherits it). Every hook call then also
keeps its stdin as `<dir>/<agent>/<epoch>-<pid>-<word>.json` (mode 0600); OpenCode's plugin writes
`<dir>/opencode/<epoch>-<pid>-<n>-<hook>.json` holding `{input, output}` or `{event}`; pi's extension writes
`<dir>/pi/<epoch>-<pid>-<n>-<event>.json` holding `{event, ctx}` (ctx: session id, mode, idle, whether the session file exists). A folder of these drops into
`tests/fixtures/agents/<agent>/` as it is, with an `expect.json`, and `cargo test recorded_` replays it through the real script.
The payloads hold prompts and tool output: share them only after reading them.

## A stop and macOS

macOS keeps no record of who sent a signal; the `stopped by` line is the only one. To see what the
system was doing around it:

```sh
log show --style compact --start "YYYY-MM-DD HH:MM:SS" --end "YYYY-MM-DD HH:MM:SS" \
  | grep -iE 'nerd|launchd|logout|shutdown|backgroundtask|loginwindow'
```

Take a window of about 10 seconds on each side of the `stopped by` second.

## Crash reports

`~/Library/Logs/DiagnosticReports/*nerd*.ips`. The report already sums each one up: time, path,
exception and signal, `asi` messages (a Rust panic's abort shows here), and the faulting thread's
first frames, and `parent: <process> pid N`. `(a test binary)` after the parent: the test suite's deliberate
panic (`NERD_PANIC_TEST`), not a user's crash. A frame without a symbol is printed as `<image> + <offset>`. Rust symbols read by
their length-prefixed parts: `_RNv...4nerd6daemon3run` and `_ZN4nerd6daemon3run17h...E` are both
`nerd::daemon::run`; `rustfilt` demangles them if it is installed.

A jetsam line reads `date: …, largest process: …, killed: <name> (<reason>), …`: macOS ended those processes for
memory. A terminal app among them explains a window that vanished with no crash report.

## Reproducing

Use a test server, never the user's own:

```sh
HOME=/tmp/nt NERD_SOCKET=/tmp/nt/s.sock nerd
```

Its log is under `/tmp/nt/Library/Application Support/nerd/`. Stop it by its socket only, never by
process name, which would kill the user's own server too:

```sh
for p in $(pgrep -f 'nerd --daemon'); do lsof -p $p | grep -q /tmp/nt/ && kill $p; done
```

To test the sender line, send the signal with `/bin/kill -TERM <pid>`; a shell's `kill` is a builtin,
so the sender is the shell itself.
