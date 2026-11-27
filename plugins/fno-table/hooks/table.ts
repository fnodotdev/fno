// Pure row and action logic for the /fno table. register.ts holds the Claude Code glue.

export type Tab = 'fleet' | 'backlog'

export type Row = {
  tab: Tab
  // The agent handle (fleet) or node id (backlog) the keys act on.
  id: string
  title: string
  fact: string
  // The node a fleet row works on, when it has one.
  node?: string
}

export type Action = { key: string; label: string; hotkey: string; argv: string[]; refusals?: Record<number, string> }

// What `fno backlog join` exits with when it refuses (skills/target/SKILL.md).
export const JOIN_REFUSALS: Record<number, string> = {
  2: 'no live claim on this node',
  3: 'the plan is one wide; nothing to join',
  4: 'no readable plan is bound',
  5: 'already joined',
  6: 'invalid name',
}

const clip = (text: unknown, max: number): string => {
  const s = String(text ?? '').replace(/\s+/g, ' ').trim()
  return s.length > max ? `${s.slice(0, max - 1)}…` : s
}

// Live registered agents, working ones first.
export function fleetRows(list: any): Row[] {
  const agents: any[] = Array.isArray(list?.agents) ? list.agents : []
  return agents
    .filter(a => a?.liveness === 'alive' && typeof a?.name === 'string')
    .sort((a, b) => Number(b.status === 'writing') - Number(a.status === 'writing'))
    .map(a => ({
      tab: 'fleet' as const,
      id: a.name,
      title: clip(a.harness_title || a.name, 48),
      fact: [a.status, a.node, a.pr ? `PR ${a.pr}` : ''].filter(Boolean).join(' · '),
      ...(typeof a.node === 'string' && a.node ? { node: a.node } : {}),
    }))
}

// In progress first, then on deck: the board's own order.
export function backlogRows(board: any): Row[] {
  const rows: Row[] = []
  for (const section of ['in_progress', 'on_deck'] as const) {
    for (const r of board?.[section]?.rows ?? []) {
      if (typeof r?.id !== 'string') continue
      rows.push({ tab: 'backlog', id: r.id, title: clip(r.title, 64), fact: clip(r.fact, 40), node: r.id })
    }
  }
  return rows
}

// What `fno backlog get` says about a node, reduced to what join needs.
export type NodeFacts = { planBound: boolean; claimed: boolean; joinRefused?: boolean }

export function nodeFacts(node: any): NodeFacts {
  return { planBound: typeof node?.plan_path === 'string' && node.plan_path !== '', claimed: Boolean(node?.locked_by) }
}

// Join shows only when it can fire: a live claim and a bound plan, and no refusal read from an earlier press.
export function canJoin(facts: NodeFacts | undefined): boolean {
  return Boolean(facts && facts.claimed && facts.planBound && !facts.joinRefused)
}

export function actionsFor(row: Row, facts?: NodeFacts): Action[] {
  const out: Action[] = []
  if (row.tab === 'backlog') {
    out.push({ key: 'spawn', label: 'spawn', hotkey: 's', argv: ['fno', 'agent', 'spawn', row.id, `/fno:target ${row.id}`] })
    out.push({ key: 'target', label: 'target', hotkey: 't', argv: ['fno', 'do', 'target', 'start', row.id] })
  }
  if (row.node && canJoin(facts)) out.push({ key: 'join', label: 'join', hotkey: 'j', argv: ['fno', 'backlog', 'join', row.node], refusals: JOIN_REFUSALS })
  if (row.tab === 'fleet') {
    out.push({ key: 'peek', label: 'peek', hotkey: 'p', argv: ['fno', 'agent', 'session', 'peek', row.id] })
    out.push({ key: 'open', label: 'open in mux', hotkey: 'o', argv: ['fno', 'mux', 'view', row.id] })
  }
  return out
}

// A refused press reads as one line; any other nonzero exit shows the command's own last line.
export function outcomeLine(action: Action, exitCode: number, stdout: string, stderr: string): string {
  if (exitCode === 0) return `${action.label}: ok${stdout.trim() ? ` · ${clip(stdout.trim().split('\n').pop(), 80)}` : ''}`
  const known = action.refusals?.[exitCode]
  if (known) return `${action.label}: ${known}`
  return `${action.label} exited ${exitCode}: ${clip((stderr || stdout).trim().split('\n').pop(), 80)}`
}
