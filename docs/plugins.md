# Plugins
<!-- summary: Installing plugins, what a plugin can do and read, and writing and publishing one. -->

A plugin is a folder with a `plugin.json` manifest. The manifest alone can describe an agent (how nerd finds it,
hooks into it and resumes it; see the README). A plugin that needs to do more also names an executable: nerd
starts it on first use, inside a sandbox of its own, and talks to it over nerd's socket.

nerd draws everything; a plugin only supplies data. Start from [`templates/plugin/`](../templates/plugin/), or from
one of the slot examples, each a real plugin with its own README: [`templates/badges/`](../templates/badges/) marks
big files in the tree and the tab, [`templates/segment/`](../templates/segment/) is a clickable clock segment,
[`templates/preview/`](../templates/preview/) previews `.log` files down to their ERROR and WARN lines, and
[`templates/panels/`](../templates/panels/) shows a list, a tree and a detail panel over a pane's own files.

## Two tiers

Every extension point has two sides:

- **The public API** is what goes over the plugin socket: the messages and events in this document. It is narrow and
  stable, and it only grows. A community plugin sees this and nothing else.
- **The internal API** is nerd's own code, for built-ins only. It is never on the socket and may change freely.

Ordinary built-ins use only the public API, through the same checks as a plugin's messages, which proves it is
enough: the CSV, JSON and Markdown previews are built this way. nerd's distinctive features stay internal: the git
status marks in the tree, image previews, the highlighting of other code, and Session history, which is drawn by
the same panel widget as a plugin's panels. A capability
starts internal; making it public is a deliberate decision, made in this document.

Public messages: `ReportState`, `Badges`, `Segment`, `SegmentClear`, `Preview`, `Panel`.
Public events: `connected`, `agent_state`, `pane_opened`, `pane_closed`, `file_chosen`, `command`, `preview_request`, `panel_opened`, `panel_item`, `pane_cwd`.

## Installing

**From GitHub or the catalog.** **Plugins…** in the main menu lists the installed plugins and the catalog's;
**Add from GitHub…** takes a public repository, `https://github.com/<owner>/<repo>`, with `@<tag>` or `@<commit>`
after it when you want one release. In a terminal: `nerd --plugin-add <address>[@tag]`. No other host, no ssh and no
private repository: nerd runs `git` with none of your or the system's git config, no home folder (so no netrc) and
without proxy, certificate or askpass variables, so nothing redirects, proxies or signs in to a fetch. A terminal also takes a local folder,
`nerd --plugin-add ./my-plugin`: for developing or debugging a plugin, or for installing code you checked yourself.
nerd fetches it with the system `git` into a staging folder, reads its files before anything is checked out (no
submodules, no link out of the folder, no file over 10 MB), checks it out with no filter or hook of yours, runs the
rules below, and shows what it can do. Nothing is placed in `plugins/` until you say yes. Without a tag nerd takes
the newest release tag (`v1.2.3` or `1.2.3`, pre-releases only when named) whose `min_nerd` it meets, and the
manifest's `version` must be that tag's.

