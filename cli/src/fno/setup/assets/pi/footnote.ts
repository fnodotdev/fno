// footnote <-> pi bridge extension (transport only).
//
// Installed by `fno config setup` into <pi agent dir>/extensions/footnote.ts
// (pi auto-discovers *.ts there; no package publish required). Plain
// TypeScript against node builtins only, so pi loads it with no install step.
//
// This extension is a TRANSPORT and nothing else. Every decision belongs to
// Rust: the gate (`fno-agents loop-check`) decides, the store commands answer,
// and this file moves bytes. Concretely it carries four abilities:
//
//   1. DISCOVERY: it tells pi where the Footnote skills live, so a native
//      pi session sees `/skill:target` in its own catalog.
//   2. MANIFEST: it asks `fno-agents state path target-state` where the
//      session manifest for this directory lives (the space-resolved path,
//      never a guess like `<cwd>/.fno/target-state.md`).
//   3. BINDING: every settle passes its own session id to the gate with
//      `--harness pi --harness-session <id>`, so the gate answers whether
//      THIS session may drive this target. A foreign session gets a typed
//      refusal, sends nothing, and records why.
//   4. CONTINUATION: on a non-terminal block it sends exactly the string the
//      gate named, with prompt expansion enabled so a skill command reaches
//      the skill. A block without a continuation sends nothing.
//
// Beside the gate it runs the claude hook CARRIERS pi had no wire for, in
// the claude chain order hooks.json declares (the same reference the pretool
// host reads for the block guards):
//
//   - `session_start` runs the SessionStart producers. pi's handler cannot
//     return context, so their text queues and the next `before_agent_start`
//     delivers it - the same boundary that already carries the announce and
//     the post-compact role re-inject.
//   - `before_agent_start` is pi's UserPromptSubmit: the inject-mail-notify
//     and node-bind prompt carriers ride every prompt.
//   - `tool_result` is pi's PostToolUse: format-on-edit, edit-integrity,
//     claim-heartbeat and merge-capture run in claude order (format before
//     integrity). pi hands back no pre-edit text, so edit-integrity's
//     originalFile leg degrades to its git fallback; and pi's tool_result
//     can return content only, never context, so a carrier finding lands in
//     the session transcript via appendEntry instead of the model's turn -
//     the one parity gap, shared with the opencode wire.
//   - `session_before_compact` is pi's PreCompact: save-session and
//     precompact-canon-doc run, the trigger maps manual|auto, and the
//     handler never cancels the compaction.
//
// Every call is bounded: a wedged child is killed and the settle records one
// `unavailable` entry instead of hanging or fabricating a termination.
// loop-check stays the SOLE completion authority: the extension never
// decides "done", never re-drives on a failure, and never emits a
// termination itself.

