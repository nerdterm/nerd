// nerd: OpenCode reports its pane's state, its subagents and its tool calls. Nothing leaves the
// machine, just small files under nerd's state directory, and only within nerd. It writes the same
// lines as nerd's shell hook for Claude Code; a subagent is a child session.
import { appendFileSync, lstatSync, mkdirSync, readFileSync, readdirSync, renameSync, unlinkSync, watch, writeFileSync } from "node:fs"
import net from "node:net"
import { dirname } from "node:path"

export const NerdPlugin = async ({ client } = {}) => {
  const pane = process.env.NERD_PANE
  const dir = process.env.NERD_STATE
  // writes go only under nerd's own state directory, the root this plugin was installed for
  const root = "/HOME/Library/Application Support/nerd/"
  const numbered = /^[0-9]+$/.test(pane ?? "")
  const sockMode = numbered && !dir && !!process.env.NERD_MSOCK
  const ok = numbered && typeof dir === "string" && dir.startsWith(root) && root.startsWith("/") && !dir.includes("..") && !/[\x00-\x1f\x7f]/.test(dir)
  if (!ok && !sockMode) return {}
  const base = sockMode ? `/${pane}` : `${dir}/${pane}`
  // child session -> the id its lines carry
  const children = new Map()
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
  const append = (file, fields) => {
    if (sockMode) {
      if (file === `${base}.events`) ask({ Append: { file: "events", line: fields.join("\t") } })
      else put(file, read(file) + fields.join("\t") + "\n")
      return
    }
    mkdirSync(dir, { recursive: true, mode: 0o700 })
    appendFileSync(file, fields.join("\t") + "\n", { mode: 0o600 })
  }
  // past 800 lines the log keeps its last 400 and says it was cut
  const event = (...fields) => {
    append(`${base}.events`, [now(), ...fields])
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
  // NERD_HOOK_DUMP=<absolute dir> keeps every call as it came, to be replayed in nerd's tests
  const dump = process.env.NERD_HOOK_DUMP
  let calls = 0
  const record = (hook, payload) => {
    if (!dump?.startsWith("/")) return
    try {
      mkdirSync(`${dump}/opencode`, { recursive: true, mode: 0o700 })
      writeFileSync(`${dump}/opencode/${now()}-${process.pid}-${++calls}-${hook}.json`, JSON.stringify(payload), { mode: 0o600 })
    } catch {}
  }
  // the pane resumes with `opencode --session <id>` after a restart; a resumed one creates no session
  const session = (id) => {
    if (/^[A-Za-z0-9_.][A-Za-z0-9_.-]{0,127}$/.test(id ?? "") && read(`${base}.session`).trim() !== id) put(`${base}.session`, id + "\n")
  }
  const who = (session) => children.get(session) ?? "-"
  const stop = (id) => {
    const lines = read(`${base}.subagents`).split("\n").filter(Boolean)
    const mine = lines.find((l) => l.split("\t")[0] === id)
    if (!mine) return
    put(`${base}.subagents`, lines.filter((l) => l !== mine).map((l) => l + "\n").join(""))
    event(id, "stop", ...mine.split("\t").slice(1, 4))
  }

  let main, asking = false, working = false
  const deliver = async (text) => {
    try {
      // promptAsync answers once the prompt is accepted; prompt only when the turn ends, past nerd's 60 s claim
      const send = client.session.promptAsync ?? client.session.prompt
      const r = await send.call(client.session, { path: { id: main }, body: { parts: [{ type: "text", text }] } })
      return !r?.error
    } catch {
      return false
    }
  }
  const mailDir = `${base}.mail`
  let held = null
  // a message taken from the socket is claimed, kept until OpenCode accepts it, then confirmed with Took; an unconfirmed claim goes back to the queue after 60 s
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
    if (working || asking || !main || !client || (sockMode && !ready)) return
    working = true
    try {
      await next()
    } catch {
    } finally {
      working = false
    }
  }
  if (!sockMode || trusted()) {
    if (sockMode) connect()
    else {
      try {
        watch(mailDir, { persistent: false }, tick)
      } catch {}
    }
    setInterval(tick, 1000).unref()
  }

  return {
    "chat.message": async (input, output) => {
      record("chat.message", { input, output })
      if (children.has(input.sessionID)) return
      main = input.sessionID
      session(input.sessionID)
      state("working")
    },
    "tool.execute.before": async (input, output) => {
      record("tool.execute.before", { input, output })
      const a = output?.args ?? {}
      const target = [a.filePath, a.command, a.pattern, a.description].find((v) => typeof v === "string" && v) ?? ""
      event(who(input.sessionID), "start", safe(input.callID), safe(input.tool, ".:"), String(target).split("\n")[0].replace(/\t/g, " ").slice(0, 120))
    },
    "tool.execute.after": async (input, output) => {
      record("tool.execute.after", { input, output })
      event(who(input.sessionID), "ok", safe(input.callID), cut(output?.title || output?.output, 160))
      put(base, "working\n")
    },
    event: async ({ event: e }) => {
      record("event", { event: e })
      const p = e?.properties ?? {}
      switch (e?.type) {
        case "session.created": {
          const info = p.info ?? {}
          if (!info.parentID) {
            drop(`${base}.subagents`)
            drop(`${base}.events`)
            children.clear()
            main = info.id
            session(info.id)
            break
          }
          const id = safe(info.id)
          children.set(info.id, id)
          append(`${base}.subagents`, [id, cut(info.agent || "agent", 32), cut(info.title, 120), now()])
          break
        }
        case "session.idle": {
          const id = children.get(p.sessionID)
          if (id) stop(id)
          else state("idle")
          break
        }
        case "session.error":
          if (!children.has(p.sessionID)) state("failed")
          break
        case "permission.asked":
          asking = true
          state("blocked")
          break
        case "permission.replied":
          asking = false
          break
      }
    },
  }
}