Run from inside a nerd pane, where an agent might be the one typing, the command only fetches: the question comes
up in nerd's own window, built from the fetched files, and only a click there installs. `--yes` answers yes in a
terminal, is refused inside a pane (and in anything started from one, which carries the pane's mark), and never
covers a release that can do more than the installed one. This keeps an agent that plays by the rules from installing
on its own; it is no wall against code running as you, which can write the plugins folder anyway (see the threat model).

In the window a click chooses a row, and the buttons on its bottom line act on that row: turn off or on, update,
show log, remove. The window stays open; a right click on a plugin opens the same commands as a menu.

Add, install, update and refresh happen in the window itself; no tab opens. git runs hidden, outside the server's
sandbox, and never asks for anything. While it runs, the row's State and details say so ("Updating plug: fetching
with git…"), and cancel stops it. Once the release is fetched, the question above opens over the window, and after
your answer the window comes back with the same row chosen. When there is nothing newer, the details say so. When
the fetch fails, they say why in a line or two, and show log opens git's full output.

**By hand.** Put the folder at `~/Library/Application Support/nerd/plugins/<name>/` (the state root's `plugins/`;
the folder name is the manifest's `name`) and choose Reload plugins and agents; until then the Plugins window says
**waits for Reload**, and Enter on its row does it. A folder that appears there is never loaded on its own, so nothing
can slip a plugin in unseen. Such a plugin is never updated. A
manifest that breaks a rule is skipped, and `daemon.log` says why (`agent manifest skipped: <path>: <why>`).

**The lock.** `plugins.lock` in the state root records each plugin installed from git: its name, address, tag and
exact commit. `nerd --plugin-add --lock <file>` installs every plugin of a lock copied from another Mac, at the same
commits, asking once. A lock that does not parse is left as it is, and no install or update runs until it does.

**Updating.** `nerd --plugin-update [<name>]`, or **Update <name>…** in its menu, fetches again and offers the newest
release above the installed one, with its commits. When the new release can do anything the installed one could not,
it says what, and asks again. From the catalog, an update is the catalog's next reviewed release, never an upstream
tag the catalog has not seen. Nothing is updated on its own.

**Turning off and removing.** `nerd --plugin-off <name>` (or **Turn <name> off**) keeps it installed but never loads
it, and takes nerd's hooks out of its agent's config; `--plugin-on` brings both back. Which plugins are off is in
`plugins-off.json`, per Mac, never in the lock; when that file does not parse, no plugin of yours loads. `nerd
--plugin-remove <name>` deletes the folder and its lock line, and with `--data` its data folder too. From inside a pane,
`--plugin-on` and `--plugin-remove` are refused like `--yes`: turning a plugin back on or removing it is the Plugins
window's to do there.

**The catalog.** `nerd --plugin-catalog`, or **Refresh catalog**, fetches the catalog from
`https://github.com/nerdterm/nerd-plugins` into the state root's `catalog/`; nothing else ever fetches it. Each plugin
is marked **reviewed** (the maintainer approved that release), **checked** (risk level 0, approved by the catalog's
checks alone), **not in catalog** (another address or another commit than the catalog's), or **by hand**. A catalog
install takes exactly the commit the catalog reviewed, refuses when its tag no longer points there, and recomputes the
risk level instead of trusting the catalog's. A release the catalog revokes is turned off at the next refresh and
stays off until it is updated to the catalog's current release, or removed; turning it off and on does not lift it. A
soft revocation only warns, in the Plugins window and in every question about that release. The window also shows each
catalog plugin's GitHub stars, counted daily by the catalog's CI and fetched with the catalog, with the date a count
last changed; nerd asks GitHub nothing.

**The network.** Every fetch above is `git`, started by you, from the terminal or the Plugins window, to the
address shown. The server itself stays in a sandbox with no network.

## Checking a plugin

`nerd --plugin-check <folder>` runs the rules nerd loads a manifest by, against the built-ins and the plugins already
installed, plus two of its own: no link that leaves the folder, no file over 10 MB. It prints what the plugin can do
and its risk level, then every problem; `--json` prints the same as one object, `--catalog` adds what the catalog
needs (`version`, `min_nerd`, a `LICENSE` file). Exit 0 clean, 1 a problem, 2 a usage error.

Risk, from the lowest: only shows things, writes files, reaches the network, hooks into an agent; a plugin's risk is its
highest ability's. nerd says it in these words; `--json` and the catalog give it as a level, 0 to 3.

## Publishing a plugin

1. Put it in a public GitHub repository with an open source licence file.
2. Give `plugin.json` a `version` and a `min_nerd`, and tag each release `v<version>`.
3. `nerd --plugin-check --catalog <folder>` must say "No problems found". It prints what the plugin can do, in the
   words a user will be asked with, and its risk level.
4. `nerd --plugin-trial <folder>` runs it once in its sandbox with no network and no write paths, sends it each
   event it declares, and says whether it connected, what it sent, and what it printed. It must connect and keep
   running.
5. Anyone can already install it with `nerd --plugin-add <address>`, marked **not in catalog**. To enter the catalog,
   open a pull request on `nerdterm/nerd-plugins` that adds `plugins/<name>.json`; its README says what goes in it.

The catalog's review is automated and fails closed: the checks above, the licence, no compiled binaries or minified
code, a lockfile that pins every dependency, OSV-Scanner (including the OpenSSF malicious-packages list), GuardDog,
YARA, Gitleaks, and the trial run. What happens next depends on the risk level:

| Risk level | The plugin | Review |
|---|---|---|
| 0 | only shows things: badges, segments, previews, panels, menu items | approved when every check passes and the level did not rise: **checked** |
| 1 | writes to a folder it names | the maintainer reads it: **reviewed** |
| 2 | reaches the network: this Mac's services, or the hosts and ports it names | the maintainer reads it: **reviewed** |
| 3 | speaks for an agent: hooks into its config, reports its state | the maintainer reads it: **reviewed** |

A release whose risk level rises goes to the maintainer whatever it was before. The maintainer may ask for an AI
analysis on the pull request; it only adds findings. To update a plugin in the catalog, open a pull request that
changes its entry to the new tag and commit.

## Manifest

```json
{
  "name": "hello",
  "title": "Hello",
  "min_nerd": "0.6.0",
  "exec": { "command": "sh", "args": ["plugin.sh"] },
  "permissions": { "network": false, "write": [] },
  "commands": [{ "id": "hello", "title": "Say hello", "where": "global" }],
  "events": ["command", "pane_opened", "pane_closed"],
  "badges": ["tree", "tab"],
  "segments": [{ "id": "status" }],
  "previews": [{ "extensions": ["log"] }],
  "panels": [{ "id": "jobs", "title": "Jobs", "where": "global" }],
  "agent": { "names": ["hello"] }
}
```

| Field | Meaning |
|---|---|
| `name` | Lowercase letters, digits and dashes, not starting with a dash; the folder's name. A built-in's or another plugin's name is refused. |
| `title` | One line. |
| `min_nerd` | Optional `x.y.z`: an older nerd skips the manifest. |
| `version` | Optional `x.y.z`, with an optional pre-release (`1.2.0-rc.1`); no build metadata. |
| `platforms` | Optional: `macos`, `linux`, `windows`, each once. None listed means macOS only; a manifest that does not list `macos` is skipped here. |
| `exec.command` | What runs. A path with `/` inside the folder (`bin/run`), an absolute path, or a bare name found on `PATH` (then `/usr/local/bin`, `/opt/homebrew/bin`, `/usr/bin`, `/bin`). It is resolved once, when the manifest is read. `open`, `osascript` and `launchctl` are refused. A script's interpreter is the command: `"command": "node", "args": ["main.js"]`. |
| `exec.args` | Fixed words, one line each. It runs in its own folder. |
| `permissions.network` | How far it reaches. Absent or `false`: nowhere. `"localhost"`: services on this Mac only (IPv4 and IPv6), no name lookups. `{"hosts": ["wttr.in"], "ports": [443]}`: only those hosts on those ports (1 to 16 hosts, 1 to 8 ports), through nerd's proxy (see [Network](#network)); the program must honour `HTTPS_PROXY`. A host is a lowercase domain name whose last label starts with a letter, or an IP address in standard form (`192.168.1.5`, `2001:db8::1`; not `010.0.0.1`, `2130706433` or `[::1]`), never an unspecified, multicast or broadcast one. Outbound only: a plugin never listens, and reaches no unix socket but nerd's plugin socket (not the system's name lookup service, not an ssh agent's, not Docker's). `true` is refused, and nerd releases before these forms refuse them as a manifest that does not parse. Official nerdterm plugins use no network, or `"localhost"` at most. |
| `permissions.write` | Extra folders it may write, `/…` or `~/…`, no `..`, inside the home folder. Refused: the home folder itself, any path through a hidden folder or file (`~/.zshrc`, `~/.config`, `~/p/.git/hooks`, `~/p/.envrc`), `~/Library`, `~/Applications`, any folder on `PATH` or holding one, nerd's state root, a name outside ASCII, and every path when nerd has no home folder; letter case does not matter. Hidden entries below a write path stay unwritable. |
| `commands` | Up to 16 menu items: `id` (a token), `title` (one short line), `where`: `global`, `pane`, `tree` (on files and folders), `file` or `folder` (the tree's menu on that kind only). They follow a separator in that menu. Commands need `command` in `events`. |
| `events` | The events this plugin may be sent (below). What it subscribes to on `Connect` is cut down to these. |
| `badges` | Where it may put badges: `tree`, `tab`, each once. |
| `segments` | Up to 4 status segments, each an `id` (a token). |
| `previews` | File types it previews: `extensions`, lowercase without the dot (`csv`, not `.CSV`), at most 16 in all, each once. Previews need `preview_request` in `events`. |
| `panels` | Up to 4 panels: `id` (a token), `title` (one short line), `where`: `global`, `pane`, `tree`, `file` or `folder`, the menu that opens it, after the commands. |
| `agent` | Optional: the agent this plugin describes, as in the built-in manifests under `plugins/`. |

A manifest needs an `agent`, an `exec`, or both. `commands`, `events`, `badges`, `segments`, `previews`, `panels`
and `permissions` need an `exec`. `previews` is a built-in's name and is refused.

The plugin's environment: `NERD_SOCKET` (the plugin socket, below), `NERD_PLUGIN` (its name), `NERD_PLUGIN_DATA`
(its writable folder, `<state root>/plugin-data/<name>/`), and `TMPDIR` set to the same folder; of the rest only
`PATH`, `HOME`, `USER`, `LOGNAME`, `SHELL`, `LANG`, `LC_*` and `TZ` pass, so no token in nerd's environment reaches it.
`PATH` ends with the folders a bare `exec.command` is looked for in, so a plugin finds its own program by name.
Its stdout and stderr go to `output.log` there, made anew inside its sandbox at each start and never through a link.

## Protocol 2

One JSON object per line, both ways, on the **plugin socket**: `nerd.psock` next to nerd's own `nerd.sock` (mode
0600). It serves only `Connect`; anything else as the first line is refused. It is the one socket a plugin's sandbox
can reach: nerd's main socket, where windows attach and `--stop` is sent, is out of its reach.

The plugin's first line:

```json
{"Connect":{"protocol":2,"role":"plugin","plugin":"hello","events":["command","pane_opened"]}}
```

nerd answers `{"event":"connected","protocol":2}`, then sends the events the plugin both declared and subscribed
to, events that came while it was starting first. On any refusal nerd writes one line, `refused: <why>`, and closes
the socket. `Connect` on nerd's main socket is refused too.

Events, server to plugin:

```json
{"event":"command","id":"hello"}
{"event":"command","id":"lint","pane":3}
{"event":"command","id":"open","path":"/Users/me/proj/a.rs"}
{"event":"agent_state","pane":3,"agent":"claude","state":"working"}
{"event":"pane_opened","pane":3,"cwd":"/Users/me/proj"}
{"event":"pane_closed","pane":3}
{"event":"pane_cwd","pane":3,"cwd":"/Users/me/proj/src"}
{"event":"file_chosen","path":"/Users/me/proj/a.rs"}
{"event":"preview_request","req":12,"path":"/Users/me/proj/app.log","cols":60,"rows":20,"mode":"table"}
{"event":"panel_opened","id":"jobs","pane":3}
{"event":"panel_item","panel":"jobs","item":"job-7"}
```

`command` and `panel_opened` carry `pane` for a pane menu's item and `path` for a tree menu's; neither for the main
menu's. `agent_state` fires when a pane's agent
or its state changes; the states are `working`, `idle`, `blocked`, `done`, `failed`, `unknown`. `pane_cwd` fires when a pane's shell has moved to another folder, as nerd reads it once a second; a plugin that
started later hears the folder a pane is in now in its `pane_opened`. `file_chosen` fires when
a file is opened from the tree, in the editor or with the system. Whenever a plugin connects, every pane
already open is a `pane_opened` with the folder it is in now, and every agent's state an `agent_state`. `preview_request`, `panel_opened` and `panel_item` are below.

Messages, plugin to server:

```json
{"ReportState":{"pane":3,"state":"blocked"}}
```

```json
{"Badges":{"target":"tree","items":[{"path":"/Users/me/proj/src/a.rs","text":"3","role":"warn"}]}}
{"Badges":{"target":"tab","items":[{"pane":3,"text":"ok","role":"ok"}]}}
{"Segment":{"id":"status","text":"2 failing","role":"bad","command":"hello"}}
{"SegmentClear":{"id":"status"}}
{"Preview":{"req":12,"lines":[[{"text":"12:00 ","role":"faint"},{"text":"started","role":"ok"}]],"cut":false}}
{"Panel":{"id":"jobs","kind":"list","title":"Jobs","items":[{"id":"job-7","label":"build","detail":"2 min","role":"ok"}]}}
```

`ReportState` is taken only for a pane whose agent is this plugin's own (`agent.names`), and only as `working`,
`idle`, `blocked` or `failed`. It writes the pane's state where the agent hooks write it, so the sidebar shows it
the same way, and only from the plugin's current connection.

`Badges` replaces everything this plugin showed on that target; an empty `items` clears it. A `tree` badge sits on
the row whose path is exactly `path`, after the git mark and before the size; nothing is summed into folders. Paths
must be absolute, without `..`, and canonical, as the sandbox's are (`/private/tmp/x`, not `/tmp/x`, which matches
nothing). A `tab` badge sits after the tab's name, set off from it by ` · `; a tab shows one badge per plugin, its
focused pane's first. nerd puts no badge of its own on a tab. A count carries its unit word (`12 todo`, so the tab
reads `1 · 12 todo`, not `1 · 12`); when the row is short badges drop before any tab does, the separator with them.

`Segment` sets one of the manifest's segments; `SegmentClear` takes it away. Segments sit in the top row's free
space, right-aligned before nerd's battery, its clock and `@ machine`, ` · ` apart: plugins by name, each plugin's in manifest
order. When the row is short the rightmost go first and the clock stays; tabs keep their room. A notice lies over
them for as long as it shows. A segment with a `command` (one of the manifest's `commands`) runs it when clicked,
exactly as its menu item does.

`role` is one of `text` (the default), `muted`, `faint`, `accent`, `ok`, `warn`, `bad`, `teal`: the theme's
colours, never a raw one. Text loses control and bidi characters; a `tree` badge is at most 4 cells wide, a `tab`
badge 8 (room for a count and its unit word), a segment 24.

The rules, each refusing the whole message: a slot the manifest does not declare; an unknown role; more than 5000
items; text over its width; a preview or a panel over its lines or cells, or a panel over its depth or with an empty item `id`; an unknown panel `kind`; a path that is not absolute or holds `..`; a `tree` item without `path` or a `tab` item
without `pane`; a segment `command` that is not the plugin's. Each connection logs its first over-cap message at
WARN, and each plugin its first other refusal; a line that is not a message at all is logged once per connection. nerd keeps the latest message per target, segment, panel or preview, takes at most 20 batches a second from a plugin and
draws them at most once a frame, so a flood costs nothing but the plugin's own time; the last message always
counts. Anything else a plugin sends is ignored, a screen message (`Hello`, `Shutdown` and the rest) included.

**Previews.** When a file is selected in the tree, nerd shows its own preview at once and sends `preview_request` to
the one source that previews the file's extension: nerd's built-in for `csv`, `json`, `md` and `markdown` (a plugin
declaring one of these is not asked for it), else the plugin whose name sorts first among those declaring it. `cols` and `rows` are the room the preview had when asked, `0`
before it was first drawn. The answer, `Preview` with the same `req`, replaces nerd's preview if that file is still
the selected one; a later answer, one for another request, or one from a plugin that was not asked is dropped. nerd
never waits: a plugin that does not answer leaves nerd's preview in place. `lines` are runs of text with a `role`, at
most 2000 lines of at most 512 cells; `cut` says the plugin left some out, and nerd says so under the view. The
preview's header names the plugin that drew it. Nothing is asked for a folder, a search hit's line, or inside the Changes view, where the diff stays.

**Views.** A `Preview` may offer up to 4 ways to show the file, `modes: [{"id": "table", "label": "Table"}, …]` (an
`id` of at most 64 bytes, a `label` of at most 16 cells), and say which one its `lines` are with `mode`. With two or
more, the header names them all (`… · table · raw`, in lowercase, the one shown in bold), and a click on another, **v** or **Next
view** in the tree's menu asks for it: nerd sends `preview_request` again with `"mode": "<id>"`, and remembers that view for this plugin, so the next file is
asked for in it too. A plugin that does not offer the mode it was asked for answers in its own choice. Wrapping long
lines is nerd's (**w**), not a view.

`preview_request` tells the plugin the full path of the selected file, whenever it is of a type the plugin declared.
This is a deliberate public decision: a preview plugin has to read the file, and its sandbox already lets it read the
user's files (below); the request reveals which file the user looks at, and when. Only the one plugin that would
answer is told, and only for its declared types.

**Panels.** A panel is a dialog nerd draws, opened from the menu its manifest names; nerd sends `panel_opened` each
time, so the plugin can send fresh content. `Panel` replaces a panel's content whether it is open or not; nerd keeps
the latest and draws it. A panel opened from a pane's or a tree row's menu starts empty ("Waiting for the plugin…")
until the plugin answers: what it showed belonged to the pane or row it was opened from last. A `detail` longer than
the window scrolls, by the wheel, Page Up and Page Down, Home and End, and the arrows when it has one row. `kind` is
`list`, `tree` or `detail`:

- `list`: `items`, each an `id` (a string the plugin chooses, at most 1024 bytes), a `label`, an optional `detail`
  shown after it in `faint`, and a `role` for the label. A filter line over the rows narrows them as the user types.
- `tree`: the same, with `children` (at most 8 deep). A row with children opens and closes when clicked; the filter
  shows every match, at any depth, flat.
- `detail`: its `items` (usually one), then `lines`, styled like a preview's (at most 2000 lines of 512 cells).

Clicking a row that has no children, or Enter on it, sends `panel_item` with that row's `id` and closes the panel.
A row with `"opens": true` sends the same `panel_item` but keeps the panel open, its filter emptied and scrolled to
the top, for the content the plugin sends next: a session's text after a list of sessions, say. The plugin draws its
own way back, as an ordinary row (`opens` too, so the list comes back in the same panel). In a tree, a row with
children is a branch and opens or closes; `opens` on it is ignored. A nerd older than this field
ignores it and closes the panel; set `min_nerd` to rely on it.
The selection is kept by `id` when new content replaces the old, and so are the open branches; a panel shows up to 15
rows and scrolls the rest. All `items` count toward the cap of 5000, at any depth; a `title` is at most 64 cells and
replaces the manifest's while it is set.

**Against a v1 daemon** (a nerd before protocol 2): there is no plugin socket, so the connect fails; and a `Connect`
sent to a v1 main socket is a message it does not know, so it closes the socket without a word. A plugin that cannot
connect, or reads EOF before `connected`, must exit. Nothing attaches, nothing is granted.

**Compatibility contract.** The protocol only grows. A new event, message or field is added, never renamed or
removed; a plugin ignores events and fields it does not know. v1 clients (`Hello` and the rest) keep working
unchanged, and `Hello` stays screen-only. The `protocol` a plugin sends is the lowest it needs; nerd answers with
what it speaks.

## Roles and authorization

The only role is `plugin`. A `Connect` for plugin X is accepted only when the connecting process is the process the
broker returned when it launched X, or descends from it. That pid proves X only while X lives: it is dropped the moment
the broker reaps the process, so a later process given the same pid proves nothing. A second connection for a
plugin that already connected is refused and closed. Refusals are logged at WARN.

A plugin cannot attach a screen, stop, update or disconnect nerd: its sandbox reaches only the plugin socket, which
serves nothing but `Connect`, and cannot run the `nerd` binary. Programs in panes may stop or update nerd, in Settings → Messages, never covers a plugin.
This holds for its descendants too, a double-forked one included: the sandbox is inherited and cannot be left.

**Threat model.** The sandbox and the pid proof keep a plugin from acting outside what its manifest declares, and keep
anything else on the machine from speaking as a plugin. They do not guard against code already running as the same
user outside nerd, which can signal nerd or edit its files anyway. `permissions` are trust: a network tier or a
`write` path is granted as written, so read them before installing a plugin. Ports and `"localhost"` are enforced by the
sandbox, hosts by the plugin's proxy; an update that widens any of them (a new tier, port or host) asks again.

**What a plugin can read.** Every file the user can, except the folders `~/.ssh`, `~/.aws`, `~/.gnupg`, `~/.docker`,
`~/.kube`, `~/.azure`, `~/.config/gh`, `~/.config/gcloud`, the whole of `~/Library` (keychains, cookies, browsers, mail)
and nerd's state root (sessions, screens, logs, other plugins and their data); the files `~/.netrc`, `~/.npmrc` and
`~/.pypirc`; the shell histories `~/.zsh_history`, `~/.zsh_sessions`, `~/.bash_history`, `~/.bash_sessions`,
`~/.sh_history`, `~/.history`, `~/.local/share/fish` and `~/.local/share/atuin`; and the histories of python, node,
psql, mysql, sqlite3, irb and less: `~/.python_history`, `~/.node_repl_history`, `~/.psql_history`, `~/.mysql_history`,
`~/.sqlite_history`, `~/.irb_history` and `~/.lesshst`. A denied file covers every name that begins with it
(`~/.zsh_history.new`, where zsh writes the history before renaming it). A link inside a denied folder other than
`~/Library` and nerd's state root, at most four levels down and among its first 2000 entries, denies what it points
to, and so does a denied file or folder that is a link, even to something not yet there, unless the target is your
home folder or above it, or a device such as `/dev/null` (how MySQL turns its history off). These rules are fixed when
nerd reads the plugin's manifest, so a link added later counts after Reload plugins and agents. A history kept anywhere else (another `HISTFILE`, `ZDOTDIR` or
`XDG_DATA_HOME`) is readable. Inside the state root a plugin reads only its own folder and data folder. It never opens
a terminal device, so what you type into a pane or into nerd stays out of its reach. **It can read the command line
and the environment of every program you run**: macOS gives those to any process of the same user, and the sandbox
has no rule against it, so a key
in a pane's environment (`ANTHROPIC_API_KEY`, `GH_TOKEN`) is readable by any plugin. **With any network it could send anything it can
read**: with hosts and ports to those hosts, which may pass it on; with `"localhost"`, to any service on this Mac, which
may pass it on too. A `write` path is a way out too, when that folder is synced or shared. A program that later reads a written
file may act on what the plugin put there, which is why the paths above are refused.

**Detached descendants.** A process a plugin detaches (double fork, or its own `setsid`) stays in the sandbox, but a
new session is outside the plugin's process group, so the group kill at stop may not reach it; it can outlive the plugin.

## Sandbox

The broker, the one part of nerd outside the server's sandbox, launches `nerd --plugin-exec`, which applies the
profile nerd built from the validated manifest (never the manifest itself) and then becomes the plugin. The profile:

```
(version 1)
(deny default)
(allow process-fork)
(allow process-exec (subpath "/bin") (subpath "/usr/bin") (subpath "<plugin folder>") (literal "<command>"))
(deny process-exec (literal "/usr/bin/open") (literal "/usr/bin/osascript") (literal "/bin/launchctl"))
(allow file-read*)
(deny file-read-data (subpath "~/.ssh") (subpath "~/.aws") ... (subpath "~/Library") (subpath "<state root>"))
(allow file-read-data (subpath "<plugin folder>") (subpath "<data folder>"))
(deny file-read* file-write* file-ioctl (regex #"^/dev/(tty|ttys[0-9]+|ttyp[0-9a-f]|pty[p-z][0-9a-f]|ptmx)$"))
(allow file-write* (literal "/dev/null") (subpath "<data folder>") (subpath "<each write path>"))
(deny file-write* (regex #"^<each write path>/(.*/)?\.[^/]*"))   ; only with write paths
(allow sysctl-read)
(allow signal (target same-sandbox))
(allow network-outbound (remote unix-socket (path-literal "<the plugin socket>")))
(allow network-outbound (remote ip "localhost:*"))                   ; only with "network": "localhost"
(allow network-outbound (remote ip "localhost:<proxy port>"))        ; only with hosts and ports, added by the wrapper
```

So by default a plugin cannot reach the network or DNS, connect to any other local socket, open apps or URLs, send
Apple events, submit launchd jobs, reach nerd's main socket, read the denied paths above, write outside its data folder
(its own folder and `plugin.json` included), or run anything written there. Paths are canonical (`/private/tmp`, not
`/tmp`), and the last matching rule wins, which is why its own folders follow the deny.

### Network

Seatbelt filters by port, not by name, so a plugin with hosts and ports reaches the network only through a proxy.
`nerd --plugin-exec` binds a port on both loopbacks, forks the plugin as its child with `HTTPS_PROXY`, `https_proxy`,
`ALL_PROXY` and `all_proxy` set to `http://127.0.0.1:<port>` (and `NO_PROXY` empty), and stays as its proxy, leading
its own process group (or it stops the plugin and exits):

- The plugin's profile reaches that port on this Mac's addresses and nothing else: no other address, no name lookups.
  TCP through `CONNECT` only: no UDP, and no plain `http://`, even to a host on port 80. The program must honour
  `HTTPS_PROXY`: curl, Python and Go do; Node's built-in `fetch` does only with `--use-env-proxy` (Node 22.21 or 24.5
  and later) in `exec.args`, since nerd passes no `NODE_USE_ENV_PROXY`.
- A Mac without the IPv6 loopback (`::1`) cannot start a plugin with hosts and ports: the wrapper needs both
  loopbacks on one port, and fails rather than leave one open to another program.
- The proxy answers only connections from its plugin's process group (a child that calls `setsid` leaves the group and
  loses the network), and only `CONNECT host:port` for a host and port the manifest names. A name is compared
  lowercased, without one trailing dot; it is looked up by the proxy, and only its public internet addresses are
  connected to: none of this Mac's own addresses or on a network one of them is on (read on every request, IPv6
  included), and no private, loopback, link-local, carrier-grade NAT, documentation, relay, multicast or reserved
  range (for IPv6, `2000::/3` without Teredo, benchmarking, 6to4 and documentation; the well-known NAT64 prefix
  `64:ff9b::/96` is judged by its IPv4). An IP address in the manifest is connected to as written, with no lookup,
  private ones included: the user was shown it, and one on this Mac or its network is named so.
