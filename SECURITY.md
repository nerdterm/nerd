# Security policy

## Supported versions

Only the latest release receives security fixes.

## Reporting a vulnerability

Report it privately through GitHub: **Security → Report a vulnerability** on this repository. Do not open a public issue, pull request or discussion.

Include what is affected, the steps to reproduce, and the impact you expect. You will get an answer when the report has been read; there is no guaranteed response time. Please allow a fix to be released before you disclose the issue publicly.

## Scope

In scope:

- escape sequences or other pane output that reach the user's terminal or trigger actions in nerd;
- ways around the server's network sandbox;
- anything that leaves nerd other than what "What leaves nerd" in [docs/privacy-and-security.md](docs/privacy-and-security.md) lists, or a plugin reaching more of the network than its permission says;
- a pane reaching more than its isolation allows (Files: This folder only or Another account; Network: Only these providers or None), or an agent changing names, routes, isolation or accounts while nerd runs;
- files written by nerd with wrong permissions or readable by other users;
- the installer, the release archive and its checksum.

Out of scope:

- what programs in a pane left at Everything and Open do, which nerd deliberately does not restrict;
- attacks that require local access as the same user;
- the binary not being notarized by Apple, which is documented.

## What the protections cover

- **The server's sandbox denies the network, and nothing else.** The Seatbelt profile starts from `(allow default)`: the file system and Mach services stay open to the server. Unix-domain sockets stay allowed, except mDNSResponder's, which is denied by name so that name lookups fail; the server can reach any other local socket its user can.
- **A plugin reaches only its network tier.** Its own Seatbelt profile allows nothing, or `localhost` only. With hosts and ports it reaches only the port of a proxy nerd runs for it alone, and the proxy connects only to those hosts on those ports: a name only at its public internet addresses, never this Mac or a network it is on; an IP address as the manifest writes it. A plugin never opens a terminal device, and does not read the credential folders and `~/Library` listed in docs/plugins.md; it can read the command line and environment of the user's other programs, which macOS does not let a sandbox hide.
- **The broker runs what the server asks.** It starts panes and plugins outside the sandbox from the server's requests, so the server's sandbox is a guarantee about nerd's own code (no network, no telemetry), not a wall around a compromised server.
- **A nerd reached over ssh is trusted like a local one.** What the remote server sends reaches your terminal as it would from a local server.
- **Panes are outside it.** A helper started before the sandbox launches every pane. A pane left at Everything and Open keeps your full access to files and the network; its isolation (**Isolation…**) narrows that with a Seatbelt profile of its own, a proxy for Only these providers, or another account. An agent's own Seatbelt sandbox cannot run inside an isolated pane.
- **`open` is outside it.** Opening a file or link "the way the system would", and opening a new terminal window, go through LaunchServices, which starts those programs outside the sandbox.
- **A sandbox that fails to start does not stop the server.** The failure is written to `daemon.log`, shown by `nerd --list`, and shown in the window of every client that attaches.
- **The `.sha256` proves integrity, not origin.** It comes from the same release as the archive, so it catches a damaged or truncated download, not a release replaced by someone who can replace both. The installer refuses an archive without one unless `NERD_INSTALL_UNVERIFIED=1` is set.
- **Both ends check the other's user on every connection.** The server talks only to processes of its own user, and a client refuses a server socket another user created first, for example at a `NERD_SOCKET` in a shared directory, before it sends anything. Such a socket still keeps nerd from starting there: keep `NERD_SOCKET` in a directory only you can write.
