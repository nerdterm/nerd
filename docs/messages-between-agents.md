# Messages

Agents in different panes can write to each other, and to you, through nerd. Every message passes the server: it reads the sender from the operating system, applies your routes and limits, keeps a record, and hands the text to the agent by the best path that agent has.

- **A name is the opt-in.** A pane named with **Rename pane…** can receive. A name is 1 to 32 letters, digits, `.`, `_` or `-`, does not start with `-`, is not all digits, and is not `you` or `all`. An unnamed pane neither sends nor receives.
- **Sending.** From a pane: `nerd send <name|all|you> <text>` (`-` reads the text from standard input), with `--reply <id>` to answer, `--ask-route <name>` to ask you for a route, and `--wait <secs>` to wait for the answer. `nerd agents` lists the panes the caller may write to, `nerd inbox [--all] [--show <id>]` reads its mail, and `nerd wait <id> [--thread] [--count N] [--timeout S]` waits for replies. Exit codes: 0 sent, 3 held for your answer, 1 refused, 2 timed out.
- **The frame.** Every delivered message starts with `[message from <name> via nerd, id <id>; reply: nerd send --reply <id> "…"]`. The sender is read from the kernel, never from the text; a line of the text that starts the same way gets `> ` in front.
- **Routes.** Mode **Open** lets panes of your account write to each other and asks you about other accounts; **Strict** allows only what a route allows. A route says Allow, Ask or No from one side to another: a pane, a space, a tab, an account, an agent, `you` or everyone. The most specific route wins, and a tie goes to the stricter. Writing to `you` is allowed unless a route to `you` says Ask or No. A reply to a message passes unless a route says No; a refused or held message opens no way back. Answering a held message can allow that pair for an hour.
- **Where.** **Messages…** in the main menu shows the mode, a matrix of who may write to whom (a click on a cell cycles that pair's route), what waits for you, and today's threads. **Set up a topology…** writes routes from a preset. **Routes map…** shows every named pane and its routes on one screen. **Settings… → Messages → Accounts…** admits other accounts.
- **A held message.** A click on it in the sidebar's needs-you list, on a `held N` cell, or on its row under what waits for you opens it: who wrote to whom, why nerd holds it in a plain sentence, how long it has waited, its text, and its buttons (deliver, deliver and allow 1 hour, drop, reply; for a route request allow, ask, no). Once it is delivered, dropped, decided or, for a message to you, answered, the next held message between the same two panes follows; when none is left the view goes back to where it was opened from.

| Limit | Value |
|---|---|
| Queued for one pane | 50 |
| Held for you in all | 100 (messages to `you` still pass) |
| Held from one pane | 20; past it that pane's sends are refused |
| Per pair of panes | 6 a minute |
| The same text again | 60 s |
| Hops in a chain | 8 |
| Messages in a thread | 40 |
| One `all` | 8 panes |
| One message | 64 KiB |
| Queued, undelivered | 24 hours |
| Held, unanswered | 7 days |
| Records kept | 5000 |

Above 20 KiB only the first 2 KiB is delivered, with `nerd inbox --show <id>` for the rest.

| Agent | How the message reaches it |
|---|---|
| Claude Code | Its inbox socket, when its session reported one and a process of that pane listens on it; otherwise its hook |
| Codex | Its hook |
| Qwen | Its hook |
| Copilot | Its hook: `block` at Stop |
| Gemini | Its hook: `deny` at AfterAgent |
| Cursor | Its hook: `followup_message` at stop. The CLI may not fire it; then typing when idle |
| pi, OpenCode | nerd's extension |

An agent with no better path gets the message typed in, only while it is idle and is the program in front of its terminal. Nothing is ever typed into a pane waiting for your answer. `via` in the Messages window says which path was used.

**When an agent's own sandbox blocks `nerd send`.** `nerd send` talks to the socket named by `NERD_MSOCK`. Claude Code with `/sandbox`: add `"sandbox": {"network": {"allowUnixSockets": ["<the path in $NERD_MSOCK>"]}}` to its settings (`echo $NERD_MSOCK` in the pane prints it; each server has its own). Codex with `workspace-write`: `[sandbox_workspace_write] network_access = true` and a `unix_sockets` allow for the socket path. Gemini's permissive profile needs nothing. pi and OpenCode have no OS sandbox. For any other agent, test it. nerd never edits these settings. Codex with `--remote` runs its tools outside the pane, so `nerd send` from them is refused (`only panes inside nerd can send`); the agent still receives, by typing.

**The trust pattern.** A pane that reads untrusted text (web pages, mail, issues) should not write unreviewed into a pane that holds private data, and a pane with private data should not write toward a way out. The **Reading and private** preset of **Set up a topology…** writes exactly those routes.

**What no agent can change while nerd runs:** names, routes, isolation, accounts, and the switch **Programs in panes may stop or update nerd**. A window attached from inside a pane can read the Messages window but changes nothing in it.

**Logs.** `daemon.log` gets one line per change of a message's state, never its text. `messages.jsonl` keeps the records for 7 days and a finished message's text for 24 hours; only you can read it, and Time Machine skips it.