- At most 16 tunnels at once; a request head of 8 KiB within 10 seconds; a lookup within 10 seconds; a connection
  within 5 seconds per address and 10 in all; a tunnel closes after 300 seconds with no data in either direction, and
  one side's end of sending is passed on to the other.
- It runs in a sandbox of its own: it reads what a plugin may read, writes only `daemon.log` and `daemon.log.old`,
  keeps off every terminal device, accepts only on its port and connects only to the manifest's ports. Each tunnel is
  logged when it opens and when it closes, each refusal with its reason (`plugin … proxy: wttr.in:443 allowed (…)`,
  `… closed, N sent, M received`, `… refused: not in the manifest`), and a lookup or connection that fails as
  `… failed: …`; past 5 such lines a second they are counted by kind into the next line, or into one before it ends. Refusals of
  programs outside the plugin's group are limited apart, one a second with a count, so they cannot crowd those out.
- The proxy and the plugin end together: it ignores `SIGTERM` and `SIGHUP`, which reach the plugin through the group,
  and leaves with the plugin's exit status or its signal; if the proxy dies, the broker kills the group. A plugin that
  crashes (`SIGSEGV`, `SIGABRT` and the like) is logged by the proxy, which exits with 128 plus the signal's number
  rather than crash itself.

What it cannot see: a name's owner chooses which public address it reaches, and nerd only keeps it off this Mac and
the networks it is directly on. Other subnets of the same home or office, networks reached through a VPN's routes,
and your own network's public IPv4 address (which the router may loop back inside) are not recognised. Inside the tunnel, TLS may ask a shared server (a CDN) for another site than the name it connected to.
A network-specific NAT64 prefix is not recognised, so on a network with one, a name could reach a private IPv4 address
through its gateway; the local-use prefix `64:ff9b:1::/48` places the IPv4 where only the network knows, so it is
refused. Seatbelt's `localhost:<port>` admits that port on every address of this Mac, so a program of yours that took
the same port on another of its addresses could receive what the plugin sends. A server from an older build passes no
hosts to the wrapper, so its plugins keep the old rule (every address on their ports, and name lookups); a newer
server's plugin under an older binary has no network at all.

