# Usage
<!-- summary: The mouse, the keys, every command, and updating in place. -->

## Mouse

| Action | Result |
|---|---|
| Right click | Menu for the pane, tab, space, sidebar row or file under the pointer |
| Click | Focus a pane, switch a tab, pick a sidebar entry, press a button |
| Drag a border | Resize panes, the sidebar, the file tree or the preview |
| Drag over text | Select and copy; double click selects a word, triple click a line |
| Ctrl+click | Open the link or file path under the pointer |
| Wheel | Scroll the pane's history |

## Keys

Every key repeats a menu item or a click. nerd's key is `⌃`+`B` (Control + B, as in tmux): press it, let go, then press one of the keys below without `⌥` and without `shift`. `⌃`+`B` twice sends `⌃`+`B` to the program in the pane (the shell's back one character, vim's page up). With nerd inside tmux, tmux takes the first `⌃`+`B`: press it twice to reach nerd. With tmux inside a nerd pane, nerd takes the first: press it twice to reach tmux. The key after it is read from the key itself, not from what the layout prints on it, and its case does not matter, so it works the same on a Cyrillic layout; `⌃`+`B` is read off its key too, where the terminal sends Control with the layout's letter. While nerd waits for the second key, the top row shows `⌃+B`. **Settings… → tmux-style prefix** switches it to `⌃`+`A` or turns it off.

With a terminal that sends Option as Alt, `⌥` and the same key does the same in one press (see below). **Keys and mouse…** in the main menu lists them all.

