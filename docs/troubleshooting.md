# Troubleshooting
<!-- summary: Common problems and their fixes, from Option keys that type characters to an agent that shows no state. -->

| Problem | Solution |
|---|---|
| `zsh: killed nerd` right after installing from source | The binary was copied over the old one. Reinstall with `install` and `codesign` as in [Build from source](../README.md#installation). |
| `⌥` keys type characters instead of acting | Press `⌃`+`B`, then the key, which works in any terminal; or `nerd --preset <terminal>` sets the terminal to send Option as Alt. See [Keys](usage.md#keys). |
| A supported agent shows no state, or the wrong one | Install the hook: **Settings… → Agent hooks**, or `nerd --install-hook`. |
| A hooked Codex shows no state | Codex runs no new hook until you approve it: at its "Hooks need review" prompt, choose "Trust all and continue". |
| A restored Codex pane starts a fresh session instead of resuming | nerd resumes a Codex session without its shared background server, as `codex --no-daemon resume <id>`, and starts Codex fresh as `codex --no-daemon`; both need a Codex that has `--no-daemon`. If you confirmed the earlier line, `codex resume <id>`, nerd types the new one only once you confirm it: **Settings… → Agent hooks** reads "a resume line changed, confirm"; click it and confirm in the window it opens. Until then a restored Codex pane starts a fresh session, as `codex --no-daemon` (a pane opened with a command of its own starts as that command, without its resume part), and each time nerd starts it says so once, after any other message. |
| A hooked Codex still shows no exact state | Codex is served by its shared background server, which runs hooks outside every pane, so nerd ignores them and reads the state from the screen. Start it with `codex --no-daemon`. In a This-folder-only pane Codex starts only that way: a plain `codex` stops and says so. |
| `zsh: locking failed for ~/.zsh_history: operation not permitted: reading anyway` in a pane set to This folder only | The pane may not write your home folder, so zsh reads its history and can neither lock nor save it there. The shell works; what you type in that pane is not added to the history. |
| `nerd send` says `cannot reach nerd` | The agent's own sandbox blocks the socket; see [Messages](messages-between-agents.md). |
| Images in the preview look blocky | The terminal has no Kitty graphics protocol, or nerd runs inside tmux. Run nerd directly in Ghostty, Kitty, WezTerm or iTerm2, or set `images` to `off`. |
| Colours look more saturated than in other terminals | WezTerm and iTerm2 show colours in Display P3; nerd's colours are sRGB. |
| An image in the preview looks broken | Set `images` to `blocks`. |
