# Troubleshooting

| Problem | Solution |
|---|---|
| `zsh: killed nerd` right after installing from source | The binary was copied over the old one. Reinstall with `install` and `codesign` as in [Build from source](../README.md#installation). |
| `⌥` keys type characters instead of acting | Set the terminal to send Option as Alt; see [Keys](usage.md#keys). |
| A supported agent shows no state, or the wrong one | Install the hook: **Settings… → Agent hooks**, or `nerd --install-hook`. |
| A hooked Codex shows no state | Codex runs no new hook until you approve it: at its "Hooks need review" prompt, choose "Trust all and continue". |
| `nerd send` says `cannot reach nerd` | The agent's own sandbox blocks the socket; see [Messages](messages-between-agents.md). |
| Images in the preview look blocky | The terminal has no Kitty graphics protocol, or nerd runs inside tmux. Run nerd directly in Ghostty, Kitty, WezTerm or iTerm2, or set `images` to `off`. |
| Colours look more saturated than in other terminals | WezTerm and iTerm2 show colours in Display P3; nerd's colours are sRGB. |
| An image in the preview looks broken | Set `images` to `blocks`. |