| Key, after `⌃`+`B` or with `⌥` | Action |
|---|---|
| `⌥`+`K` | Keys and mouse: every key and click |
| `⌥`+`M` | Main menu |
| `⌥`+`/` | Search the pane and its history; again to close |
| `⌥`+`T` | Show or hide the file tree |
| `⌥`+`N` | Navigator |
| `⌥`+`,` | Settings |
| `⌥`+`C` | New tab |
| `⌥`+`W` | Close the pane, asking first |
| `⌥`+`-` | Split the pane down |
| `⌥`+`\` | Split the pane right |
| `⌥`+`Z` | Full screen for the pane; again to leave |
| `⌥`+`S` | Collapse or expand the sidebar |
| `⌥`+`U` | Update to the installed build; panes keep running |
| `⌥`+`Q` | Detach, asking first; the server keeps running |
| `⌥`+`1`…`9` | Switch to that tab |
| an arrow after `⌃`+`B`, or `⌥`+`shift`+arrow | The next pane in that direction; from the last pane, the area beyond it: the sidebar, the file tree, or the tab bar |
| arrows, `return` | Inside the sidebar or the tab bar |
| `Esc` | Close a menu or dialog, or leave an area for the pane |

The file tree's own keys (**Tree** in **Keys and mouse…**) work after `⌃`+`B` too, while the tree has the keys.

### Terminal presets

`⌥` and a key needs the terminal to send Option as Alt, which no macOS terminal does on every layout out of the box. `nerd --preset <terminal>`, or **Settings… → Terminal preset…**, sets it up for Ghostty, kitty, WezTerm or iTerm2, together with a 16 point margin and nerd's colours. It shows every change first and makes it only on your word; `nerd --preset <terminal> --undo` takes it back, and `nerd --preset` alone says which are applied. What each one does:

| Terminal | The preset |
|---|---|
| Ghostty | Writes `ghostty-preset.conf` and two colour files in nerd's folder: `macos-option-as-alt = left`, `window-padding-x/y = 16` with `window-padding-balance`, `theme = light:…,dark:…` with nerd's colours, and the Option keys bound to their physical keys, so they work on a non-Latin layout, where Ghostty 1.3.1 drops them otherwise. Adds `config-file = ?…` in a marked block at the end of the Ghostty config it loads last (`config.ghostty` in `~/.config/ghostty` when there is none). An include is read after the file that names it, so the preset wins over the same settings there. It replaces `nerd --install-keys`, whose line it takes out; until then the list says "old keys link" and nerd keeps that link's key file up to date |
| kitty | Writes `kitty-preset.conf` in nerd's folder: `macos_option_as_alt left`, `window_padding_width 16` and nerd's colours for the appearance at the time; adds `include …` in a marked block at the end of kitty.conf, made if missing. Restart kitty: reloading the config does not change Option |
| WezTerm | Writes `wezterm-preset.lua` in nerd's folder, a function that sets the left Option key to send Alt, a 16 pt margin and nerd's colours for the current appearance. With no WezTerm config, nerd makes `~/.wezterm.lua` that calls it. A config of yours that ends with `return config` (`config` built in it by `wezterm.config_builder()` or `{`, under any name) gets one marked line right before that return. Any other config is not edited: nerd shows the one line to add, the window copies it, and the preset reads "needs your line" until the line is there |
| iTerm2 | Adds a dynamic profile named nerd (left Option key Esc+, right Option normal, nerd's colours for light and dark) and sets iTerm2's margins, which every profile shares, to 16, keeping the values before for undo, which puts back each margin still at 16 and leaves one you changed since. It also makes nerd the default profile, and undo puts the one before back if nerd is still the default. iTerm2 takes the default profile and the margins on its next start; then new windows use nerd, and windows it restores keep their profile. Quit iTerm2 before undo to get the default profile before back: an open iTerm2 makes its first profile the default instead. When HOME is not your account's home, the margins and the default profile are left as they are |

The right Option key keeps typing the layout's symbols, such as — and €. nerd rewrites its own preset files when it starts, so a new build's keys and your `colors` reach the terminal; a theme changed later reaches it at the next start.

## Commands

| Command | What it does |
|---|---|
| `nerd` | Reopen the server a session on this Mac attached to last. When another session shows it, nerd asks first: **take it over**, **new server** or **cancel**. With no such server, or from inside a pane, it starts a new one |
| `nerd --new` | Make a new server, with its own desktop, in the current folder. Also **New server…** in **Servers…**, which moves the session to it |
| `nerd --resume [NAME]` | Return to a server by name or number; by default the one a session on this Mac attached to last. Also **Open** in **Servers…** |
| `nerd --rename N [NAME]` | Name a server, or clear its name. Also **Rename this server…**, and **Rename…** in **Servers…** |
| `nerd --remove NAME` | Remove a stopped server: its layout, pane text, messages, routes, logs and saved agent session ids. config.json, plugins and the agents' own sessions stay. A running server, the legacy one and the server a pane belongs to are refused, and so is the command from inside a pane. Also **Remove…** in **Servers…** |
| `nerd --remove --stopped` | List every stopped server, ask `y` or `n`, then remove each the same way. Also **Remove all stopped…** in **Servers…** |
| `nerd --list` | Servers, stopped, running or in a session, with their names, when a session last attached, sandbox and spaces; then any `nerd --daemon` no listed server leads to, as a stray. Also **Servers…** |
| `nerd --stop [NAME]` | Stop a server, or all of them. Also **Stop this server…**, and **Stop…** in **Servers…**, for a stray too |
| `nerd --update [NAME]` | Switch running servers to the installed build; panes keep running. Also **Update…**, and **Update…** in **Servers…** |
| `nerd --check-update` | Ask GitHub for the newest release and show its changelog; from a terminal outside nerd's panes, install it after you answer `y`. Also **Check for a new release…** |
| `nerd --disconnect [NAME]` | End every connection to a server, or to all; programs keep running. Also **Disconnect everyone…** |
| `nerd --remote M [NAME]` | Open a server on machine `M` from `remotes` in config.json, over ssh. Also **Servers…**: open the machine's section, then **Open** on a server |
| `nerd --daemon` | Start a server with no window; nerd does this itself when a window needs one |
| `nerd --install-hook` | Install the hook into every supported agent found. Also **Settings… → Agent hooks** |
| `nerd --uninstall-hook` | Take the hook out of every agent |
| `nerd --install` | Start nerd at login |
| `nerd --uninstall` | Stop starting nerd at login, and undo every terminal preset |
| `nerd --preset [TERMINAL] [--undo]` | Option as Alt, a margin and nerd's colours in `ghostty`, `kitty`, `wezterm` or `iterm2`, after showing every change; `--undo` takes it back; alone, which are applied. Also **Settings… → Terminal preset…**. It replaces `--install-keys` and `--uninstall-keys` |
| `nerd --plugin-add URL[@TAG]` | Install a plugin from a public GitHub repository or a local folder, after showing what it can do. Also **Plugins…** |
| `nerd --plugin-update [NAME]` | Install a plugin's newer release, asking again when it can do more |
| `nerd --plugin-remove NAME` | Remove a plugin; `--data` also deletes its data |
| `nerd --plugin-off NAME`, `--plugin-on NAME` | Turn a plugin off or on |
| `nerd --plugin-catalog` | Fetch the plugin catalog; a release it revokes is turned off |
| `nerd --plugin-check DIR`, `--plugin-trial DIR` | For plugin authors: what a plugin can do and any problem; one run in its sandbox with no network |
| `nerd --sandbox-selftest` | Test the server's sandbox profile on this Mac |
| `nerd --config` | Print the path of the configuration file |
| `nerd --export-logs [HOURS]` | Save the logs of the last hours (24) to a file for a bug report; nothing is sent. Also **Export logs…** |
| `nerd --version` | The version, commit and build date. Also **About nerd…** |
| `nerd --help` | All commands |

`nerd send`, `nerd agents`, `nerd inbox` and `nerd wait` are for programs in panes: see [Messages](messages-between-agents.md).

## Updating

**Check for a new release…** in the main menu asks GitHub for the newest release of nerd and shows it with its changelog: **install** downloads it, checks that nerd's release key signed its checksum and that the archive matches, and puts it in `~/.local/bin`, as install.sh does (a release without a signature is not offered); **skip this release** keeps the daily check quiet about it; **later** leaves it. A nerd installed with Homebrew is told to run `brew upgrade nerd`, and one running from anywhere else names its path and installs nothing. Install works only from nerd's own window on the Mac the server runs on, not from a window opened inside a pane or from another Mac (though a program running as you could ask for the install itself: it would get only the newest release, checked against nerd's release key: nothing it could not already do as you). The first window on a Mac asks once whether nerd may check by itself once a day; **Settings… → Check for new releases** changes the answer, which is kept per Mac in `update-check` next to config.json. A daily check only shows a card, and marks **Check for a new release…** in the menu until you install or skip that release. Every check writes a line to the log, and what it sends is in [What leaves nerd](privacy-and-security.md#what-leaves-nerd).

After you install a new build, a card says **Update available**, and the **≡** menu button and **Update…** in the menu are marked. **… more** on the card, or **Update…**, opens a window with the running build, the new one and what changed between them: **update now** hands the running server's panes to the new build, and their programs keep running; **skip this build** says nothing more about that build; **later** leaves it for now.
