# Menus, settings and windows

Every menu, settings row, window and button in nerd follows these rules. A new item is placed by them, not by
taste.

## Rules

1. **A menu belongs to what it opens on.** Right click on a pane gives what a pane can do; on a tab, a tab; on a
   tree row, that file or folder. The main menu (≡, ⌥+M) holds the rest: this window, this server, the program.
   An item is in two menus only when it acts on both objects.
2. **Every action is in a menu.** A key or a button is a second way in, never the only one.
3. **One action, one name, everywhere:** the menu item, the window it opens, the key list, the docs.
4. **A name is a verb and its object:** "Close pane", "Rename tab", "Copy path". The object is named even in its
   own menu. Sentence case, no final period.
5. **"…" means a window opens first.** An item that asks for a name, a choice or a confirmation, or opens a
   screen, ends with the one character "…" (U+2026). An item without it acts at once.
6. **A switch names what it will do:** "Hide file tree" while it shows. Pairs: Show/Hide, Collapse/Expand,
   Wrap/Don't wrap, Full screen/Leave full screen.
7. **Groups, then order.** Separators split a menu into groups of related items, at most six each: make or open,
   view, the object's own tools, plugins, rename and move, then what destroys. Destroying is always last.
8. **Menus keep their shape.** An item that belongs to the object stays in its place and is greyed when it cannot
   act now; it is hidden only when it can never apply here (worktree items outside git). Digits 1–9 count every
   row but separators, so they always pick the same item. One exception: the items of Messages, Isolation and
   Accounts stay enabled in a window attached from inside a pane and refuse with a notice when chosen, because a
   greyed item cannot show the reason.
9. **Keys are shown where they exist,** right-aligned, as printed on the key: ⌥+T, ⌥+shift+arrow. Menus, the key
   list and docs/usage.md read them from the key tables (`keys::GLOBAL`, `tree::keys`).
10. **A window is named after its item:** "Export logs…" opens "Export logs", and its result is "Logs exported".
    Titles, section headers and settings rows are sentence case; values are lowercase words.
11. **Every window closes the same way:** a clickable `[Esc] close`, Esc, or a click outside. Buttons read
    `[Key] outcome`: `[Enter] close pane`, `[Esc] cancel`. A window that only tells has one button, `[Esc] close`,
    and Enter closes it too: two buttons that do the same thing offer a choice that is not there.
12. **Plugins get their own group,** after the object's own tools and before rename and destroy. A plugin's panel
    ends in "…"; its command does not.

## Windows

Every window except a context menu is drawn through `chrome::dialog`, and `chrome::fit` sizes it.

- The routes map and Review changes are the windows that fill the screen, so a wide layout or a diff has room; they
  leave the top row to nerd's logo, an image the terminal draws over any text.
