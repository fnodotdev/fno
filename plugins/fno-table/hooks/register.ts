import type { EngineInterface, On } from 'claude-code'

import { type Action, type NodeFacts, type Row, type Tab, actionsFor, backlogRows, fleetRows, nodeFacts, outcomeLine } from './table'

const PANE_ID = 'fno-table'
const POLL_MS = 10_000
const PANE_COLUMNS = 56
const MAX_ROWS = 14

let tab: Tab = 'fleet'
let rows: Record<Tab, Row[]> = { fleet: [], backlog: [] }
let selected = 0
let status = ''
let isOpen = false
const facts = new Map<string, NodeFacts>()

async function runJson($: EngineInterface, argv: string[]): Promise<any> {
  try {
    const out = await $.process.run(argv, { timeoutMs: 20_000 })
    return out.exitCode === 0 ? JSON.parse(out.stdout) : undefined
  } catch {
    return undefined
  }
}

async function refresh($: EngineInterface): Promise<void> {
  const [list, board] = await Promise.all([
    runJson($, ['fno', 'agent', 'session', 'list', '--json']),
    runJson($, ['fno', 'backlog', 'board', '--json']),
  ])
  if (list) rows.fleet = fleetRows(list)
  if (board) rows.backlog = backlogRows(board)
  selected = Math.min(selected, Math.max(0, rows[tab].length - 1))
  await readFacts($)
  $.ui.invalidate('ui.render')
}

// Join needs the node's claim and plan; read them for the selected row only.
async function readFacts($: EngineInterface): Promise<void> {
  const row = rows[tab][selected]
  if (!row?.node) return
  const prior = facts.get(row.node)
  const node = await runJson($, ['fno', 'backlog', 'get', row.node])
  if (node) facts.set(row.node, { ...nodeFacts(node), ...(prior?.joinRefused ? { joinRefused: true } : {}) })
}

async function press($: EngineInterface, row: Row, action: Action): Promise<void> {
  status = `${action.label}…`
  $.ui.invalidate('ui.render')
  try {
    const out = await $.process.run(action.argv, { timeoutMs: 60_000 })
    status = outcomeLine(action, out.exitCode, out.stdout, out.stderr)
    // A refusal other than a transient one means join will not fire for this node until it changes.
    if (action.key === 'join' && row.node && out.exitCode >= 3 && out.exitCode <= 5) {
      facts.set(row.node, { ...(facts.get(row.node) ?? { planBound: false, claimed: false }), joinRefused: true })
    }
  } catch (err) {
    status = `${action.label} failed: ${String(err).slice(0, 80)}`
  }
  $.ui.invalidate('ui.render')
}

export function register(on: On) {
  on('session.start', async ($, e, next) => {
    $.clock.every(POLL_MS, async () => {
      if (isOpen) await refresh($).catch(() => {})
    })
    try {
      await $.command.register({ name: 'fno', description: 'Your fno fleet and backlog as a table', argumentHint: '[fleet|backlog|close]', immediate: true })
    } catch {
      // A name already taken is not fatal; the pane still opens through /fno-table when that is free.
    }
    return next(e)
  })

  on('command.run', { command: 'fno' }, async ($, e) => {
    const arg = e.args.trim().toLowerCase()
    if (arg === 'close') {
      isOpen = false
      await $.ui.close({ id: PANE_ID }).catch(() => {})
      return { text: 'fno table closed' }
    }
    if (arg === 'fleet' || arg === 'backlog') tab = arg
    selected = 0
    isOpen = true
    await $.ui.open({ id: PANE_ID, title: 'fno', columns: PANE_COLUMNS, focus: true, closeOnEscape: true })
    await refresh($)
    return { text: `fno table: ${tab}` }
  })

  on('ui.render', { component: 'Pane' }, async ($, e, next) => {
    if (e.requestId !== PANE_ID) return next(e)
    const { Box, Text, Button } = $.ui.resolve(e)
    const list = rows[tab]
    const row = list[selected]
    const actions = row ? actionsFor(row, row.node ? facts.get(row.node) : undefined) : []
    const switchTab = (to: Tab) => async () => {
      tab = to
      selected = 0
      await readFacts($)
      $.ui.invalidate('ui.render')
    }
    return Box({
      flexDirection: 'column',
      children: [
        Box({
          flexDirection: 'row',
          children: [
            Button({ key: 'tab-fleet', label: tab === 'fleet' ? '[fleet]' : 'fleet', hotkey: 'f', plain: true, onPress: switchTab('fleet') }),
            Text({ children: ['  '] }),
            Button({ key: 'tab-backlog', label: tab === 'backlog' ? '[backlog]' : 'backlog', hotkey: 'b', plain: true, onPress: switchTab('backlog') }),
          ],
        }),
        ...(list.length === 0
          ? [Text({ dimColor: true, children: [tab === 'fleet' ? 'no live agents' : 'nothing in progress or on deck'] })]
          : list.slice(0, MAX_ROWS).map((r, i) =>
              Button({
                key: `row-${i}`,
                label: `${i === selected ? '>' : ' '} ${r.id}  ${r.title}${r.fact ? `  (${r.fact})` : ''}`,
                plain: true,
                dimColor: i !== selected,
                onPress: async () => {
                  selected = i
                  await readFacts($)
                  $.ui.invalidate('ui.render')
                },
              }),
            )),
        Box({
          flexDirection: 'row',
          children: actions.flatMap((a, i) => [
            ...(i ? [Text({ children: ['  '] })] : []),
            Button({ key: `act-${a.key}`, label: a.label, hotkey: a.hotkey, onPress: () => press($, row!, a) }),
          ]),
        }),
        ...(status ? [Text({ dimColor: true, wrap: 'wrap', children: [status] })] : []),
      ],
    })
  })
}
