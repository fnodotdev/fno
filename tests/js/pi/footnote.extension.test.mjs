// Behavioral tests for the pi bridge extension (transport contract).
// Run: node --test tests/js/pi/footnote.extension.test.mjs   (node 24)
//
// A fake pi host records `on`, `sendUserMessage` and `appendEntry` calls, and
// FNO_AGENTS_BIN points at a scratch shell stub that answers `state path`,
// `loop-check` and `distress-scan` from env-selected fixtures and appends its
// argv to a log. The stub IS the seam the assertions read: every test pins
// what the extension sends, records, and never does.

import { test } from "node:test"
import assert from "node:assert/strict"
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import footnote from "../../../cli/src/fno/setup/assets/pi/footnote.ts"

// The scratch stub binary: logs its argv, then answers from fixtures.
// `state path target-state` prints $STUB_STATE_PATH when set; `loop-check`
// optionally sleeps past a short gate bound, then prints the fixture file.
const STUB = `#!/bin/bash
echo "$@" >> "\$STUB_LOG"
case "\$1" in
  state)
    if [ -n "\$STUB_STATE_PATH" ]; then echo "\$STUB_STATE_PATH"; fi
    ;;
  hook)
    if [ -n "\$STUB_STDIN_DIR" ]; then cat > "\$STUB_STDIN_DIR/\$\$.json"; fi
    ;;
  loop-check)
    if [ -n "\$STUB_LOOPCHECK_SLEEP" ]; then sleep "\$STUB_LOOPCHECK_SLEEP"; fi
    if [ -f "\$STUB_LOOPCHECK_REPLY" ]; then cat "\$STUB_LOOPCHECK_REPLY"; fi
    ;;
  announce)
    if [ -n "\$STUB_ANNOUNCE" ]; then echo "\$STUB_ANNOUNCE"; fi
    ;;
esac
exit 0
`

// Every hook script the extension's carrier chains name. One stub per
// script: it captures its stdin payload, appends its name to a shared
// sequence file, and answers one fixture context line.
const CARRIER_SCRIPTS = [
  "session-start-using-fno.sh",
  "context-run.sh",
  "daemon-liveness-session-start.sh",
  "bootstrap-session-start.sh",
  "inject-mail-notify.sh",
  "node-bind-prompt-submit.sh",
  "format-on-edit.sh",
  "edit-integrity.sh",
  "claim-heartbeat.sh",
  "merge-capture.sh",
  "save-session.sh",
  "precompact-canon-doc.sh",
]

// A fixture plugin root the extension resolves through FNO_HOME: real
// .claude-plugin marker, stub hook scripts, nothing else.
function makeFixtureRoot(baseDir) {
  const root = join(baseDir, "plugin-root")
  mkdirSync(join(root, ".claude-plugin"), { recursive: true })
  writeFileSync(join(root, ".claude-plugin", "plugin.json"), "{}")
  mkdirSync(join(root, "hooks"), { recursive: true })
  for (const name of CARRIER_SCRIPTS) {
    writeFileSync(
      join(root, "hooks", name),
      `#!/bin/bash
cat > "$CAPTURE_DIR/${name}.json"
echo "${name}" >> "$CAPTURE_DIR/seq.txt"
# CAPTURE_CONTEXT gates the context line: the real carriers stay silent
# when they have nothing to deliver, and the announce-deferral test needs
# that silence on its second boundary.
[ -n "\$CAPTURE_CONTEXT" ] && echo '{"hookSpecificOutput":{"additionalContext":"[fixture] ${name}"}}'
exit 0
`,
      { mode: 0o755 },
    )
  }
  return root
}

// The FNO_HOME that pins pluginRoot() to a fixture root (or to an empty
// dir, which answers null and keeps the no-root worlds deterministic).
function makeFixtureHome(baseDir, root) {
  const home = join(baseDir, "home")
  mkdirSync(join(home, "install"), { recursive: true })
  if (root) writeFileSync(join(home, "install", "plugin-root"), root)
  return home
}