## Lifecycle

- **Lazy start.** A plugin starts the first time one of its menu commands is chosen or an event it declared happens.
  The start is asynchronous: the server never waits on it.
- **Slot plugins start with the server.** A plugin that declares `badges`, `segments`, `previews` or `panels` starts when the server does,
  and again after an update in place or Reload plugins and agents. What it showed goes the moment its connection ends, it
  stops or it is turned off; after an update it shows again once it has reconnected and sent it.
- **Handshake.** It must send `Connect` within 5 s, or it is stopped.
- **Its socket closes, it exits.** When the connection ends nerd ends it anyway, and that exit is a crash.
- **Stop.** nerd sends SIGTERM to its whole process group, SIGKILL 250 ms later. A plugin runs in a session of its
  own, so the group is everything it started.
- **Crashes.** Any exit other than nerd's own stop (server stop, update, Reload plugins and agents) is a crash, before or after
  `Connect`; so are a failed launch and a missed handshake. Each is logged with its count. After 5 crashes within
  180 s the plugin is off (logged at ERROR); choosing its command says so. Reload plugins and agents turns it back on. A command
  chosen while a plugin is being stopped is answered with a notice to choose it again.
- **Reload plugins and agents** reads the manifests again and stops running plugins; they start again on next use, slot plugins
  at once.
- **Update in place.** Plugins are stopped before the new build takes over and start again on first use; those exits
  are not crashes. When the running broker is from a build before plugins, which only opens panes, the new build asks
  it for one hidden pane running this build as a second broker, used only for plugins. It connects back once, over a
  socket in the server's folder (mode 0600) with a one-time token, within 10 s, and only the process whose pid the old
  broker reported for it is accepted: the token alone is not proof, since a plugin allowed the network could read it
  and reach the socket. It ends with the server's side at the next update or stop, taking its plugins with it, and a
  fresh one is started. No pane is closed. Events in the seconds before it connects reach no plugin. If it does not
  connect, or ends early, plugins stay off until the next update or a restart ("restart the server to enable plugins").
- **Server stop.** Every plugin's process group is killed; nothing is left running.

## Config

`plugins` in config.json is kept exactly as written, for per-plugin settings; nerd itself reads nothing from it yet.
