# Isolation
<!-- summary: What a pane may read, write and reach, per pane, tab or space. -->

A pane starts with your access: its program, an agent included, can read and write your files and reach the network. **Isolation…** in a pane's menu narrows that. **Isolation for new panes…** in a tab's or a space's menu sets it for the panes opened there, and a split keeps the isolation of the pane it came from. A change applies when the program starts: **[r] save and restart the program** restarts it.

| Files | What the pane's programs get |
|---|---|
| Everything | Your files, as you. |
| This folder only | Write access to the pane's folder, temporary folders, `~/Library/Caches` and the agents' session folders. No read access to `~/.ssh`, `~/.aws`, `~/.gnupg`, `~/.netrc`, `~/.docker`, `~/.kube`, `~/.azure`, `~/.config/gh`, `~/.config/gcloud`, `~/Library/Mail`, `~/Library/Messages`, `~/Library/Cookies` or nerd's own state. |
| Another account | What that macOS account may reach: its own files, and any of yours that other accounts can read (by default much of your home folder). Listed once you admit the account in **Settings… → Messages → Accounts…**. |

| Network | What the pane's programs get |
|---|---|
| Open | Your network, as you. |
| Only these providers | Only the hosts and ports that the agents you choose name in their manifests, through a proxy nerd runs for that pane alone. For pi, **Choose providers…** narrows that to some of its providers. The agent must honour `HTTPS_PROXY`; one that does not gets no network. Agents whose manifests name different ports cannot share a pane. |
| None | No internet and no name lookups. Services on this Mac (`localhost`) stay reachable. |

The hosts in the agents' `plugin.json` are a starting point: check them against what your agent contacts. pi's names every provider pi ships with, except four you add yourself if you use them: Bedrock outside us-east-1 and eu-central-1, Vertex and Azure use addresses of your own account; Cloudflare (`api.cloudflare.com`, `gateway.ai.cloudflare.com`) can fetch any URL for an account token, so it would make the pane's network open. In a pane set to Only these providers, Bedrock works with keys, not with SSO or profile login.

**Choose providers…** appears under the chips once pi is chosen and lists pi's providers, each with the hosts it opens: `deepseek` opens `api.deepseek.com`, `openai` opens `api.openai.com` and `auth.openai.com`, `github-copilot` opens GitHub's Copilot hosts and `github.com`, which pi also downloads its tools from. **all** is every provider, those added in a later version of pi included; a list of providers stays that list, so a provider added later stays closed. The pane is saved as `pi/deepseek`, one entry per provider; a pane saved before this choice existed is saved as `pi` and keeps every provider. A provider that a later nerd renames or removes stops the pane rather than opening more: it does not start, and the notice names the pane and the provider; the **Isolation** window shows it as no longer in pi's manifest and drops it on save. Only built-in agents name providers; a plugin's or your own agent manifest names its hosts as one list.

A pane's `~/.pi/agent/*.lock` is writable from every This-folder-only pane: one session can hold another pi session's lock, which only delays that session until the lock is deleted.

**This folder only** also keeps one session from planting code or instructions for another. Inside the places it may write, these stay read-only: shell, git and editor startup files, `~/.local/bin` and other folders ahead on `PATH`, agents' settings and memory, Claude Code's task list, Codex's shared databases, Qwen Code's workflow scripts, what agents source or run later from their own folders, and the caches package managers run code from (pnpm, uv, deno, Go, Playwright, Poetry, Yarn, node-gyp, pip, Cypress, SwiftPM, Copilot). When the pane's folder is your home folder, its dot files, `~/Library` and `~/bin` stay read-only too, except `~/Library/Caches` and the agents' session folders; choose a project folder for a real limit. The **Isolation** window lists what stops working.

Local sockets: with Network **Open**, a This-folder-only pane reaches no ssh agent and no Docker, colima, OrbStack, Rancher Desktop, lima or podman socket, so `git` over ssh and `docker` do not work there. With **Only these providers** or **None**, a pane reaches no Unix socket but nerd's message socket.

Programs that need the setuid bit (`ps`, `top`, `sudo`) do not run in a pane with This folder only, Only these providers or None: Seatbelt refuses them under a profile. An agent's own Seatbelt sandbox cannot run there either; the **Isolation** window says how to turn it off for each agent.

**Another account.** **Settings… → Messages → Accounts…** shows the commands that create the account; nerd never runs them. Its panes ask for your password (sudo) each time they start, and run a copy of nerd that only root can change. Put it in place once, and again after each nerd update:

```sh
sudo install -d -o root -g wheel -m 755 "/Library/Application Support/nerd/bin"
sudo install -o root -g wheel -m 755 <path of the running nerd> "/Library/Application Support/nerd/bin/nerd"
```

Until that copy matches the running nerd, the account's panes refuse to start. The window also shows an optional sudoers line that skips the password; with it, any program running as you can act as that account at any time. Neither way stops a program that already controls your own session.

A pane of the account reaches nerd's message socket through `/tmp/nerd-<uid>/`; its Only these providers proxy logs to that account's own `daemon.log`.