// One isolated world per test: a fixture dir (manifest, reply file, log), the
// env the extension and stub read, and the fake host. FNO_HOME pins to an
// empty home by default, so pluginRoot() answers null deterministically;
// `root: true` builds the fixture plugin root and points FNO_HOME at it.
function makeWorld({
  reply = { decision: "allow", termination_reason: null },
  manifest = true,
  root = false,
  env = {},
} = {}) {
  const dir = mkdtempSync(join(tmpdir(), "fno-pi-ext-"))
  const manifestPath = join(dir, "target-state.md")
  if (manifest) {
    writeFileSync(
      manifestPath,
      "---\nsession_id: fno-own\nharness: pi\nharness_session_id: s-1\n---\n",
    )
  }
  const replyPath = join(dir, "reply.json")
  writeFileSync(replyPath, JSON.stringify(reply))
  const logPath = join(dir, "stub.log")
  writeFileSync(logPath, "")
  const stubPath = join(dir, "stub.sh")
  writeFileSync(stubPath, STUB, { mode: 0o755 })
  const fixtureRoot = root ? makeFixtureRoot(dir) : null
  const captureDir = join(dir, "capture")
  mkdirSync(captureDir, { recursive: true })

  const saved = new Map()
  const next = {
    FNO_AGENTS_BIN: stubPath,
    STUB_LOG: logPath,
    STUB_STATE_PATH: manifest ? manifestPath : "",
    STUB_LOOPCHECK_REPLY: replyPath,
    FNO_HOME: makeFixtureHome(dir, fixtureRoot),
    CAPTURE_DIR: captureDir,
    CAPTURE_CONTEXT: fixtureRoot ? "1" : "",
    ...env,
  }
  for (const [k, v] of Object.entries(next)) {
    saved.set(k, process.env[k])
    process.env[k] = v
  }
  for (const k of ["FNO_AGENT_SESSION_ID", "FNO_PI_GATE_TIMEOUT_MS"]) {
    if (!(k in next)) {
      saved.set(k, process.env[k])
      delete process.env[k]
    }
  }

  const handlers = {}
  const sent = []
  const entries = []
  const notified = []
  const host = {
    on(event, handler) {
      handlers[event] = handler
    },
    sendUserMessage(content, options) {
      sent.push({ content, options })
    },
    appendEntry(type, data) {
      entries.push({ type, data })
    },
  }
  const ctx = {
    sessionManager: {
      getSessionId: () => "s-1",
      buildContextEntries: () => [],
    },
    isIdle: () => true,
    hasUI: true,
    ui: {
      notify(message, level) {
        notified.push({ message, level })
      },
    },
  }
  footnote(host)

  const world = {
    dir,
    handlers,
    sent,
    entries,
    notified,
    captureDir,
    fixtureRoot,
    log: () => readLog(logPath),
    async settle(eventCtx = ctx) {
      await handlers["agent_settled"]({}, eventCtx)
    },
    payload(name) {
      try {
        return JSON.parse(readFileSync(join(captureDir, name), "utf8"))
      } catch {
        return null
      }
    },
    seq() {
      try {
        return readFileSync(join(captureDir, "seq.txt"), "utf8").split("\n").filter(Boolean)
      } catch {
        return []
      }
    },
    cleanup() {
      for (const [k, v] of saved) {
        if (v === undefined) delete process.env[k]
        else process.env[k] = v
      }
      rmSync(dir, { recursive: true, force: true })
    },
  }
  return world
}

function readLog(logPath) {
  // execFile waits for exit, and the stub appends before answering, so by
  // the time a settle resolves every line is on disk.
  try {
    return readFileSync(logPath, "utf8")
  } catch {
    return ""
  }
}

test("AC12-HP: a bound settle runs the gate and sends the gate's continuation", async () => {
  const world = makeWorld({
    reply: {
      decision: "block",
      termination_reason: null,
      continuation: "/skill:target resume",
    },
  })
  try {
    await world.settle()
    const loopLine = world
      .log()
      .split("\n")
      .find((l) => l.startsWith("loop-check "))
    assert.ok(loopLine, "loop-check must be called")
    assert.ok(loopLine.includes("--harness pi"), loopLine)
    assert.ok(loopLine.includes("--harness-session s-1"), loopLine)
    assert.ok(loopLine.includes("--state "), loopLine)
    assert.equal(world.sent.length, 1)
    assert.equal(world.sent[0].content, "/skill:target resume")
    assert.deepEqual(world.sent[0].options, {
      deliverAs: "followUp",
      triggerTurn: true,
      expandPromptTemplates: true,
    })
  } finally {
    world.cleanup()
  }
})

