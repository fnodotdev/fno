import { expect, test } from 'claude-code/testing'

import { JOIN_REFUSALS, actionsFor, backlogRows, canJoin, fleetRows, nodeFacts, outcomeLine } from './table'

const board = {
  in_progress: { rows: [{ id: 'x-1', title: 'A task', fact: 'no PR yet' }] },
  on_deck: { rows: [{ id: 'x-2', title: 'Next task', fact: 'ready' }] },
}

test('backlog rows keep the board order and offer spawn and target', () => {
  const rows = backlogRows(board)
  expect(rows.map(r => r.id)).toEqual(['x-1', 'x-2'])
  expect(actionsFor(rows[0]).map(a => a.key)).toEqual(['spawn', 'target'])
})

test('fleet rows drop dead agents and list working ones first', () => {
  const rows = fleetRows({
    agents: [
      { name: 'a', liveness: 'alive', status: 'quiet' },
      { name: 'b', liveness: 'dead', status: 'writing' },
      { name: 'c', liveness: 'alive', status: 'writing', node: 'x-9' },
    ],
  })
  expect(rows.map(r => r.id)).toEqual(['c', 'a'])
  expect(actionsFor(rows[0]).map(a => a.key)).toEqual(['peek', 'open'])
})

test('join shows only with a claim and a plan, and hides after a refusal', () => {
  const row = fleetRows({ agents: [{ name: 'c', liveness: 'alive', node: 'x-9' }] })[0]
  expect(actionsFor(row, nodeFacts({ plan_path: 'p.md', locked_by: 's' })).map(a => a.key)).toContain('join')
  expect(canJoin(nodeFacts({ plan_path: 'p.md' }))).toBe(false)
  expect(canJoin(nodeFacts({ locked_by: 's' }))).toBe(false)
  expect(canJoin({ planBound: true, claimed: true, joinRefused: true })).toBe(false)
})

test('a join refusal reads as one line', () => {
  const join = actionsFor(fleetRows({ agents: [{ name: 'c', liveness: 'alive', node: 'x-9' }] })[0], { planBound: true, claimed: true }).find(a => a.key === 'join')!
  expect(outcomeLine(join, 3, '', '')).toBe(`join: ${JOIN_REFUSALS[3]}`)
  expect(outcomeLine(join, 0, 'joined 2 workers\n', '')).toBe('join: ok · joined 2 workers')
})
