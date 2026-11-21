# Agents' screens, as captured

93 files. 84 are each one live screen of a real agent, run by hand in a nested tmux in an empty scratch folder on
2026-11-21, captured with `tmux capture-pane -p`; 23 of the 93 carry their colours too, as `tmux capture-pane -p -e`
printed the same moment (escape sequences in the rows). The first line is the cursor as tmux reported it (`cursor ROW
COL shown|hidden size COLSxROWS`, 0-based); the rest is the screen. Paths, account names, a session id, subscription
and usage figures, and provider and cost lines are replaced or removed; the agents' own text is as they drew it.

The 9 made by hand are named so:
- `claude/busy-30-handmade-cut.txt`, `codex/busy-30-handmade-cut.txt`, `gemini/busy-30-handmade-wrapped.txt`,
  `qwen/busy-30-handmade-wrapped.txt`: the 40-column busy captures cut to 30 columns, the status line cut with `…`
  (Claude's footer, Codex's working line) or wrapped over two rows above the rule (Gemini, Qwen). A pane narrower than
  30 columns is never typed into.
- `opencode/question-approval-cursor-in-dialog.txt`: `opencode/question-approval.txt` with the hidden cursor moved
  into the dialog.
- `gemini/idle-30-handmade-wrapped.txt`, `qwen/idle-30-handmade-cut.txt`: the 40-column idle captures cut to 30
  columns, the placeholder wrapped onto the next row (Gemini) or cut with `…` (Qwen).
- `claude/idle-echoed-busy-words-handmade.txt`: Claude Code's pane as a live check through nerd showed it, idle after
  answering a message that held `esc to interrupt`, the words still in the conversation 4 rows above the prompt; its
  cursor placed where every Claude capture has it. `claude/busy-echoed-busy-words-handmade.txt` is the same screen
  with the footer of a working Claude (`… · esc to interrupt · …`, as `claude/busy-tool.txt` shows it).

`src/app/deliver.rs` feeds every file into a pane on a real pseudo-terminal: a file named `idle-*` must be typed into
when its agent's manifest records a prompt, unless it holds a half-typed reply (`*half-typed*`), and every other file
must not, whatever the agent's hook says.

| Agent | `--version` printed | `idle_text` | `busy_text`, `busy_at` | `placeholder_text`, `hides_cursor` | Files | Evidence |
|---|---|---|---|---|---|---|
| Claude Code | `2.1.330 (Claude Code)` | `❯` | `esc to interrupt`, `below`: the footer under the input | its placeholder and its suggestion after a turn are dim, what you type is not (`claude/idle-start.txt`, `claude/idle-suggestion.txt`, `claude/idle-half-typed-colour.txt`) | 20 | `claude/idle-*`, `claude/busy-*` (the prompt stays drawn while it works), `claude/question-*` (cursor hidden; the trust question's ` ❯` is one column in) |
| Codex CLI | `codex-cli 0.167.0` | `›` | `esc to interrupt)`, `above`: `• Working (…s • esc to interrupt)`, the nearest line with text above the prompt | `Ask Codex to do anything` (also dim) | 11 | `codex/idle-*`, `codex/busy-*` (prompt and cursor stay while it works), `codex/question-*` (cursor hidden in the bottom row) |
| Gemini CLI | `0.67.0` | ` >` | `(esc to cancel`, `above_rule`: `⠋ Thinking... (esc to cancel, 1s)`, the line above the rule over its input, 4 or 5 rows up | `Type your message or @path/to/file` (grey, not dim); `hides_cursor`: every capture hides the cursor and Gemini draws its own | 19 | `gemini/idle-*`, `gemini/busy-*`, `gemini/question-approval.txt` (`● 1.`, in `blocked_text`), `gemini/question-free-text*.txt` (its own input row starts `│ >`; `│ Answer Questions` in `blocked_text`), `gemini/question-trust.txt` |
| Qwen Code | `0.27.0` | `>` | `esc to cancel)`, `above_rule`: `.. Warp speed engaged... (0s · esc to cancel)`, the line above the rule over its input | `Type your message or @path/to/file` | 12 | `qwen/idle-*`, `qwen/busy-*`, `qwen/question-approval.txt` (`› 1.`, in `blocked_text`) |
| OpenCode | `1.19.5` | `  ┃` | `esc interrupt`, `⬝`, `below`: the status row under the input | none | 11 | `opencode/idle-*`, `opencode/busy-*` (at 40 columns `esc interrupt` breaks over rows; the progress bar's `⬝` stays), `opencode/question-approval*.txt` (`  ┃` also frames the dialog: the cursor is hidden there, and `△ Permission required` is in `blocked_text`); `opencode/home-start.txt` is idle, but its prompt sits at a column that moves with the width and is not recognised |
| Copilot CLI | `GitHub Copilot CLI 1.0.107.` | `❯` | `ctrl+c interrupt`, `below`: the row under the input | none | 11 | `copilot/idle-*`, `copilot/busy-*`, `copilot/question-*` (cursor hidden) |
| pi | `1.4.0` | none | none | none | 9 | `pi/*`: its input has no mark of its own (an empty row between two rules), and the rules also frame an extension's question (`pi/question-extension.txt`, a third-party extension's dialog, its words translated). pi is never typed into |
| Cursor | not installed | none | none | none | 0 | no capture: never typed into |

Every agent with a recorded prompt keeps its prompt row at idle while text is half typed into it, and at 40 columns
(`*-40*`); nerd does not type after a half-typed reply. Every tool approval captured hides the cursor or moves it off
the prompt; forms and free-text questions were captured from Gemini only.