test("AC12-ERR: a refusal sends nothing and records one refused entry", async () => {
  const world = makeWorld({
    reply: {
      decision: "refuse",
      reason: "wrong session: manifest bound to pi/s-1, asked pi/foreign",
    },
    env: { FNO_AGENT_SESSION_ID: "foreign" },
  })
  try {
    await world.settle()
    assert.equal(world.sent.length, 0, "a refused settle must not re-drive")
    const gates = world.entries.filter((e) => e.type === "fno-gate")
    assert.equal(gates.length, 1)
    assert.equal(gates[0].data.state, "refused")
    assert.ok(String(gates[0].data.reason).includes("wrong session"))
  } finally {
    world.cleanup()
  }
})

test("AC13-ERR: a wedged gate is killed, records unavailable, and notifies", async () => {
  const world = makeWorld({
    reply: { decision: "block", termination_reason: null, continuation: "/skill:target resume" },
    env: { FNO_PI_GATE_TIMEOUT_MS: "300", STUB_LOOPCHECK_SLEEP: "5" },
  })
  try {
    await world.settle()
    assert.equal(world.sent.length, 0, "no re-drive past a wedged gate")
    const gates = world.entries.filter((e) => e.type === "fno-gate")
    assert.equal(gates.length, 1)
    assert.equal(gates[0].data.state, "unavailable")
    assert.equal(world.notified.length, 1, "one visible warning when there is a UI")
    assert.ok(world.notified[0].message.includes("gate unavailable"))
  } finally {
    world.cleanup()
  }
})

test("AC13-EDGE: a block with no continuation sends nothing and records why", async () => {
  const world = makeWorld({
    reply: { decision: "block", termination_reason: null },
  })
  try {
    await world.settle()
    assert.equal(world.sent.length, 0)
    const gates = world.entries.filter((e) => e.type === "fno-gate")
    assert.equal(gates.length, 1)
    assert.equal(gates[0].data.state, "unavailable")
    assert.equal(gates[0].data.reason, "gate named no continuation")
  } finally {
    world.cleanup()
  }
})

test("AC14-HP: a native pi session is no longer excluded by the extension itself", async () => {
  const world = makeWorld({
    reply: {
      decision: "block",
      termination_reason: null,
      continuation: "/skill:target resume",
    },
    env: { FNO_AGENT_SESSION_ID: "" },
  })
  try {
    // No FNO_AGENT_SESSION_ID: the extension still gates, because the
    // manifest-bound session id is the binding now.
    await world.settle()
    const loopLine = world
      .log()
      .split("\n")
      .find((l) => l.startsWith("loop-check "))
    assert.ok(loopLine, "the gate runs for a native session too")
    assert.equal(world.sent.length, 1)
  } finally {
    world.cleanup()
  }
})

test("AC11-ERR: discovery with no plugin-root pointer returns empty, prints one line, throws nothing", async () => {
  const home = mkdtempSync(join(tmpdir(), "fno-pi-home-"))
  const saved = process.env.FNO_HOME
  process.env.FNO_HOME = home
  const lines = []
  const err = console.error
  console.error = (m) => lines.push(String(m))
  try {
    const handlers = {}
    footnote({
      on(event, handler) {
        handlers[event] = handler
      },
      sendUserMessage() {},
    })
    const answer = handlers["resources_discover"]({}, {})
    assert.deepEqual(answer, {})
    const footLines = lines.filter((l) => l.includes("[footnote]"))
    assert.equal(footLines.length, 1, `one [footnote] line, got: ${footLines}`)
  } finally {
    console.error = err
    if (saved === undefined) delete process.env.FNO_HOME
    else process.env.FNO_HOME = saved
    rmSync(home, { recursive: true, force: true })
  }
})

