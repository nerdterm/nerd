# Agent hook payloads

Each folder holds one agent's hook payloads, replayed through nerd's real hook script (OpenCode's through
`opencode.js` and pi's through `pi.js`, under node) by `hook::tests::recorded_payloads_replay_through_the_real_script`,
`recorded_opencode_calls_replay_through_the_plugin` and `recorded_pi_calls_replay_through_the_extension`.

Recorded from live runs with `NERD_HOOK_DUMP=<dir>`: each agent was given "Create a file hello.txt containing the
word hi, then read it back." in a nerd pane, and its permission prompt was approved (Claude Code ran in auto mode, so
it asked none). Versions: codex-cli 0.157.1, Copilot CLI 1.0.88, Gemini CLI 0.61.0, Qwen Code 0.24.6, OpenCode
1.18.32; Claude Code's payloads do not carry its version. The home folder is written as `/Users/user`.

A file is `<epoch>-<pid>-<word>.json`, one per hook call, so an event several words answer appears once per word;
the replay runs each event once, with the words this build's manifest gives it. OpenCode's are
`<epoch>-<pid>-<n>-<hook>.json`, holding `{input, output}` or `{event}`: the calls the plugin acts on and a few
ordinary message events, out of the 161 recorded. pi's are `<epoch>-<pid>-<n>-<event>.json`, holding `{event, ctx}`:
the event as pi sent it and what the extension read from its ctx. pi 1.0.4 ran the same prompt against a local stand-in
model that called write, read and a `sudo` bash command, behind a test extension that asks before `sudo`; the
`agent_before_settle` event is cut to its outcome (the rest is the whole conversation).

`expect.json`: the state word, the session id and the subagent rows `[type, task, running]` the replay must leave,
and `after`, the state word after each event in turn.

Cursor's folder is still written from its documented schema (https://cursor.com/docs/agent/hooks), not recorded.
