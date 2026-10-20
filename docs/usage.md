# Usage

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

Every key repeats a menu item or a click. nerd's key is `⌃`+`B` (Control + B, as in tmux): press it, let go, then press one of the keys below without `⌥` and without `shift`. `⌃`+`B` twice sends `⌃`+`B` to the program in the pane (the shell's back one character, vim's page up). Inside tmux it is still nerd's: press it twice to reach tmux. The key after it is read from the key itself, not from what the layout prints on it, and its case does not matter, so it works the same on a Cyrillic layout; `⌃`+`B` is read off its key too, where the terminal sends Control with the layout's letter. While nerd waits for the second key, the top row shows `⌃+B`. **Settings… → tmux-style prefix** switches it to `⌃`+`A` or turns it off.

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
| Ghostty | Writes `ghostty-preset.conf` and two colour files in nerd's folder: `macos-option-as-alt = left`, `window-padding-x/y = 16` with `window-padding-balance`, `theme = light:…,dark:…` with nerd's colours, and the Option keys bound to their physical keys, so they work on a non-Latin layout, where Ghostty 1.3.1 drops them otherwise. Adds `config-file = ?…` in a marked block at the end of the Ghostty config it loads last (`config.ghostty` in `~/.config/ghostty` when there is none). An include is read after the file that names it, so the preset wins over the same settings there. It replaces `nerd --install-keys`, whose line it takes out |
| kitty | Writes `kitty-preset.conf` in nerd's folder: `macos_option_as_alt left`, `window_padding_width 16` and nerd's colours for the appearance at the time; adds `include …` in a marked block at the end of kitty.conf, made if missing. Restart kitty: reloading the config does not change Option |
| WezTerm | Writes `wezterm-preset.lua` in nerd's folder, a function that sets the left Option key to send Alt, a 16 pt margin and nerd's colours for the current appearance. With no WezTerm config, nerd makes `~/.wezterm.lua` that calls it. A config of yours that ends with `return config` (`config` built in it by `wezterm.config_builder()` or `{`, under any name) gets one marked line right before that return. Any other config is not edited: nerd shows the one line to add, the window copies it, and the preset reads "needs your line" until the line is there |
| iTerm2 | Adds a dynamic profile named nerd (left Option key Esc+, right Option normal, nerd's colours for light and dark) and sets iTerm2's margins, which every profile shares, to 16, keeping the values before for undo. Make the profile the default in **Settings → Profiles → Other Actions → Set as Default**; iTerm2 takes the margins on its next start |

The right Option key keeps typing the layout's symbols, such as — and €. nerd rewrites its own preset files when it starts, so a new build's keys and your `colors` reach the terminal; a theme changed later reaches it at the next start.

## Commands

| Command | What it does |
|---|---|
| `nerd` | Start a new server, with its own desktop, in the current folder |
| `nerd --resume [NAME]` | Return to a server by name or number, the most recent by default |
| `nerd --rename N [NAME]` | Name a server, or clear its name. Also **Rename this server…** |
| `nerd --list` | Servers, running or stopped, with their names, sandbox and spaces |
| `nerd --stop [NAME]` | Stop a server, or all of them. Also **Stop this server…** |
| `nerd --update [NAME]` | Switch running servers to the installed build; panes keep running. Also **Update…** |
| `nerd --disconnect [NAME]` | End every connection to a server, or to all; programs keep running. Also **Disconnect everyone…** |
| `nerd --remote M [NAME]` | Open a server on machine `M` from `remotes` in config.json, over ssh. Also **Connect to another computer…** |
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

After you install a new build, a card says **Update available**, and the **≡** menu button and **Update…** in the menu are marked. **… more** on the card, or **Update…**, opens a window with the running build, the new one and what changed between them: **update now** hands the running server's panes to the new build, and their programs keep running; **skip this build** says nothing more about that build; **later** leaves it for now.
