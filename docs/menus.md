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

- The routes map is the one window that fills the screen, so a wide layout can be panned; it leaves the top row to
  nerd's logo, an image the terminal draws over any text.
- The title is in the frame, never a row inside it. A context menu has no title.
- The size fits the content: as wide as its widest line, key strip included, and at least 40 columns; as tall as
  its rows, the strip, the blank row and `[Esc] close`. At most the screen less 1/16 at each side and 1/10 above
  and below; a longer list scrolls. It is centred.
- A window with a search or a filter is sized from its whole content, so it does not move while typing.
- The key strip is broken between keys into lines that fit the width (`chrome::hint`).
- Section headers are sentence case; a rule between groups is drawn in the border colour.

## Messages, isolation and accounts

- Main menu, server group: **Messages…** opens the Messages window: the mode and what it means for a pair with no
  route, Broadcast…, Set up a topology…, Routes map…, a matrix of the named panes (a row per sender, a column per
  recipient and `you`; each cell is the verdict a message would get now, bright when a route of the owner's decides,
  faint when the mode does; a click on a cell cycles that pair's route: none, allow, ask, no, none; a click on a row
  label shows its rows), what waits for the owner, and today's threads. When the columns do not fit, the window says
  which recipients fit and chips pick the space they start at; too narrow for one column, it says how many it needs.
  Set up a topology… comes back to the window. Routes that are not a pair of named panes stay as
  chips: a click cycles allow, ask, no, none.
- Main menu, server group, after Messages…: **Routes map…** opens the routes map: every named pane as a card, per
  space, with its routes, filling the screen. It only shows: a click on a card lights its routes; the wheel (with
  shift, sideways), the arrow keys and the left, right, up and down buttons pan, and a line says which columns and
  lines are shown. The Messages window has the same button; Esc goes back to that window when the map was opened
  from it, and closes the map when it was opened from the menu.
- Pane menu, a group after the view group: **Send message…** (greyed for a pane with no address-shaped name),
  **Show inbox…**, **Who can write here…**, **Where it can write…**, and the greyed note "Install the hook for
  exact delivery" when the pane's agent delivers through a hook that is not this build's. Then **Isolation…** as
  the pane's own tool. **Rename pane…** refuses a name another pane holds and asks keep or drop for a name with
  routes or waiting messages from before.
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

## Words

| Word | Means |
|---|---|
| server | a running nerd with its spaces and panes; it has a name and can be updated, stopped and connected to |
| session | one window's connection to a server (Session history, Detach) |
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
