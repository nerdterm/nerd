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

Every key repeats a menu item or a click. The shortcuts start with `⌥` (Option) and are read from the key itself, not from what the keyboard layout prints on it, so they work the same on a Cyrillic layout once the terminal sends Option as Alt (see below). **Keys and mouse…** in the main menu lists them all.

| Key | Action |
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
| `⌥`+`shift`+arrow | The next pane in that direction; from the last pane, the area beyond it: the sidebar, the file tree, or the tab bar |
| arrows, `return` | Inside the sidebar or the tab bar |
| `Esc` | Close a menu or dialog, or leave an area for the pane |

**Settings… → tmux-style prefix** turns on `⌃`+`B` or `⌃`+`A`; after it, a key without `⌥` does what `⌥` and that key do, and the prefix pressed twice goes to the pane. It is off by default, because while it is on, the shell and an outer tmux never see that key.

The terminal must send Option as Alt. Some send the character the layout prints for Option and the key instead, until told otherwise:

| Terminal | Setting |
|---|---|
| Ghostty | Nothing on a U.S. Standard or U.S. International layout, where both Option keys send Alt (`macos-option-as-alt = left` keeps the right one for symbols). Otherwise `macos-option-as-alt = left` in its config, then **Ghostty → Reload Configuration** |
| Kitty | `macos_option_as_alt left` in kitty.conf, then restart Kitty: reloading the config does not change it |
| WezTerm | Nothing: the left Option key sends Alt already |
| iTerm2 | **Settings → Profiles → Keys → General**: Left Option key **Esc+** |

Set the left Option key only: it then sends nerd's shortcuts, and the right one still types the layout's symbols, such as — and €.

On a non-Latin layout Ghostty 1.3.1 still drops these keys; `nerd --install-keys` binds them to the physical keys in Ghostty's config, which works on any layout.

Control with a non-Latin letter, such as the tmux-style prefix on a Cyrillic layout: Kitty sends it as Control and the Latin letter on the same key. In the other terminals, switch to a Latin layout for Control shortcuts.

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
| `nerd --uninstall` | Stop starting nerd at login, and remove what `--install-keys` added |
| `nerd --install-keys` | Option keys on the physical keys in Ghostty, for any layout |
| `nerd --uninstall-keys` | Remove them |
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