- The title is in the frame, never a row inside it. A context menu has no title.
- Each window type has one width, frame included, whatever it shows:
  - 48 columns: a one-line field (Rename, a space's name, every prompt); the typed text scrolls inside it, a note
    under it wraps and a long title is cut with "…".
  - 68 columns: Confirm, Search, Settings, Keys and mouse, New space, every picker (Open worktree, Preset terminal,
    Topology, Report period, a subagent's run), Session history and a plugin's panel.
  - 80 columns: Servers, Plugins, Messages, Isolation, Choose providers and Accounts.

  What does not fit is wrapped or cut, never widened: a body, a hint and a key's description wrap (the description
  under itself, a table's last column under its own start, a command under its indent, a line of chips under its
  first chip, each chip keeping its click); a picker's second column, a panel's row and the
  right-hand columns of a table are cut with "…"; a folder's path in New space loses its middle to "…". Not sized
  this way: the routes map, context menus and the notice card. A window is as tall as its rows, the strip, the
  blank row and `[Esc] close`. At most the screen less 1/16 at each side and 1/10 above and below, which narrows a
  window on a small screen; a longer list scrolls. It is centred.
- A window with a search or a filter is sized from its whole content, so it does not move while typing.
- Content taller than its window scrolls one way in every window (`chrome::Scroll`): the wheel three lines, Up and Down
  a line (where they move a selection, the selection leads and is always in view), Page Up and Page Down a page, Home
  and End to the start and the end. While anything is hidden, the row over the buttons reads "lines N–M of K"; the
  buttons never scroll. A panel whose rows and text are both hidden says both on that row ("rows N–M of K · lines
  N–M of K"); a Search help page says it over **[o] open on nerdterm.dev**. A context menu taller than the screen
  opens where it does and scrolls the same way, its last row saying what is hidden. Keys and mouse lists these keys
  under "A window taller than the screen". The routes map pans instead.
- The key strip is broken between keys into lines that fit the width (`chrome::hint`). A row of buttons wider than
  the window is broken the same way, close on the last line.
- Section headers are sentence case; a rule between groups is drawn in the border colour.

## Search

**Search…** in the main menu and `⌥`+`O` open one window over what nerd already
has. The field has a frame of its own at the top. Type to filter; every word must match. Nothing is listed before a
letter is typed: the sidebar already lists the panes. Each result is one line, its name on the left and one value on
the right (a key, a setting's value, a folder, a pane's place); a name, place or value is cut at 512 bytes. Search
keeps its top where its longest list would put it and grows downward with its results, up to 14 rows; more scroll, and
the list moves only to keep the chosen row in view and with the wheel, never under the pointer. Groups are tabs: All,
Panes, Commands, Settings, Plugins, Files, Help and Text; on a narrow window the tabs and the chips take more lines. All
shows at most five rows of each group and a row that says how many more there are. Panes has chips for the agent states;
a chip other than all lists the panes in that state. Search, a name field and New space take a paste's first line; every
other window drops it; New space trims it, and a path starting with `/` or `~` replaces the `~/` the field opens with;
the pane search and the file tree's filter take it while they hold the keys, and the sidebar and the tab bar drop it.
What a result says while the window stays (a pane that has closed, a refusal, a help page's address) is a line in the
window above the keys, gone at the next letter typed; a longer note takes more rows.

- Panes: Enter or a click goes to the pane.
- Commands: runs the menu item on the pane, tab or space it was listed for; a pane that has closed gives a notice and
  nothing else is touched. The item's own window replaces Search.
- Settings: Scrollbars, File preview, Wrap long lines, Play a sound, Ring the terminal bell and Restore screens after a
  restart toggle in their row and Search stays; every other setting opens Settings on that row.
- Plugins: an installed plugin turns on or off in its row; one only in the catalog opens the Plugins window.
- Files: shows the file in the file tree. Names are read once per window, from the second letter typed, or the first
  on the Files tab; a link is listed by its own name and never followed. Desktop, Documents, Downloads, Library,
  Pictures, Movies and Music in the home folder, and other disks and network folders (/Volumes, /net, /Network,
  /System/Volumes), are never entered from outside them, and the Files tab says so; a space inside one is searched.
- Help: shows the section in the window; `o` opens its page on nerdterm.dev.
- Text: searches the text of every pane, only on Enter, never while typing; the last row searches again. A line opens
  in the pane search, on that line even when the pane has printed more since.

A window attached from inside a pane is refused the same actions through Search as through the windows they come
from (turning a plugin on, removing, installing, updating).

## Servers

- Main menu, server group: Messages…, Routes map…, Rename this server…, **Servers…**, Session history…, and, inside
  tmux or screen, Open outside tmux…. **Stop this server…** stays last in the menu, with what destroys.
- **Servers…** opens the Servers window. A section **this Mac** lists every server of this Mac, the one this session
  shows first (State `current`), then by when a session last attached: name and number, State (`current`, `running`,
  `open in another session`, `stopped`), Sessions (0 or 1: a server shows one session at a time), Last used,
  Sandbox and Folders. Under them **New server…** (a new server, and this session moves to it; this server keeps
  running), **Remove all stopped…** (greyed while none is stopped), and a row per stray: a `nerd --daemon` of yours
  no listed server leads to. Then a collapsed section per machine in `remotes`: a click or the right arrow opens it
  and lists its servers over ssh, as `nerd --remote` does; a click or the left arrow closes it, and the left arrow in a
  section goes up to its head. Last, **Connect to another computer…**: its button opens config.json, where machines
  are added.
- A click on a row chooses it, and the buttons under the rows act on it; Enter does the first, the letter in a
  button its own. The current server: **Rename…**, **Update…**, **Stop…** (the same as Rename this server…, Update…
  and Stop this server…). Another running one: **Open** (this session moves to it), **Open in a new window**,
  **Rename…**, **Update…**, **Stop…**. A stopped one: **Open** (it starts again), **Rename…**, **Remove…**. A stray:
  **Stop…**, which checks the pid again before it sends SIGTERM. A server on a machine: **Open**, in a terminal window
  of its own.
- Opening a server another session shows asks first: **take it over**. **Remove…** and **Remove all stopped…** say
  what goes (the layout, pane text, messages, routes, logs and saved agent session ids) and what stays (config.json,
  plugins and the agents' own sessions); a folder whose server never started goes without asking. Every dialog
  comes back to the window once answered.
- A session from another Mac sees only the server's own Mac, with no machines, no New server… and no Open. A session
  attached from inside a pane only reads: every action but Open on a machine's server is refused with a notice.
- With the sidebar collapsed, the top row names the server this session shows at the right, beside the machine
  badge, cut short when narrow.

## Messages, isolation and accounts

- Main menu, server group: **Messages…** opens the Messages window: the mode and what it means for a pair with no
  route, Broadcast…, Set up a topology…, Routes map…, a quick start of three lines under them, a matrix of the spaces
  that hold a named pane (a row per sending space, a column per receiving space and `you`; a cell sums up every pair
  of their panes: allow, ask or no when they agree, bright when a route of the owner's decides one of them, faint
  when only the mode does, mixed when they differ, held N when messages between them wait), what waits for the owner,
  and today's threads. A press on a cell opens that pair of spaces in place of the matrix: ‹ every space and Escape
  go back; every pane sets one route for the whole pair (allow, ask, no, mode), with a line that more specific routes
  go first; then a row per sending pane and a column per receiving pane, each cell the verdict a message would get
  now, a click cycling that pair's route: none, allow, ask, no, none; a click on a pane's name shows its rows. When
  the columns do not fit, the window says which fit and ‹ earlier and later › page them; too narrow for one column,
  it says how many it needs. Set up a topology… comes back to the window. Routes that are not a pair of named panes
  stay as chips: a click cycles allow, ask, no, none.
- A waiting message opens the view **Held message** (or **Message** for one written to the owner) inside the
  Messages window: a click on its row in the sidebar's needs-you list or on the collapsed rail, on a `held N` cell
  (the first message of that pair), on a waiting row's head (also under Show inbox…), or Enter on the current waiting
  row. It shows sender → recipient, how long it waited, the reason in a sentence, a line of buttons that does not
  scroll (**[d] deliver**, **[h] deliver, allow 1 hour**, **[x] drop**, **[r] reply…**; for a route request
  **[a] allow**, **[k] ask**, **[n] no**; for a message to the owner **[r] reply…**, **[x] drop**), then the text,
  which the wheel and the arrow keys scroll. Once it is delivered, dropped, decided or, for a message to the owner,
  answered, the next waiting message of the same pair follows; with none left, and on **[Esc] back**, it returns to
  the window, or closes when it was opened from the sidebar (**[Esc] close**).
- Main menu, server group, after Messages…: **Routes map…** opens the routes map: every named pane as a card, per
  space, with its routes, filling the screen. It only shows: a click on a card lights its routes; the wheel (with
  shift, sideways), the arrow keys and the left, right, up and down buttons pan, and a line says which columns and
  lines are shown. The Messages window has the same button; Esc goes back to that window when the map was opened
  from it, and closes the map when it was opened from the menu.
- Pane menu, view group, after Subagents…: **Review changes…**, greyed unless the pane runs an agent in a folder of a
  git repository. It fills the screen: the changed files on the left, the chosen file's diff on the right, and above
  them **since main** (where this branch left the default branch) and **uncommitted** to compare with. A click on a
  diff line, or Enter, asks for a comment on it; the comment shows under its line with **edit** and **delete**.
  **[s] send N comments to** the pane's name sends them as one message from you; **[x] discard** drops them,
  **[r] refresh** reads the repository again. Comments stay until sent or discarded while the pane lives.
- Pane menu, a group after the view group: **Send message…** (greyed for a pane with no address-shaped name),
  **Show inbox…**, **Who can write here…**, **Where it can write…**, and the greyed note "Install the hook for
  exact delivery" when the pane's agent delivers through a hook that is not this build's. Then **Isolation…** as
  the pane's own tool. **Rename pane…** refuses a name another pane holds and asks keep or drop for a name with
  routes or waiting messages from before.
- Space menu of a space with one pane: **Rename pane…** after **Rename space…**. The sidebar folds a lone pane into its
  space's row, so that row names the pane too; with two panes or more each has its own row and menu.
- In the Isolation window, once an agent with providers (pi) is chosen under Only these providers, a line under the
  chips says which (`pi: all`, `pi: deepseek, openai`) beside **[Choose providers…]**. It opens the view **Choose
  providers** inside the window: a row **all** with its host count, then a row per provider with its hosts. A click
  ticks a row (all clears the providers; a provider clears all); **[Enter] done** writes the choice back, **[Esc] back**
  returns unchanged; done with nothing ticked stays and says so.
- Tab menu and space menu: **Isolation for new panes…**, with "as the space" or "as nerd's default".
- Settings → Messages: Messaging, Programs in panes may stop or update nerd, Set up a topology…, Accounts…. The
  server stores them in policy.json, not in config.json.
- Settings → Terminal: **Terminal preset…** lists Ghostty, kitty, WezTerm and iTerm2, each applied, not applied,
  (Ghostty) old keys link, or (WezTerm) needs your line. A click on one opens "<terminal> preset": every file it writes or changes, then
  **[Enter] apply** (or **[Enter] undo** when it is applied) and **[Esc] cancel**; nothing changes before the button.
  For a WezTerm config nerd does not edit, the window shows the line to add in full, with **[Enter] copy the line**
  (it also writes nerd's file) and **[Esc] close**. The files are on the server's Mac, so a window from another Mac is
  told to run `nerd --preset` there.
- Every change these windows make is refused from a window attached from inside a pane; reading them is not.
- Main menu, after **Update…**: **Check for a new release…** opens "Check for a new release" with the newest release and its
  changelog, then **install** (a click only), **[Esc] later** and **[s] skip this release**; install is refused from a window inside
  a pane or from another Mac. Settings → Updates: **Check for new releases**, off or daily, stored per Mac beside config.json
  and changed only from nerd's own window on that Mac.

## Words

| Word | Means |
|---|---|
| server | a nerd with its spaces and panes, running or stopped; it has a name and can be opened, updated, stopped and removed |
| session | one window's connection to a server (Session history, Detach); a server shows one at a time |
| machine | another Mac named in `remotes`, whose servers Servers… lists |
| stray | a `nerd --daemon` of yours that no listed server leads to |
| space | a folder with its tabs |
| tab, pane | as they are |
| full screen | one thing takes the whole screen: a pane, the file tree, a run |
| file tree | the tree and its preview; its views are files and changes, changes shown as a tree or as a list |
| run | a subagent's record |
| logs | what Export logs… saves to a file; nothing is sent |

"Instance", "workspace", "zoom" and "window" (for a connection) are not used in the interface.

## Settings

Only what a person changes often is on the Settings screen. The security switch `sandbox`, `mouse` and structured
values (`colors`, `remotes`, `plugins`) stay in config.json on purpose;
"Edit config.json…" opens it. A config.json that does not parse is never overwritten: nerd keeps the last good
settings and writes nothing until the file parses again. The Messages rows (Messaging, Programs in panes may stop
or update nerd) are stored by the server in policy.json, not in config.json, and a window attached from inside a pane
cannot change them.
