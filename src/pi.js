// nerd: pi reports its pane's state, its tool calls and its session. Nothing leaves the machine, just small
// files under nerd's state directory, and only within nerd. It writes the same lines as nerd's shell hook for Claude Code.
import { appendFileSync, existsSync, lstatSync, mkdirSync, readFileSync, readdirSync, renameSync, unlinkSync, watch, writeFileSync } from "node:fs"
import net from "node:net"
import { dirname } from "node:path"

export default function NerdPiExtension(pi) {
  const pane = process.env.NERD_PANE
  const dir = process.env.NERD_STATE
  // writes go only under nerd's own state directory, the root this extension was installed for
  const root = "@ROOT@"
  const numbered = /^[0-9]+$/.test(pane ?? "")
  const sockMode = numbered && !dir && !!process.env.NERD_MSOCK
  const ok = numbered && typeof dir === "string" && dir.startsWith(root) && root.startsWith("/") && !dir.includes("..") && !/[\x00-\x1f\x7f]/.test(dir)
  if (!ok && !sockMode) return
  const base = sockMode ? `/${pane}` : `${dir}/${pane}`
  const now = () => Math.floor(Date.now() / 1000)
  const safe = (s, extra = "") => String(s ?? "").replace(new RegExp(`[^A-Za-z0-9_${extra}-]`, "g"), "")
  const line = (s) => String(s ?? "").split("\n").find((l) => l.trim()) ?? ""
  const cut = (s, n) => line(s).replace(/[\t\r]/g, " ").trim().slice(0, n)
  // socket mode: an isolated pane has no state root, so state and mail go through nerd's message socket
  const memory = {}
  let conn, ready = false, dead = false, sent = null, buf = ""
  const queue = []
  const pump = () => {
    if (!ready || sent || !queue.length) return
    sent = queue.shift()
    conn.write(JSON.stringify(sent.req) + "\n")
  }
  const ask = (req) =>
    new Promise((done) => {
      if (queue.length > 500) queue.shift().done(null)
      queue.push({ req, done })
      pump()
    })
  const answer = (text) => {
    let a
    try {
      a = JSON.parse(text)
    } catch {
      return
    }
    if (!ready) {
      if (a.error) {
        dead = true
        conn.destroy()
      } else ready = true
    } else if (sent) {
      const s = sent
      sent = null
      s.done(a)
    }
    pump()
  }
  const connect = () => {
    const c = (conn = net.connect(process.env.NERD_MSOCK))
    ready = false
    buf = ""
    c.setEncoding("utf8")
    c.unref()
    c.on("connect", () => c.write('{"Hello":{"protocol":1}}\n'))
    c.on("data", (d) => {
      buf += d
      for (let i; (i = buf.indexOf("\n")) >= 0; ) {
        const l = buf.slice(0, i)
        buf = buf.slice(i + 1)
        answer(l)
      }
    })
    c.on("error", () => {})
    c.on("close", () => {
      if (conn !== c) return
      ready = false
      sent?.done(null)
      sent = null
      if (!dead) setTimeout(connect, 1000).unref()
    })
  }
  // the pane wrapper checked a nerd-<uid> directory; one more look before the first byte
  const trusted = () => {
    const sock = process.env.NERD_MSOCK
    const m = /\/nerd-([0-9]+)\/[^/]+$/.exec(sock)
    if (!m) return true
    try {
      const st = lstatSync(dirname(sock))
      return st.isDirectory() && st.uid === Number(m[1]) && st.uid === Number(process.env.NERD_MSOCK_UID) && (st.mode & 0o777) === 0o711
    } catch {
      return false
    }
  }
  const part = (file) => (file === base ? "" : file.slice(base.length + 1))
  const read = (file) => {
    if (sockMode) return memory[part(file)] ?? ""
    try {
      return readFileSync(file, "utf8")
    } catch {
      return ""
    }
  }
  const put = (file, text) => {
    if (sockMode) {
      memory[part(file)] = text
      ask({ Put: { file: part(file), data: text } })
      return
    }
    mkdirSync(dir, { recursive: true, mode: 0o700 })
    writeFileSync(`${file}.tmp`, text, { mode: 0o600 })
    renameSync(`${file}.tmp`, file)
  }
  const drop = (file) => {
    if (sockMode) {
      delete memory[part(file)]
      ask({ Drop: { file: part(file) } })
      return
    }
    try {
      unlinkSync(file)
    } catch {}
  }
  // past 800 lines the log keeps its last 400 and says it was cut
  const event = (...fields) => {
    if (sockMode) {
      ask({ Append: { file: "events", line: [now(), ...fields].join("\t") } })
      return
    }
    mkdirSync(dir, { recursive: true, mode: 0o700 })
    appendFileSync(`${base}.events`, [now(), ...fields].join("\t") + "\n", { mode: 0o600 })
    const lines = read(`${base}.events`).split("\n").filter(Boolean)
    if (lines.length > 800) put(`${base}.events`, [`${now()}\t-\tcut`, ...lines.slice(-400)].join("\n") + "\n")
  }
  const state = (word) => {
    // a failed turn stays failed through the idle that follows it, until the next prompt
    if (word === "idle" && read(base).trim() === "failed") word = "failed"
    put(base, word + "\n")
    if (word !== "blocked") {
      event("-", "turn")
      event("-", "end", word)
    }
  }
  // pi writes its session file at the first message: until then `pi --session <id>` would find nothing
  const session = (ctx) => {
    const file = ctx.sessionManager?.getSessionFile?.()
    const id = ctx.sessionManager?.getSessionId?.()
    if (!file || !existsSync(file) || !/^[A-Za-z0-9_.][A-Za-z0-9_.-]{0,127}$/.test(id ?? "")) return false
    if (read(`${base}.session`).trim() !== id) put(`${base}.session`, id + "\n")
    // pi shows only `pi` in ps; nerd's restore looks there for a process that already holds this session
    process.title = `pi --session ${id}`
    return true
  }
  // NERD_HOOK_DUMP=<absolute dir> keeps every call with what nerd reads from its ctx, to be replayed in nerd's tests
  const dump = process.env.NERD_HOOK_DUMP
  let calls = 0
  const record = (name, e, ctx) => {
    if (!dump?.startsWith("/")) return
    try {
      const file = ctx.sessionManager?.getSessionFile?.()
      const c = { sessionId: ctx.sessionManager?.getSessionId?.(), mode: ctx.mode, idle: ctx.isIdle?.(), fileExists: !!file && existsSync(file) }
      mkdirSync(`${dump}/pi`, { recursive: true, mode: 0o700 })
      writeFileSync(`${dump}/pi/${now()}-${process.pid}-${++calls}-${name}.json`, JSON.stringify({ event: e, ctx: c }), { mode: 0o600 })
    } catch {}
  }
  let last, prompting = false, working = false, started = false
  const deliver = async (text) => {
    try {
      await pi.sendMessage({ customType: "nerd_message", content: text, display: true }, last.isIdle() ? { triggerTurn: true } : { triggerTurn: false, deliverAs: "followUp" })
      return true
    } catch {
      return false
    }
  }
  const mailDir = `${base}.mail`
  let held = null
  // a message taken from the socket is claimed, kept until pi accepts it, then confirmed with Took; an unconfirmed claim goes back to the queue after 60 s
  const next = async () => {
    if (sockMode) {
      held ??= (await ask({ Take: {} }))?.mail ?? null
      if (held && (await deliver(held.frame + "\n" + held.text))) {
        await ask({ Took: { id: held.id } })
        held = null
      }
      return
    }
    const f = readdirSync(mailDir).filter((n) => n.endsWith(".json")).sort()[0]
    if (!f) return
    const m = JSON.parse(readFileSync(`${mailDir}/${f}`, "utf8"))
    if (await deliver(m.frame + "\n" + m.text)) renameSync(`${mailDir}/${f}`, `${mailDir}/${f.slice(0, -5)}.sent`)
  }
  const tick = async () => {
    if (working || prompting || !last || (sockMode && !ready)) return
    working = true
    try {
      await next()
    } catch {
    } finally {
      working = false
    }
  }
  const start = () => {
    if (started || (sockMode && !trusted())) return
    started = true
    if (sockMode) connect()
    else {
      try {
        watch(mailDir, { persistent: false }, tick)
      } catch {}
    }
    setInterval(tick, 1000).unref()
  }
  // a child `pi -p` (a subagent, a tool's helper) inherits the pane's variables: only the interactive pi is the pane's
  const on = (name, fn) =>
    pi.on(name, (e, ctx) => {
      if (ctx?.mode !== "tui") return
      last = ctx
      start()
      record(name, e, ctx)
      fn(e, ctx)
      if (name !== "session_start") session(ctx)
    })

  on("session_start", (e, ctx) => {
    if (e.reason !== "reload") {
      drop(`${base}.events`)
      drop(`${base}.subagents`)
    }
    if (!session(ctx)) drop(`${base}.session`)
    state("idle")
  })
  on("agent_start", () => state("working"))
  on("agent_before_settle", (e) => {
    if (e.outcome === "error") put(base, "failed\n")
  })
  on("agent_settled", () => state("idle"))
  on("ui_prompt_start", () => {
    prompting = true
    state("blocked")
  })
  on("ui_prompt_end", (e, ctx) => {
    prompting = false
    put(base, ctx.isIdle() ? "idle\n" : "working\n")
  })
  on("tool_execution_start", (e) => {
    const a = e.args ?? {}
    const target = [a.path, a.command, a.pattern].find((v) => typeof v === "string" && v) ?? ""
    event("-", "start", safe(e.toolCallId), safe(e.toolName, ".:"), String(target).split("\n")[0].replace(/\t/g, " ").slice(0, 120))
  })
  on("tool_execution_end", (e) => {
    const text = (e.result?.content ?? []).find((c) => c?.type === "text")?.text
    event("-", e.isError ? "fail" : "ok", safe(e.toolCallId), cut(text, 160))
  })
}