test("per-turn state: turn start, tool call and settle each push through the session-state entry", async () => {
  // One file per fire: concurrent stub processes never interleave a payload.
  const stdinDir = mkdtempSync(join(tmpdir(), "fno-pi-stdin-"))
  const world = makeWorld({ env: { STUB_STDIN_DIR: stdinDir } })
  try {
    const ctx = {
      sessionManager: { getSessionId: () => "s-1", buildContextEntries: () => [] },
      model: { id: "glm-5.3", provider: "zai" },
      isIdle: () => true,
    }
    await world.handlers["before_agent_start"]({}, ctx)
    await world.handlers["tool_call"]({ toolName: "read", input: {} }, ctx)
    await world.settle(ctx)
    // The push is fire-and-forget, so wait for all three payloads to land.
    const end = Date.now() + 3000
    const payloads = () =>
      readdirSync(stdinDir).map((f) => readFileSync(join(stdinDir, f), "utf8")).filter((t) => t.endsWith("}"))
    while (Date.now() < end && payloads().filter((t) => t.includes("hook_event_name")).length < 3) {
      await new Promise((r) => setTimeout(r, 20))
    }
    const events = world
      .log()
      .split("\n")
      .filter((l) => l.startsWith("hook session-state --harness pi "))
      .map((l) => l.split(" ").pop())
      .sort()
    assert.deepEqual(events, ["PreToolUse", "Stop", "UserPromptSubmit"])
    // The settle also feeds the rules entry on the same stdin log; keep the
    // session-state payloads, which alone carry hook_event_name.
    const parsed = payloads()
      .map((l) => JSON.parse(l))
      .filter((p) => p.hook_event_name)
    assert.equal(parsed.length, 3)
    assert.ok(parsed.every((p) => p.session_id === "s-1" && p.model === "zai/glm-5.3"))
    assert.equal(parsed.find((p) => p.hook_event_name === "PreToolUse").tool_name, "read")
  } finally {
    world.cleanup()
    rmSync(stdinDir, { recursive: true, force: true })
  }
})

test("the manifest read rides the command, so a directory with no .fno still gates", async () => {
  const world = makeWorld({
    reply: {
      decision: "block",
      termination_reason: null,
      continuation: "/skill:target resume",
    },
  })
  try {
    // The fixture dir carries no .fno at all: the space-resolved path from
    // the command is the only manifest the extension knows.
    assert.equal(existsSync(join(world.dir, ".fno")), false)
    await world.settle()
    assert.equal(world.sent.length, 1, "the gate still ran")
  } finally {
    world.cleanup()
  }
})

test("session_start producers run and their context rides the next prompt boundary once", async () => {
  const world = makeWorld({ root: true })
  try {
    await world.handlers["session_start"]({ reason: "startup" }, {
      sessionManager: { getSessionId: () => "s-1" },
    })
    // All four producers ran, in the claude chain's names, with the claude
    // SessionStart payload (source mapped from pi's reason).
    for (const name of [
      "session-start-using-fno.sh",
      "context-run.sh",
      "daemon-liveness-session-start.sh",
      "bootstrap-session-start.sh",
    ]) {
      const p = world.payload(`${name}.json`)
      assert.ok(p, `${name} must run at session_start`)
      assert.equal(p.hook_event_name, "SessionStart")
      assert.equal(p.source, "startup")
      assert.equal(p.session_id, "s-1")
    }
    // The queued text rides the next prompt boundary...
    const answer = await world.handlers["before_agent_start"]({ prompt: "hi" }, {
      sessionManager: { getSessionId: () => "s-1" },
    })
    const text = answer?.message?.content?.[0]?.text || ""
    assert.ok(text.includes("[fixture] session-start-using-fno.sh"), text)
    // ...once: a second boundary does not repeat it.
    const second = await world.handlers["before_agent_start"]({ prompt: "again" }, {
      sessionManager: { getSessionId: () => "s-1" },
    })
    const secondText = second?.message?.content?.[0]?.text || ""
    assert.ok(!secondText.includes("[fixture] session-start-using-fno.sh"), secondText)
  } finally {
    world.cleanup()
  }
})

test("resume maps to the claude resume source at session_start", async () => {
  const world = makeWorld({ root: true })
  try {
    await world.handlers["session_start"]({ reason: "resume" }, {
      sessionManager: { getSessionId: () => "s-1" },
    })
    assert.equal(world.payload("context-run.sh.json").source, "resume")
  } finally {
    world.cleanup()
  }
})

