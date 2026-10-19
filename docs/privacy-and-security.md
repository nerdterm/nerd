# Privacy and security

- **The core.** nerd sends no telemetry and never checks for updates on its own. The server locks itself into a macOS Seatbelt sandbox that denies it the network: `nerd --sandbox-selftest` shows the profile refusing it, and `nerd --list` shows whether each server has it. nerd's only network code is the proxy it runs for a plugin with network, or for a pane set to Only these providers: outside the server's sandbox, in a sandbox of its own, it connects only to the hosts and ports that the plugin's manifest or the chosen agents' manifests name. Everything that can leave nerd is in [What leaves nerd](#what-leaves-nerd); [SECURITY.md](../SECURITY.md) says what each protection covers and what it does not.
- **Terminal output is contained.** Escape sequences printed by programs are interpreted inside nerd and never reach your terminal.
- **What is stored.** Everything is under `~/Library/Application Support/nerd/`, private to your user:
  - the layout, working directories, commands, and each agent's session id with the agent it belongs to;
  - the resume command of each agent you confirmed when its hook was installed;
  - pane text, written when the server stops and deleted when it starts again, kept out of Time Machine and Spotlight. It holds whatever your panes printed. Set `restore_screens` to `false` to turn it off.
- **Dependencies** are permissively licensed and checked with `cargo deny`.

## How it works

nerd is a client and a server. `nerd` starts a server in the background if needed and connects to it. The server owns the panes, runs their programs and draws the screen; the client passes your keys and mouse to it and shows what it draws. Closing the window only disconnects the client.

Each pane is a pseudo-terminal read by a terminal emulator library. nerd reads each pane's process tree once a second to see which agent runs there; the hook reports a supported agent's state directly.

## What leaves nerd

Each way out starts from something you do or approved, and writes a line to `daemon.log`.

| Way out | When | Where to | What it carries | Logged as |
|---|---|---|---|---|
| <!-- egress:git-fetch --> `git` | You add a plugin, update it or check for updates, or refresh the catalog (**Plugins…**, `nerd --plugin-add`, `--plugin-update`, `--plugin-catalog`). Never in the background. | The public GitHub address shown in the window. | git's own requests for that repository, from a `git` run with none of your git config, no home folder, no credentials and no proxy settings. | `git clone …` |
| <!-- egress:ssh --> `ssh` | You connect to another computer (**Connect to another computer…**, `nerd --remote`). | The host you named in `remotes`, reached through your own `~/.ssh/config`, keys and agent, so a `ProxyJump` there decides the path. | nerd's protocol to the nerd on that machine, including what you type and what its panes print. | `remote: ssh …` |
| <!-- egress:plugin-network --> A plugin with network | A plugin you installed with a network permission runs; approving it at install is what lets it reach the network later. | `"localhost"`: services on this Mac only, and whatever they pass on. Hosts and ports: only those hosts on those ports, through a proxy nerd runs for that plugin alone in a sandbox of its own; the plugin itself reaches only that proxy's port on this Mac's addresses and looks up no name. A name is reached at whatever public address its owner gives it, never this Mac or a network it is on; an IP address in the manifest is reached as written, and one on your local network or this Mac is named so when you are asked. The plugin must honour `HTTPS_PROXY`; plain `http://` does not work. | Anything the plugin can read, the command line and environment of your other programs included (macOS lets any of your processes read those). | `plugin … started: network …`; each connection and refusal as `plugin … proxy: …` |
| <!-- egress:pane-network --> A pane set to Only these providers | You choose **Isolation…** → Network: Only these providers for a pane, and its program starts. | Only the chosen agents' or chosen providers' hosts, on the ports their manifests name, through a proxy nerd runs for that pane alone in a sandbox of its own; the pane's programs reach only that proxy's port on this Mac and look up no names. The agent must honour `HTTPS_PROXY`; one that does not gets no network. | Anything the pane's programs send. | `pane N proxy: …` |
| <!-- egress:link --> A link or a file | You open a link in a pane, or a file with **Open in its app**. | `http(s)` in your browser, `mailto:` in your mail app, a file in its app. Files that hold an address of their own (`.webloc`, `.inetloc`, `.url`, `.fileloc`, `.ftploc`, `.afploc`, `.vncloc`, `.mailloc`) are not opened; a page opened in the browser may still load addresses of its own, and a file opened in its app runs as that app does (a `.command` or an app runs code as you). | The address. | `link opened …`, with the host for `http(s)` and only the kind otherwise |
| <!-- egress:clipboard --> The clipboard | You copy. | The macOS clipboard, and through OSC 52 the terminal you run nerd in, which may be on another machine over `ssh`. | The text you copied. | `copied N characters` |
| <!-- egress:agent-inbox --> Claude Code's inbox | A message from another pane, or from you, is delivered to a Claude Code pane whose session reported its inbox socket. | That session's own socket on this Mac, used only after nerd has checked that a process of that pane listens on it. | The message, its first line naming the sender: `[message from <name> via nerd, id <id>; …]`. | `message … via inbox: delivered` |

Outside the table, and said here so nothing is implied:

- <!-- egress:pane --> **Panes.** The programs in your panes, agents included, run as you unless their isolation says otherwise (see [Isolation](isolation.md)), outside the server's sandbox and outside this table: a shell you open, a log viewer nerd opens from a menu (`less` on its own log), or an agent's resume command you confirmed once, which a restored pane types for you. nerd also hands a message to an agent in a pane: at the end of its turn as its hook's own answer (`block`, `deny` or `followup_message`), through the pi or OpenCode extension, or, for an idle agent with no better path, typed in with nerd's frame line first and a carriage return after it; never into a pane waiting for your answer.
- <!-- egress:sandbox-off --> **The sandbox off.** With `sandbox` set to `false`, or when the sandbox fails to start (the server keeps running; `nerd --list` and every window say so), the server is not kept off the network until the sandbox is back.

Links are found in the text a pane prints; OSC 8 hyperlinks are not read. Anything else that leaves nerd is a bug: report it as a vulnerability (see [SECURITY.md](../SECURITY.md)).

## What a program inside a pane can do

An agent in a pane runs shell commands as you, so it could type `nerd --stop` or `nerd --update`. The server refuses a connection from a process that descends from its own panes when it asks to stop, update, disconnect everyone or take the screen; the command prints `refused: … from inside a pane`, exits non-zero, and the server logs it. `nerd --stop`, `--update` and `--disconnect` without a name, run inside any nerd pane, refuse too: name the server, or set `NERD_SOCKET` or `NERD_STATE_DIR` to target a test server. **Programs in panes may stop or update nerd**, in **Settings… → Messages**, turns both refusals off. Plugins follow the same rule: from a pane, `nerd --plugin-add` only fetches, and the question comes up in nerd's own window, where only a click installs; `--yes`, `--plugin-on` and `--plugin-remove` are refused.

The way in is the message socket. Panes of admitted accounts reach it through `/tmp/nerd-<uid>/`, a folder only nerd's owner can write, checked before use, and reach nothing else of nerd. No path in nerd lets an agent change names, routes, isolation or that switch while nerd runs; from an isolated pane, no file can either.

This guards against accidents, not against code that means it: a program that detaches from the pane (a double fork) is no longer its descendant, and any program running as you can stop the server with a signal anyway.