import { execFile } from "node:child_process"
import { existsSync, readFileSync, unlinkSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { homedir } from "node:os"
import { join } from "node:path"

type Ctx = {
  sessionManager?: {
    getSessionId?: () => string
    buildContextEntries?: () => unknown[]
    getBranch?: () => unknown[]
    getSessionFile?: () => string | undefined
  }
  model?: { id?: string; provider?: string }
  isIdle?: () => boolean
  hasUI?: boolean
  ui?: { notify?: (message: string, level?: string) => unknown }
}

// The gate bound, overridable so a test can drive the timeout path in
// milliseconds instead of five minutes.
function gateTimeoutMs(): number {
  const raw = Number(process.env.FNO_PI_GATE_TIMEOUT_MS || "")
  return Number.isFinite(raw) && raw > 0 ? raw : 300000
}

function distresTimeoutMs(): number {
  const raw = Number(process.env.FNO_PI_DISTRESS_TIMEOUT_MS || "")
  return Number.isFinite(raw) && raw > 0 ? raw : 30000
}

function statePathTimeoutMs(): number {
  const raw = Number(process.env.FNO_PI_STATE_TIMEOUT_MS || "")
  return Number.isFinite(raw) && raw > 0 ? raw : 5000
}

// The pretool host bound: the whole claude block-guard chain (up to six
// guards, one subprocess each) measured at p95 826ms with a 4.2s cold-CLI
// max, so the bound sits above the cold max instead of pi's old 5s lead
// guard bound.
function pretoolTimeoutMs(): number {
  const raw = Number(process.env.FNO_PI_PRETOOL_TIMEOUT_MS || "")
  return Number.isFinite(raw) && raw > 0 ? raw : 10000
}

// Per-script bound for the carrier chains. A carrier blocks its boundary
// (a prompt, a tool result, a compaction), so a wedged script dies here
// instead of holding the session; each script also carries its own internal
// hook budget, making this the outer backstop.
function carrierTimeoutMs(): number {
  const raw = Number(process.env.FNO_PI_CARRIER_TIMEOUT_MS || "")
  return Number.isFinite(raw) && raw > 0 ? raw : 15000
}

// The claude hook chains this extension runs, one `[script, ...argv]` entry
// per claude hooks.json row, in claude order. The chain is the reference;
// a script added to hooks.json reaches pi when its name lands here.
const SESSION_START_CHAIN: string[][] = [
  ["session-start-using-fno.sh"],
  ["context-run.sh", "claude-session-start"],
  ["daemon-liveness-session-start.sh"],
  ["bootstrap-session-start.sh"],
]
const PROMPT_CARRIERS: string[][] = [
  ["inject-mail-notify.sh"],
  ["node-bind-prompt-submit.sh"],
]
const POSTTOOL_CARRIERS: string[][] = [
  ["format-on-edit.sh"],
  ["edit-integrity.sh"],
  ["claim-heartbeat.sh"],
  ["merge-capture.sh"],
]
const PRECOMPACT_CARRIERS: string[][] = [
  ["save-session.sh"],
  ["precompact-canon-doc.sh"],
]

// Run one carrier chain and return the context text each script printed
// (additionalContext when the stdout carries claude hook JSON, else the raw
// stdout). Sequential by default: PostToolUse order is load-bearing
// (format-on-edit runs before edit-integrity reads the file it formatted);
// SessionStart producers are independent and run in parallel.
async function runCarrierChain(
  root: string,
  chains: string[][],
  payload: string,
  timeoutMs: number,
  parallel = false,
): Promise<string[]> {
  const run = (chain: string[]): Promise<string> =>
    runBounded(
      "bash",
      [join(root, "hooks", chain[0] || ""), ...chain.slice(1)],
      timeoutMs,
      payload,
      { FNO_HOOK_HARNESS: "pi" },
    ).then(contextCarrierText)
  const outs = parallel
    ? Promise.all(chains.map(run))
    : (async () => {
        const acc: string[] = []
        for (const chain of chains) acc.push(await run(chain))
        return acc
      })()
  return (await outs).filter(Boolean)
}

// pi tool names to the claude names the carriers match on (the same three
// mappings the Rust pretool adapter makes; everything else rides verbatim,
// unjudged by name).
function claudeToolName(name: string): string {
  if (name === "bash") return "Bash"
  if (name === "edit") return "Edit"
  if (name === "write") return "Write"
  return name
}

// The text part of a pi tool result, joined: the shape the PostToolUse
// payload's tool_response.output carries so claim-heartbeat's error read
// and merge-capture's provenance door see the same strings claude sends.
function contentText(content: unknown): string {
  if (!Array.isArray(content)) return ""
  return content
    .filter(
      (p): p is { type: "text"; text: string } =>
        !!p &&
        typeof p === "object" &&
        (p as { type?: string }).type === "text" &&
        typeof (p as { text?: unknown }).text === "string",
    )
    .map((p) => p.text)
    .join("")
}

// One bounded child run. Resolves "" when the child failed for any reason
// (spawn error, nonzero exit, timeout) - the caller records why it could not
// read an answer rather than guessing one. An optional stdin payload rides
// to the child (the hook commands read a claude-shaped payload on stdin),
// and optional extra env rides beside the shared FNO_HOOK_CONTEXT stamp.
function runBounded(
  bin: string,
  args: string[],
  timeoutMs: number,
  stdin?: string,
  extraEnv?: Record<string, string>,
): Promise<string> {
  return new Promise((resolve) => {
    try {
      const child = execFile(
        bin,
        args,
        {
          timeout: timeoutMs,
          killSignal: "SIGKILL",
          // Hook context: a child's fno call never joins the session.
          env: { ...process.env, FNO_HOOK_CONTEXT: "1", ...(extraEnv || {}) },
          maxBuffer: 10 * 1024 * 1024,
        },
        (err, stdout) => resolve(err ? "" : String(stdout)),
      )
      if (stdin !== undefined && child.stdin) {
        // A fast-exiting child can close the pipe before the payload lands;
        // the resulting EPIPE is an uncaught exception without this.
        child.stdin.on("error", () => {})
        child.stdin.write(stdin)
        child.stdin.end()
      }
    } catch {
      resolve("")
    }
  })
}

// Per-turn session-state push: working and done reach the daemon row through
// `fno-agents hook session-state --harness pi <Event>`, the same entry claude
// and codex use, so the row reads inside-leg state and a verified model
// instead of the screen scrape. The payload is claude-shaped;
// hook/adapter/pi.rs reads it. pi exposes no permission-prompt event, so
// there is no blocked push. Fire-and-forget: a missing binary or a failed run
// costs the push, never the turn.
function pushState(
  event: string,
  ctx: unknown,
  extra: Record<string, unknown> = {},
): void {
  try {
    const c = ctx as Ctx
    const sid = c?.sessionManager?.getSessionId?.() || ""
    if (!sid) return
    const m = c?.model
    const model = m?.id ? (m.provider ? `${m.provider}/${m.id}` : m.id) : ""
    const payload = JSON.stringify({
      hook_event_name: event,
      cwd: process.cwd(),
      session_id: sid,
      ...(model ? { model } : {}),
      ...extra,
    })
    const bin = process.env.FNO_AGENTS_BIN || "fno-agents"
    void runBounded(bin, ["hook", "session-state", "--harness", "pi", event], 3000, payload)
  } catch {
    // best-effort
  }
}

// The space-resolved manifest for `dir`, when one exists. The command is the
// only path authority: a directory with no `.fno` of its own still gates,
// because the manifest lives under the session's space root.
async function resolveManifestPath(bin: string, dir: string): Promise<string | null> {
  const out = await runBounded(bin, ["state", "path", "target-state"], statePathTimeoutMs())
  const candidate = out.trim().split("\n").pop()?.trim()
  if (!candidate || !existsSync(candidate)) return null
  void dir
  return candidate
}

// The plugin root the install pointer names, or null with one `[footnote]`
// line naming why. A stale or missing pointer degrades to silence, never to
// an error. The pointer moved under install/ (the state-root tidiness wave);
// an unmigrated install keeps the legacy root name until the daemon migrates.
function pluginRoot(): string | null {
  const base = process.env.FNO_HOME || join(homedir(), ".fno")
  for (const name of ["install/plugin-root", "plugin-root"]) {
    try {
      const root = readFileSync(join(base, name), "utf8").trim()
      if (root && existsSync(join(root, ".claude-plugin", "plugin.json"))) return root
    } catch {
      // try the next spelling
    }
  }
  console.error("[footnote] no plugin-root pointer; hook carriers not offered")
  return null
}

// The skills directory under the plugin root, or null.
function skillsRoot(): string | null {
  const root = pluginRoot()
  if (!root) return null
  if (!existsSync(join(root, "skills"))) {
    console.error("[footnote] plugin root carries no skills dir; skills not offered")
    return null
  }
  return join(root, "skills")
}

// Build the minimal transcript loop-check scans. Its detect_intent_full
// filters lines on /message/role == "assistant" AND extract_assistant_text
// reads /message/content - BOTH are required, so each line carries both
// fields. pi stores content as typed blocks ({type:"text",text}), so the
// text parts are joined per assistant message.
function synthesizeTranscript(entries: unknown[]): string {
  const lines: string[] = []
  for (const entry of entries) {
    const message = (entry as { message?: { role?: string; content?: unknown } })
      ?.message
    if (message?.role !== "assistant") continue
    const content = Array.isArray(message.content) ? message.content : []
    const text = content
      .filter(
        (p): p is { type: "text"; text: string } =>
          !!p &&
          typeof p === "object" &&
          (p as { type?: string }).type === "text" &&
          typeof (p as { text?: unknown }).text === "string",
      )
      .map((p) => p.text)
      .join("")
    if (!text) continue
    lines.push(JSON.stringify({ message: { role: "assistant", content: text } }))
  }
  return lines.length ? lines.join("\n") + "\n" : ""
}

function lastAssistantText(entries: unknown[]): string {
  for (let i = entries.length - 1; i >= 0; i--) {
    const message = (entries[i] as { message?: { role?: string; content?: unknown } })?.message
    if (message?.role !== "assistant") continue
    const content = Array.isArray(message.content) ? message.content : []
    const text = content
      .filter(
        (p): p is { type: "text"; text: string } =>
          !!p &&
          typeof p === "object" &&
          (p as { type?: string }).type === "text" &&
          typeof (p as { text?: unknown }).text === "string",
      )
      .map((p) => p.text)
      .join("")
    if (text) return text
  }
  return ""
}
// The context text a carrier hook printed: the claude JSON shape's
// additionalContext, else the raw trimmed stdout. A carrier that answered
// nothing injects nothing.
function contextCarrierText(stdout: string): string {
  const t = (stdout || "").trim()
  if (!t) return ""
  try {
    const v = JSON.parse(t)
    const ctx = v?.hookSpecificOutput?.additionalContext
    if (typeof ctx === "string" && ctx) return ctx
    return ""
  } catch {
    return t
  }
}

export default function (pi: {
  on: (
    event: string,
    handler: (event: unknown, ctx: unknown) => Promise<unknown> | void,
  ) => void
  sendUserMessage: (
    content: string,
    options?: {
      deliverAs?: string
      triggerTurn?: boolean
      expandPromptTemplates?: boolean
    },
  ) => void
  appendEntry?: (type: string, data: unknown) => void
}): void {
  // Per-session in-flight guard. This is scheduling, never authority: one
  // loop-check at a time per pi session, and the binding gate still answers
  // who owns the target.
  const busy = new Map<string, boolean>()

  // Context text queued for the next prompt boundary: the SessionStart
  // producers run where pi cannot deliver (its session_start handler has no
  // context return), and the PreCompact carriers run before the compaction
  // their text should survive. Drained, then cleared, at the next
  // before_agent_start.
  const pendingContext = new Map<string, string[]>()

  pi.on("session_shutdown", (_event: unknown, ctx: unknown) => {
    const sid = (ctx as Ctx)?.sessionManager?.getSessionId?.()
    if (sid) {
      busy.delete(sid)
      pendingContext.delete(sid)
    }
  })

  // DISCOVERY: put the Footnote commands in pi's own catalog as skills.
  pi.on("resources_discover", () => {
    const skills = skillsRoot()
    return skills ? { skillPaths: [skills] } : {}
  })

  // SESSIONSTART PRODUCERS: the claude SessionStart chain, run at pi's own
  // session start (startup, new, reload, resume, fork) and queued for the
  // next prompt boundary. Fail-open: no root or a failed producer costs its
  // text, never the session. The claude payload's source maps from pi's
  // reason (resume and fork are resumptions; everything else is a startup).
  pi.on("session_start", async (event: unknown, ctx: unknown) => {
    try {
      const sid = (ctx as Ctx)?.sessionManager?.getSessionId?.() || ""
      const root = pluginRoot()
      if (!sid || !root) return
      const reason = (event as { reason?: string })?.reason || "startup"
      const source = reason === "resume" || reason === "fork" ? "resume" : "startup"
      const payload = JSON.stringify({
        hook_event_name: "SessionStart",
        source,
        cwd: process.cwd(),
        session_id: sid,
      })
      const texts = await runCarrierChain(
        root,
        SESSION_START_CHAIN,
        payload,
        carrierTimeoutMs(),
        true,
      )
      if (texts.length) {
        const q = pendingContext.get(sid) || []
        q.push(...texts)
        pendingContext.set(sid, q)
      }
    } catch {
      // best-effort: a failed producer chain never blocks session start
    }
  })

  // Fleet announcements at the pre-turn boundary.
  // `before_agent_start` fires after a prompt and before the agent loop, and
  // its returned `message` is injected into the session and sent to the LLM -
  // the one pi event that delivers text at a real boundary. One announcement
  // is one bus line; the per-session cursor on the reader side makes this
  // print once and stay silent after. Fail-open: no binary, no output, or a
  // failed read injects nothing.
  // Post-compact re-inject: pi exposes session_compact, so the compaction
  // marks THIS session and the next `before_agent_start` boundary carries
  // the lead's rules again (the same carrier claude runs at SessionStart
  // source=compact). One message per compact; the mark clears on delivery
  // or on a failed carrier - never re-armed mid-turn.
  const rolePending = new Set<string>()
  pi.on("session_compact", (_event: unknown, ctx: unknown) => {
    const sid = (ctx as Ctx)?.sessionManager?.getSessionId?.()
    if (sid) rolePending.add(sid)
  })

  // PRECOMPACT CARRIERS: pi's session_before_compact is claude's PreCompact.
  // save-session snapshots the transcript (the real pi session file when the
  // manager names one) and canon-doc writes the handoff doc; their pointer
  // text queues for the next prompt boundary, where the post-compact role
  // re-inject lands too. The trigger maps manual|auto from pi's reason
  // (threshold and overflow are the automatic pair). The handler returns
  // nothing, so it can never cancel the compaction.
  pi.on("session_before_compact", async (event: unknown, ctx: unknown) => {
    try {
      const ev = event as { reason?: string; customInstructions?: string }
      const sid = (ctx as Ctx)?.sessionManager?.getSessionId?.() || ""
      const root = pluginRoot()
      if (!root) return
      const transcript = (ctx as Ctx)?.sessionManager?.getSessionFile?.() || ""
      const payload = JSON.stringify({
        hook_event_name: "PreCompact",
        trigger: ev?.reason === "manual" ? "manual" : "auto",
        custom_instructions: ev?.customInstructions || "",
        cwd: process.cwd(),
        session_id: sid,
        ...(transcript ? { transcript_path: transcript } : {}),
      })
      const texts = await runCarrierChain(root, PRECOMPACT_CARRIERS, payload, carrierTimeoutMs())
      if (sid && texts.length) {
        const q = pendingContext.get(sid) || []
        q.push(...texts)
        pendingContext.set(sid, q)
      }
    } catch {
      // best-effort: a failed carrier chain never blocks the compaction
    }
  })

  // PROMPT BOUNDARY: every before_agent_start is pi's UserPromptSubmit.
  // One message returns, and pi's display flag is per-message, so the
  // boundary keeps two classes apart instead of joining them: context text
  // (the post-compact role re-inject, the queued SessionStart/PreCompact
  // carrier context, the prompt carriers) always answers alone and stays
  // off the transcript, exactly the claude context-only channel; the fleet
  // announce answers alone and renders. The announce read advances its
  // per-session cursor, so when context answers, the announce is not read
  // at all - it delivers, unread, at the next boundary.
  pi.on("before_agent_start", async (event: unknown, ctx: unknown) => {
    pushState("UserPromptSubmit", ctx)
    try {
      const sid = (ctx as Ctx)?.sessionManager?.getSessionId?.() || ""
      const sessionKey = process.env.FNO_AGENT_SESSION_ID || sid || `pi:${process.cwd()}`
      const bin = process.env.FNO_AGENTS_BIN || "fno-agents"
      const root = pluginRoot()
      const texts: string[] = []
      // Role re-inject first: fresh operating rules outrank an announce.
      // Fail-open: no root, a failed run, or an empty payload injects
      // nothing and drops the mark.
      if (sid && rolePending.delete(sid) && root) {
        const payload = JSON.stringify({
          hook_event_name: "SessionStart",
          source: "compact",
          cwd: process.cwd(),
          session_id: sid,
        })
        const out = await runBounded(
          "bash",
          [join(root, "hooks", "lead-postcompact-reinject.sh")],
          5000,
          payload,
        )
        // The using-fno carrier answers only for a session that joined fno.
        const preamble = await runBounded(
          "env",
          ["FNO_HOOK_HARNESS=pi", "bash", join(root, "hooks", "session-start-using-fno.sh")],
          5000,
          payload,
        )
        const text = [contextCarrierText(out), contextCarrierText(preamble)]
          .filter(Boolean)
          .join("\n\n")
        if (text) texts.push(text)
      }
      // Queued session-start and compact-carrier context: delivered once,
      // then dropped (a failed delivery drops the text, never re-arms).
      if (sid) {
        const queued = pendingContext.get(sid)
        if (queued?.length) {
          texts.push(...queued)
          pendingContext.delete(sid)
        }
      }
      // Prompt carriers: mail delivery and the first-turn node bind, with
      // the claude UserPromptSubmit payload (prompt text included).
      if (root && sid) {
        const payload = JSON.stringify({
          hook_event_name: "UserPromptSubmit",
          prompt: (event as { prompt?: string })?.prompt || "",
          cwd: process.cwd(),
          session_id: sid,
        })
        texts.push(...(await runCarrierChain(root, PROMPT_CARRIERS, payload, carrierTimeoutMs())))
      }
      // Session-start report: the registry holds this session's id (mail and
      // liveness stop guessing). Fire-and-forget, best-effort; a spawned
      // worker passes FNO_AGENT_SELF so the daemon can match its row.
      if (sid) {
        const reportArgs = [
          "report", "--kind", "session",
          "--harness", "pi",
          "--session-id", sid,
        ]
        if (process.env.FNO_AGENT_SELF) {
          reportArgs.push("--agent-self", process.env.FNO_AGENT_SELF)
        }
        runBounded(bin, reportArgs, 2000).catch(() => {})
      }
      // Context text answers alone, off the transcript; the announce read
      // waits for a boundary with no context, so its cursor never consumes
      // an announcement this boundary cannot show.
      if (texts.length) {
        return {
          message: {
            customType: "fno-role",
            content: [{ type: "text", text: texts.join("\n\n") }],
            display: false,
          },
        }
      }
      const out = await runBounded(
        bin,
        ["announce", "read", "--session-id", sessionKey, "--harness", "pi", "--boundary", "prompt"],
        2000,
      )
      const announce = out.trim()
      if (!announce) return
      return {
        message: {
          customType: "fno-announce",
          content: [{ type: "text", text: announce }],
          display: true,
        },
      }
    } catch {
      return
    }
  })

  // PRETOOL HOST: the tool call rides to `fno-agents hook pretool --wire pi`
  // and the host runs the full claude PreToolUse block guard chain
  // (hooks/hooks.json filtered by hook-classes.tsv) with the payload cwd and
  // the harness env, relaying the first deny as `{block: true, reason}`.
  // Fail-open: no root, no binary, an unreadable answer, or a failed run
  // returns nothing and the tool proceeds - the never-block contract every
  // shim ships under (docs/hooks-fail-open.md).
  pi.on("tool_call", async (event: unknown, ctx: unknown) => {
    pushState("PreToolUse", ctx, {
      tool_name: (event as { toolName?: string })?.toolName || "",
    })
    try {
      const ev = event as { toolName?: string; input?: Record<string, unknown> }
      const root = pluginRoot()
      if (!root) return
      const payload = JSON.stringify({
        toolName: ev?.toolName || "",
        input: ev?.input || {},
        cwd: process.cwd(),
        session_id: (ctx as Ctx)?.sessionManager?.getSessionId?.() || "",
      })
      const bin = process.env.FNO_AGENTS_BIN || "fno-agents"
      const out = await runBounded(
        bin,
        ["hook", "pretool", "--wire", "pi", "--plugin-root", root],
        pretoolTimeoutMs(),
        payload,
      )
      if (!out.trim()) return
      let decision: { block?: boolean; reason?: string } | null = null
      try {
        decision = JSON.parse(out)
      } catch {
        return
      }
      if (decision?.block === true) {
        return {
          block: true,
          reason: decision.reason || "blocked by footnote hook",
        }
      }
    } catch {
      return
    }
  })

  // POSTTOOL CARRIERS: pi's tool_result is claude's PostToolUse. The four
  // claude wait-class carriers run in claude order - format-on-edit before
  // edit-integrity, so integrity reads the file the formatter just wrote.
  // The payload is the claude shape: tool_name mapped, input.path promoted
  // to tool_input.file_path (the same promotion the Rust pretool adapter
  // makes), and a tool_response carrying pi's isError and text output - pi
  // exposes no pre-edit text, so edit-integrity's originalFile leg falls
  // back to git. pi's tool_result can return content only, never context,
  // so a carrier finding lands in the session transcript (appendEntry),
  // not the model's turn - the same channel limit the opencode wire has.
  // Awaited so a carrier never races the next tool call.
  pi.on("tool_result", async (event: unknown, ctx: unknown) => {
    try {
      const ev = event as {
        toolName?: string
        input?: Record<string, unknown>
        content?: unknown
        isError?: boolean
      }
      const root = pluginRoot()
      if (!root) return
      const input: Record<string, unknown> = { ...(ev?.input || {}) }
      if (!("file_path" in input)) {
        const p = (input as { path?: unknown }).path
        if (typeof p === "string") input.file_path = p
      }
      const payload = JSON.stringify({
        hook_event_name: "PostToolUse",
        tool_name: claudeToolName(ev?.toolName || ""),
        tool_input: input,
        tool_response: { is_error: ev?.isError === true, output: contentText(ev?.content) },
        cwd: process.cwd(),
        session_id: (ctx as Ctx)?.sessionManager?.getSessionId?.() || "",
      })
      const texts = await runCarrierChain(root, POSTTOOL_CARRIERS, payload, carrierTimeoutMs())
      if (texts.length) {
        try {
          pi.appendEntry?.("fno-carriers", { event: "PostToolUse", context: texts })
        } catch {
          // an appendEntry that throws is the host's problem
        }
      }
    } catch {
      // fire-and-forget: a failed carrier chain never fails the tool result
    }
  })

  pi.on("agent_settled", async (_event: unknown, ctx: unknown) => {
    pushState("Stop", ctx)
    const dir = process.cwd()
    const sm = (ctx as Ctx)?.sessionManager
    const sid = sm?.getSessionId?.() || ""
    const bin = process.env.FNO_AGENTS_BIN || "fno-agents"

    // MANIFEST: the command is the presence guard and the gate's --state.
    const manifestPath = await resolveManifestPath(bin, dir)
    if (!manifestPath) {
      // No footnote session here. A worker that dies before `target init`
      // writes a manifest still carries a distress tag nobody would otherwise
      // read, so the pre-manifest scan keeps the spawned-run requirement
      // (FNO_AGENT_SESSION_ID is the run id it reports under).
      if (!process.env.FNO_AGENT_SESSION_ID) return
      const preSynth = join(
        tmpdir(),
        `.pi-premanifest-${process.pid}-${Date.now()}.jsonl`,
      )
      try {
        const read = sm?.buildContextEntries ?? sm?.getBranch
        const entries = read ? (read.call(sm) as unknown[]) : []
        writeFileSync(preSynth, synthesizeTranscript(entries))
        await runBounded(
          bin,
          [
            "distress-scan",
            "--transcript",
            preSynth,
            "--run",
            String(process.env.FNO_AGENT_SESSION_ID),
            "--harness",
            "pi",
            "--cwd",
            dir,
          ],
          distresTimeoutMs(),
        )
      } catch (e: unknown) {
        console.error(`[footnote] pre-manifest distress-scan skipped (non-fatal): ${e}`)
      } finally {
        try {
          unlinkSync(preSynth)
        } catch {
          // nothing to clean up / already gone
        }
      }
      return
    }

    // ctx.isIdle() is true at agent_settled unless another extension started
    // a run; a busy pi is mid-re-drive or mid-tool and the next settle comes.
    if ((ctx as Ctx)?.isIdle?.() === false) return
    if (!sid || busy.get(sid)) return
    busy.set(sid, true)

    let decision: {
      decision?: string
      termination_reason?: string
      continuation?: string
      message?: string
      reason?: string
    } | null = null
    let unavailableReason = ""
    // Declared before the try so the finally can clean it up on every path.
    const synth = join(tmpdir(), `.pi-loopcheck-${process.pid}-${Date.now()}.jsonl`)
    try {
      // Read this session's assistant messages and synthesize the transcript
      // the gate scans.
      const read = sm?.buildContextEntries ?? sm?.getBranch
      const entries = read ? (read.call(sm) as unknown[]) : []
      writeFileSync(synth, synthesizeTranscript(entries))

      // Event rules: the Stop-boundary rule table runs through the transport
      // entry, before the gate, with the same Stop payload shape claude's
      // stop hook evaluates in-process. A block relays to the session in
      // place of the gate and the settle ends.
      const lastText = lastAssistantText(entries)
      const rulesOut = await runBounded(
        bin,
        ["hook", "rules", "--event", "stop"],
        gateTimeoutMs(),
        JSON.stringify({
          session_id: sid,
          last_assistant_message: lastText,
          transcript_path: synth,
          cwd: dir,
        }),
      )
      if (rulesOut) {
        try {
          const ruled = JSON.parse(rulesOut) as { decision?: string; reason?: string }
          if (ruled.decision === "block") {
            try {
              pi.appendEntry?.("fno-rules", { state: "blocked", reason: ruled.reason || "" })
            } catch {
              // best-effort record
            }
            try {
              pi.sendUserMessage(ruled.reason || "an event rule blocked this stop", {
                deliverAs: "followUp",
                triggerTurn: true,
              })
            } catch (e: unknown) {
              console.error(`[footnote] rules relay failed: ${e}`)
            }
            return
          }
        } catch (parseErr: unknown) {
          console.error(`[footnote] hook rules unparseable: ${parseErr}`)
        }
      }

      const out = await runBounded(
        bin,
        [
          "loop-check",
          "--state",
          manifestPath,
          "--transcript",
          synth,
          "--cwd",
          dir,
          "--harness",
          "pi",
          "--harness-session",
          sid,
        ],
        gateTimeoutMs(),
      )
      if (!out) {
        unavailableReason = "loop-check failed or timed out"
      } else {
        try {
          decision = JSON.parse(out)
        } catch (parseErr: unknown) {
          unavailableReason = `unparseable gate output: ${parseErr}`
        }
      }
    } catch (e: unknown) {
      // A failed transcript read or write must not throw out of a hook: the
      // session settles ungated (as it would with no extension) rather than
      // dying mid-settle.
      unavailableReason = `settle handling failed: ${e}`
    } finally {
      try {
        unlinkSync(synth)
      } catch {
        // nothing to clean up / early return skipped the write
      }
      busy.delete(sid)
    }

    if (unavailableReason) {
      // Never re-drive on an unreadable gate, and never fabricate a
      // termination: record the fact once, visibly.
      try {
        pi.appendEntry?.("fno-gate", { state: "unavailable", reason: unavailableReason })
      } catch {
        // an appendEntry that throws is the host's problem, not a gate answer
      }
      if ((ctx as Ctx)?.hasUI && (ctx as Ctx)?.ui?.notify) {
        try {
          ;(ctx as Ctx).ui?.notify?.(`[footnote] gate unavailable: ${unavailableReason}`, "warning")
        } catch {
          // notify is best-effort
        }
      }
      return
    }
    if (!decision) return
    // The binding refused this session: send nothing, record why. This turns
    // a wrong-session continuation (the audit's one foreign continuation)
    // into a typed refusal.
    if (decision.decision === "refuse") {
      try {
        pi.appendEntry?.("fno-gate", {
          state: "refused",
          reason: decision.reason || decision.message || "",
        })
      } catch {
        // best-effort record
      }
      return
    }
    // Terminal: loop-check already emitted `termination` - let the session
    // end and emit nothing extra (no duplicate termination event).
    if (decision.termination_reason) return
    // Non-terminal block: send EXACTLY the continuation the gate named, with
    // prompt expansion so a skill command reaches the skill. followUp +
    // triggerTurn covers both an idle pi (sends immediately, triggers a turn)
    // and a still-settling one (queues behind the current run). Fire-and-
    // forget; the next turn's agent_settled runs the gate again. loop-check's
    // NoProgress backstop bounds a stuck loop.
    if (decision.decision === "block") {
      const continuation = decision.continuation
      if (!continuation) {
        try {
          pi.appendEntry?.("fno-gate", {
            state: "unavailable",
            reason: "gate named no continuation",
          })
        } catch {
          // best-effort record
        }
        return
      }
      try {
        pi.sendUserMessage(continuation, {
          deliverAs: "followUp",
          triggerTurn: true,
          expandPromptTemplates: true,
        })
      } catch (e: unknown) {
        console.error(`[footnote] re-drive sendUserMessage threw: ${e}`)
      }
    }
  })
}
