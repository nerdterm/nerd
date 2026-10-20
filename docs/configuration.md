# Configuration

Settings live in `~/Library/Application Support/nerd/config.json`. **Settings…** edits the common ones, and **Edit config.json…** there opens the file. nerd re-reads the file while it runs, so an edit lands within a second.

| Key | Default | Description |
|---|---|---|
| `theme` | `"auto"` | `auto` follows the macOS appearance, switching when it does, or `dark`, `light`. nerd paints every colour itself, pane output included |
| `colors` | `{}` | Overrides the built-in palette by role: `accent`, `fill` and `on_fill` (the cursor and selections, and the text on them), `ground`, `surface`, `slab` (the cursor while the keys are in another area), `border`, `border_focus`, `text`, `muted`, `faint`, `ok`, `warn`, `bad`, `wait` (an agent waiting for you), `teal`, and `ansi0`..`ansi15` for the sixteen colours pane output asks for by number. Values are `#rrggbb`; `accent` carries `border_focus` and the fill with it. A role left out, misspelled or malformed keeps its built-in colour |
| `mouse` | `true` | Mouse support |
| `sidebar_visible` | `true` | Sidebar expanded; `false` collapses it to a rail of state marks. `«` and `»` at its bottom switch it, and nerd remembers the choice |
| `sidebar_width` | `34` | Sidebar width in columns, 20 to 48 |
| `tree_width` | `34` | File tree width, 20 to 80 |
| `preview_width` | `60` | File preview width, 30 to 120 |
| `scrollback` | `10000` | History lines per pane, 100 to 100000; panes opened after a change get it |
| `pane_border` | `"always"` | `always`, `multiple` or `never` |
| `pane_gap` | `1` | Space between panes, 0 to 3 |
| `scrollbar` | `true` | Show scrollbars |
| `clock` | `"time"` | The clock at the end of the top row, written as the menu bar clock writes it: `"off"` (plugins' segments take its room), `"time"`, or `"date"` (the date and the time). A click on it switches time and date |
| `clock_time` | `""` | Your own pattern for the time, in the letters macOS uses: `"HH:mm:ss"`; empty: as the menu bar |
| `clock_date` | `""` | Your own pattern for the date: `"EEE d MMMM"`; empty: as the menu bar |
| `battery` | `"percent"` | The battery before the clock: `"off"`; `"percent"`: `46%` on battery, `ϟ 71%` on the charger, `L` after it in Low Power Mode; `"time"`: to empty on battery, to full on the charger. A click switches percent and time. Nothing on a Mac with no battery |
| `battery_low` | `20` | On battery at this percent or less the charge is yellow; `0` turns it off |
| `battery_critical` | `10` | On battery at this percent or less the charge is red and a notice says so once; `0` turns both off |
| `restore_screens` | `true` | Keep pane text across restarts |
| `sandbox` | `true` | Deny networking to the nerd server |
| `preview` | `true` | Show the file preview next to the tree |
| `preview_wrap` | `false` | Wrap long lines in the preview; off, they scroll sideways. `w` in the preview switches it |
| `paths` | `"home"` | `home` writes paths from your home folder (`~/dev/nerd`), `full` writes them whole (`/Users/you/dev/nerd`). Typing `~/` works either way |
| `images` | `"auto"` | Image previews: `auto` (full quality where the terminal supports it, blocks elsewhere), `blocks` or `off` |
| `prefix` | `"ctrl_b"` | nerd's key, as in tmux: `ctrl_b`, `ctrl_a` or `off`. A stored `off` becomes `ctrl_b` once, on the first start of the build that made it the default |
| `tmux` | `"ask"` | Started inside tmux: `ask` to open a window outside it, `new_window` without asking, or `stay` |
| `notify` | `{"sound": true, "bell": false}` | When an agent outside the focused pane needs you: play a sound, ring the terminal bell. nerd sends no macOS notifications |
| `remotes` | none | Machines for **Connect to another computer…** and `nerd --remote`: `[{"name": "studio", "host": "me@studio.local"}]`, a name of 1 to 16 letters, digits, `.`, `_` or `-`, with ssh keys set up for the host |
| `plugins` | none | Settings for each plugin, by its name; kept as written |

Check for new releases (**Settings…**, off or daily) is not in config.json: it is kept per Mac in `update-check` beside it, written only from nerd's own window on that Mac (a window inside a pane cannot change it, though a program running as you could write the file itself). With no file nerd asks once. Installing a release is asked for the same way, only from nerd's own window on that Mac, though a program running as you could ask for the install itself: it would get only the newest release, checked against its checksum, and the running servers change only when you choose **update now**.

A config.json that does not parse is never overwritten: nerd keeps the last good settings, says so, and writes nothing until the file parses again.