test("the mail-notify and node-bind carriers ride every prompt with the claude payload", async () => {
  const world = makeWorld({ root: true })
  try {
    const answer = await world.handlers["before_agent_start"](
      { prompt: "start the bound node" },
      { sessionManager: { getSessionId: () => "s-1" } },
    )
    for (const name of ["inject-mail-notify.sh", "node-bind-prompt-submit.sh"]) {
      const p = world.payload(`${name}.json`)
      assert.ok(p, `${name} must run at the prompt boundary`)
      assert.equal(p.hook_event_name, "UserPromptSubmit")
      assert.equal(p.prompt, "start the bound node")
      assert.equal(p.session_id, "s-1")
    }
    // Carrier context joins the message; with no announce the message stays
    // off the transcript (claude's context-only channel).
    const text = answer?.message?.content?.[0]?.text || ""
    assert.ok(text.includes("[fixture] inject-mail-notify.sh"), text)
    assert.equal(answer.message.display, false)
    assert.equal(answer.message.customType, "fno-role")
  } finally {
    world.cleanup()
  }
})

test("context text defers the announce: it answers alone, and the announce lands at the next boundary", async () => {
  const world = makeWorld({ root: true, env: { STUB_ANNOUNCE: "[announce] fleet news" } })
  try {
    await world.handlers["session_start"]({ reason: "startup" }, {
      sessionManager: { getSessionId: () => "s-1" },
    })
    const first = await world.handlers["before_agent_start"]({ prompt: "one" }, {
      sessionManager: { getSessionId: () => "s-1" },
    })
    const firstText = first?.message?.content?.[0]?.text || ""
    assert.ok(firstText.includes("[fixture] session-start-using-fno.sh"), firstText)
    assert.ok(!firstText.includes("[announce] fleet news"), "no join with the announce")
    assert.equal(first.message.display, false)
    assert.ok(
      !world.log().includes("announce read"),
      "the announce cursor is not consumed under context",
    )
    // Boundary two: the carriers go silent (nothing to deliver), as the
    // real ones do, so the announce finally answers alone.
    delete process.env.CAPTURE_CONTEXT
    const second = await world.handlers["before_agent_start"]({ prompt: "two" }, {
      sessionManager: { getSessionId: () => "s-1" },
    })
    const secondText = second?.message?.content?.[0]?.text || ""
    assert.ok(secondText.includes("[announce] fleet news"), secondText)
    assert.equal(second.message.display, true)
    assert.equal(second.message.customType, "fno-announce")
  } finally {
    world.cleanup()
  }
})

test("tool_result maps pi tool names, promotes file_path, and runs the PostToolUse chain in order", async () => {
  const world = makeWorld({ root: true })
  try {
    await world.handlers["tool_result"](
      {
        toolName: "edit",
        input: { path: "/tmp/a.rs" },
        content: [{ type: "text", text: "edited" }],
        isError: false,
      },
      { sessionManager: { getSessionId: () => "s-1" } },
    )
    assert.deepEqual(world.seq(), [
      "format-on-edit.sh",
      "edit-integrity.sh",
      "claim-heartbeat.sh",
      "merge-capture.sh",
    ])
    const fmt = world.payload("format-on-edit.sh.json")
    assert.equal(fmt.tool_name, "Edit")
    assert.equal(fmt.tool_input.file_path, "/tmp/a.rs")
    assert.equal(fmt.hook_event_name, "PostToolUse")
    const integrity = world.payload("edit-integrity.sh.json")
    assert.equal(integrity.tool_response.output, "edited")
    assert.equal(integrity.tool_response.is_error, false)
  } finally {
    world.cleanup()
  }
})

test("session_before_compact maps the trigger, never cancels, and queues the pointer text", async () => {
  const world = makeWorld({ root: true })
  try {
    const sm = {
      getSessionId: () => "s-1",
      getSessionFile: () => "/tmp/pi-session.jsonl",
    }
    const answer = await world.handlers["session_before_compact"](
      { reason: "threshold", customInstructions: "keep the plan" },
      { sessionManager: sm },
    )
    assert.equal(answer, undefined, "the handler must not cancel the compaction")
    const save = world.payload("save-session.sh.json")
    assert.equal(save.hook_event_name, "PreCompact")
    assert.equal(save.trigger, "auto")
    assert.equal(save.transcript_path, "/tmp/pi-session.jsonl")
    assert.equal(save.custom_instructions, "keep the plan")
    const canon = world.payload("precompact-canon-doc.sh.json")
    assert.equal(canon.trigger, "auto")
    // The pointer text rides the next prompt boundary.
    const next = await world.handlers["before_agent_start"]({ prompt: "go on" }, {
      sessionManager: { getSessionId: () => "s-1" },
    })
    const text = next?.message?.content?.[0]?.text || ""
    assert.ok(text.includes("[fixture] precompact-canon-doc.sh"), text)
  } finally {
    world.cleanup()
  }
})

test("a manual compaction maps the trigger to manual", async () => {
  const world = makeWorld({ root: true })
  try {
    await world.handlers["session_before_compact"](
      { reason: "manual" },
      { sessionManager: { getSessionId: () => "s-1" } },
    )
    assert.equal(world.payload("save-session.sh.json").trigger, "manual")
  } finally {
    world.cleanup()
  }
})

// The real-host leg: pi fires the events for real and the extension runs the
// real chain against fixture hooks. Skipped wherever pi is not installed
// (CI); the provider call after the prompt 401s on a bogus key, which is
// fine - the carriers run before it, and the child is killed at the bound.
const hasPi = (() => {
  for (const dir of (process.env.PATH || "").split(":")) {
    try {
      if (dir && existsSync(join(dir, "pi"))) return true
    } catch {
      // a PATH entry that is not a directory
    }
  }
  return false
})()

test("a real pi host fires session_start producers and prompt carriers through the extension", { skip: !hasPi }, async () => {
  // Bun's node:test shim ignores the skip option object, so the guard runs
  // in the body too: without it, a pi-less runner dies at the spawn itself.
  if (!hasPi) return
  const { spawn } = await import("node:child_process")
  const dir = mkdtempSync(join(tmpdir(), "fno-pi-real-"))
  const captureDir = join(dir, "capture")
  mkdirSync(captureDir, { recursive: true })
  const agentDir = join(dir, "agent")
  mkdirSync(join(agentDir, "extensions"), { recursive: true })
  const fixtureRoot = makeFixtureRoot(dir)
  const stubPath = join(dir, "stub.sh")
  writeFileSync(stubPath, STUB, { mode: 0o755 })
  const saved = new Map()
  const next = {
    FNO_AGENTS_BIN: stubPath,
    STUB_LOG: join(dir, "stub.log"),
    FNO_HOME: makeFixtureHome(dir, fixtureRoot),
    CAPTURE_DIR: captureDir,
    PI_CODING_AGENT_DIR: agentDir,
    // A bogus key: the provider call 401s fast, after the events under test.
    ANTHROPIC_API_KEY: "sk-fno-test-bogus",
  }
  for (const [k, v] of Object.entries(next)) {
    saved.set(k, process.env[k])
    process.env[k] = v
  }
  const extPath = new URL("../../../cli/src/fno/setup/assets/pi/footnote.ts", import.meta.url).pathname
  const child = spawn(
    "pi",
    ["-e", extPath, "--no-session", "-p", "hello from the fno test", "--provider", "anthropic", "--model", "claude-haiku"],
    { stdio: "ignore", timeout: 30000, killSignal: "SIGKILL" },
  )
  try {
    // The carriers write at session_start and the prompt boundary, both
    // before the provider call; poll past a wedged start, then read.
    const want = ["session-start-using-fno.sh.json", "inject-mail-notify.sh.json"]
    const end = Date.now() + 25000
    while (Date.now() < end && !want.every((f) => existsSync(join(captureDir, f)))) {
      await new Promise((r) => setTimeout(r, 200))
    }
    for (const f of want) {
      assert.ok(existsSync(join(captureDir, f)), `${f} must land from a real pi run`)
    }
    const mail = JSON.parse(readFileSync(join(captureDir, "inject-mail-notify.sh.json"), "utf8"))
    assert.equal(mail.hook_event_name, "UserPromptSubmit")
    assert.equal(mail.prompt, "hello from the fno test")
    assert.ok(mail.session_id, "the real host names a session id")
  } finally {
    try {
      child.kill("SIGKILL")
    } catch {
      // already gone
    }
    for (const [k, v] of saved) {
      if (v === undefined) delete process.env[k]
      else process.env[k] = v
    }
    rmSync(dir, { recursive: true, force: true })
  }
})
