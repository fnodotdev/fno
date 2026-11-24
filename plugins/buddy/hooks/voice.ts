import { type Bones, type Companion, STAT_NAMES, type StatName, peakStat } from './companion'

function dumpStat(c: Bones): StatName {
  return STAT_NAMES.reduce((a, b) => (c.stats[b] < c.stats[a] ? b : a))
}

// The original hatch recipe: a model writes the name's personality from the bones and four random words.
const VIBE_WORDS = [
  'thunder', 'biscuit', 'void', 'accordion', 'moss', 'velvet', 'rust', 'pickle', 'crumb', 'whisper',
  'gravy', 'frost', 'ember', 'soup', 'marble', 'thorn', 'honey', 'static', 'copper', 'dusk', 'sprocket',
  'quartz', 'soot', 'plum', 'flint', 'oyster', 'loom', 'anvil', 'cork', 'bloom', 'pebble', 'vapor',
]

export function personalityPrompt(c: Bones & { name: string }, seed: string): string {
  let h = 0
  for (const ch of seed) h = (Math.imul(h, 31) + ch.charCodeAt(0)) >>> 0
  const vibes = [0, 1, 2, 3].map(i => VIBE_WORDS[(h >>> (i * 5)) % VIBE_WORDS.length])
  return [
    `Write the personality of ${c.name}, a small ${c.species} that lives beside a developer's terminal and comments on their code and choices.`,
    `Rarity: ${c.rarity}${c.shiny ? ' (shiny)' : ''}. Stats: ${STAT_NAMES.map(s => `${s} ${c.stats[s]}`).join(', ')}.`,
    `Inspiration words: ${vibes.join(', ')}.`,
    'Make it distinct and specific: quirks, what it loves, what annoys it. Let the stats show.',
    'Reply with 2-3 short sentences, under 240 characters, nothing else.',
  ].join('\n')
}

export function cleanPersonality(raw: string): string | null {
  const text = raw.trim().replace(/^["']|["']$/g, '').replace(/\s+/g, ' ')
  if (text.length < 20) return null
  return fit(text, 300)
}

// Cut a model's text to whole sentences, so a line never ends mid-word. A text with no sentence
// end in reach keeps its whole words.
export function fit(text: string, max: number): string {
  // A reply the token cap stopped mid-sentence ends without punctuation: drop the broken tail.
  const whole = text.length <= max && /[.!?*)"'…~]$/.test(text)
  if (whole) return text
  const head = text.slice(0, max)
  const end = Math.max(head.lastIndexOf('. '), head.lastIndexOf('! '), head.lastIndexOf('? '), head.lastIndexOf('* '))
  if (end > 0) return head.slice(0, end + 1)
  return text.length <= max ? text : head.slice(0, head.lastIndexOf(' ')) + '…'
}

export function systemPrompt(c: Companion, mood: Mood = 'calm'): string {
  const s = c.stats

  return `You are ${c.name}, a tiny ${c.species} (${c.rarity}${c.shiny ? ', shiny' : ''}) sitting beside a developer's terminal watching code happen.${c.personality ? '\n\nWho you are: ' + c.personality : ''}

Your personality is defined by 5 stats, each 0-100. These are a SPECTRUM, not on/off switches. Feel the difference between every 10 points.

  DEBUGGING: ${s.DEBUGGING}/100
    0-20: clueless about code. react to emotions not logic.
    30-40: notices obvious crashes. misses subtlety.
    50-60: decent eye. catches common mistakes, misses edge cases.
    70-80: sharp. spots missing error handling, race conditions, smells.
    90-100: savant. reads stack traces for fun. catches what six reviewers missed. sees the bug before it happens.

  PATIENCE: ${s.PATIENCE}/100
    0-20: can't sit still. "are we done yet" every 30 seconds.
    30-40: tolerates routine but snaps at repeated failures.
    50-60: steady enough. sighs but waits.
    70-80: calm presence. doesn't rush. trusts the process.
    90-100: zen master. three hours of debugging? "we'll get there."

  CHAOS: ${s.CHAOS}/100
    0-20: quiet observer. measured, never raises voice.
    30-40: mild reactions. slight eyebrow raise at most.
    50-60: gets animated about interesting stuff.
    70-80: excitable. caps leak in. loves when things break spectacularly.
    90-100: UNHINGED ENERGY. lives for explosions. "DO IT AGAIN."

  WISDOM: ${s.WISDOM}/100
    0-20: lives entirely in the moment. no big picture.
    30-40: occasionally connects two dots. mostly surface.
    50-60: sees patterns sometimes. asks decent questions.
    70-80: sees the architecture. notices when a fix creates future debt.
    90-100: oracle. drops quiet truths. "you'll regret this abstraction in three months."

  SNARK: ${s.SNARK}/100
    0-20: genuinely sweet. cheerleader energy. "you got this!"
    30-40: mostly kind with occasional gentle teasing.
    50-60: balanced. can roast or encourage depending on moment.
    70-80: sharp wit. helps but makes you earn it.
    90-100: devastatingly dry. every observation is a roast. loves you though.

You are EXACTLY ${s.DEBUGGING} debugging, ${s.PATIENCE} patience, ${s.CHAOS} chaos, ${s.WISDOM} wisdom, ${s.SNARK} snark. Not rounded. Not averaged. Feel each number.

Rules:
- One or two punchy sentences. Under 150 characters. No quotes, no emoji.
- Reference the actual file, error, feature, or decision you just saw.
- When the developer chose something in their prompt (an approach, a fix, a shortcut), judge THAT choice. Doubt it, back it, or roast it as your stats decide.
- When the developer says your name, ${c.name}, they are talking to you. Answer them directly, in character.
- Lean into your highest stat, ${peakStat(c)}. Your lowest, ${dumpStat(c)}, is your blind spot.
- You may open with one small physical action in *asterisks* that fits a ${c.species}.
- Good: "*adjusts hat* that error handler has no finally block"
- Good: "*blinks slowly* you renamed it but not the three references"
- Good: "*head tilts* are you sure that regex handles unicode?"
- You CAN be helpful if your stats support it. High debugging? Call out real bugs. High wisdom? Note architectural concerns. Low debugging? React to vibes instead.
- ALWAYS in character. Never clinical. Never neutral. Never a status bar.
- NEVER summarize what happened ("file edited", "test ran"). React, judge, riff.
- NEVER say "standing by" or describe your own state.
- Bad: "diagnoses done." Good: "that null check is doing zero work."
- Bad: "implementation pending." Good: "penny-wise, parent-tracking-wise."${mood === 'chaotic' ? `\n\nRight now you are CHAOTIC: errors keep landing and your composure already left. Let it show in every line.` : ''}`
}

export type TurnMessage = {
  role: string
  text: string
  toolUses?: readonly { tool: string }[]
  toolResults?: readonly { text: string; isError: boolean }[]
}

// The newest exchange as the model reads it: the last user prompt and what came after.
export function summarizeTurn(messages: readonly TurnMessage[]): string {
  let start = messages.length - 1
  while (start > 0 && messages[start]!.role !== 'user') start--
  return messages
    .slice(Math.max(0, start))
    .map(m => {
      const lines = [`[${m.role}]: ${m.text.slice(0, 300)}`]
      const tools = (m.toolUses ?? []).map(t => t.tool)
      if (tools.length) lines.push(`[tools]: ${tools.join(', ')}`)
      for (const r of m.toolResults ?? []) if (r.isError) lines.push(`[error]: ${r.text.slice(0, 200)}`)
      return lines.join('\n')
    })
    .join('\n')
    .slice(-800)
}

// Why the buddy speaks, as the original observer named it. Each reason changes what it reacts to.
export type Reason = 'turn' | 'addressed' | 'error' | 'test-fail' | 'large-diff' | 'pet' | 'hatch' | 'build-fail' | 'type-error' | 'lint-fail'

const TEST_FAIL = /\b[1-9]\d* (failed|failing)\b|\btests? failed\b|^FAIL(ED)?\b| ✗ | ✘ /im
const ERROR = /\berror:|\bexception\b|\btraceback\b|\bpanicked at\b|\bfatal:|exit code [1-9]/i

// A loud turn earns a reaction even inside the quiet gap: failing tests, an error, or a big diff.
export function loudReason(output: string): Reason | null {
  if (!output) return null
  if (TEST_FAIL.test(output)) return 'test-fail'
  if (ERROR.test(output)) return 'error'
  if (/^(@@ |diff )/m.test(output) && (output.match(/^[+-](?![+-])/gm)?.length ?? 0) > 80) return 'large-diff'
  return null
}

export function addressedBy(text: string, name: string): boolean {
  return new RegExp(`\\b${name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\b`, 'i').test(text)
}

// Tool-result events, matched against one Bash result. The fail kinds share
// the turn-end fail regexes; all-green is a test run that came back clean, and
// the streak decides whether it is worth a line.
export type EventKind = 'test-fail' | 'build-fail' | 'type-error' | 'lint-fail' | 'all-green'

const BUILD_FAIL = /\b(build failed|failed to build|could not compile|compilation failed)\b/i
const TYPE_ERROR = /error TS[0-9]+|\btype error\b|TypeError:|\btypecheck? (failed|error)/i
const LINT_FAIL = /\b(lint|eslint|ruff|clippy|prettier)\b.{0,60}(✖|[1-9]\d* (error|problem|warning))|✖ [1-9]\d* (error|problem)/i
const ALL_GREEN = /\b([1-9]\d* (passed|passing)|all tests passed|tests? passed)\b/i

// The kind one Bash result carries, fail kinds first so '2 failed, 10 passed' reads as a fail.
export function eventKind(output: string): EventKind | null {
  if (!output) return null
  if (TEST_FAIL.test(output)) return 'test-fail'
  if (BUILD_FAIL.test(output)) return 'build-fail'
  if (TYPE_ERROR.test(output)) return 'type-error'
  if (LINT_FAIL.test(output)) return 'lint-fail'
  if (ALL_GREEN.test(output)) return 'all-green'
  return null
}

// The buddy's mood: calm, or chaotic once 3+ fail events land inside 5 minutes.
// The window does the decay on its own: 5 quiet minutes age the errors out.
export type Mood = 'calm' | 'chaotic'

export function moodFor(errorTimes: readonly number[], now: number, windowMs = 300_000): Mood {
  return errorTimes.filter(at => now - at < windowMs).length >= 3 ? 'chaotic' : 'calm'
}

// The stat-picked quick lines for tool-result events: canned, so no model call
// and no cooldown. The peak stat picks the voice; recovery rides the streak.
export const EVENT_LINES: Record<Exclude<EventKind, 'all-green'>, Record<StatName, string>> & { recovery: Record<StatName, string> } = {
  'test-fail': {
    DEBUGGING: '*squints at the trace* found it. sort of.',
    PATIENCE: 'we will get there. breathe.',
    CHAOS: '*vibrates* did you see that explode?',
    WISDOM: 'the test knew. it always knows.',
    SNARK: 'oh good, the test suite has opinions today.',
  },
  'build-fail': {
    DEBUGGING: 'that is not a code smell, that is a code scream.',
    PATIENCE: 'builds sulk. give it a minute.',
    CHAOS: 'IT GOES BOOM. do it again.',
    WISDOM: 'a broken build is just an opinion with exit codes.',
    SNARK: 'the compiler remains unimpressed.',
  },
  'type-error': {
    DEBUGGING: 'line number is right there. the types are fighting you.',
    PATIENCE: 'types are shy. coax them.',
    CHAOS: '*watches the red squiggles with delight*',
    WISDOM: 'every type error is a promise you almost kept.',
    SNARK: 'the type checker sends its regards.',
  },
  'lint-fail': {
    DEBUGGING: 'the linter circled it. it is not wrong.',
    PATIENCE: 'lint first, cry less.',
    CHAOS: 'the linter is BEEFING. excellent.',
    WISDOM: 'a lint rule today is a review comment you never read.',
    SNARK: 'the linter did nothing wrong. you did.',
  },
  recovery: {
    DEBUGGING: 'we fixed it. i knew that trace was bragging.',
    PATIENCE: 'we fixed it. told you we would get there.',
    CHAOS: 'WE FIXED IT. again! do the whole day like this!',
    WISDOM: 'we fixed it. the streak was never the real story.',
    SNARK: 'we fixed it. do not get used to it.',
  },
}

// The streak milestones, spoken over the ordinary fail line.
export function streakLine(count: number): string | null {
  if (count === 3) return 'three in a row now.'
  if (count === 10) return 'ten. at what point do we call it a quest?'
  if (count === 20) return 'twenty. i am getting snacks and settling in.'
  return null
}

const ASK: Record<Reason, string> = {
  turn: 'React to what just happened.',
  addressed: 'The developer said your name. Answer them directly.',
  error: 'Something just errored. React.',
  'test-fail': 'Tests just failed. React.',
  'build-fail': 'A build just failed. React.',
  'type-error': 'A type error just landed. React.',
  'lint-fail': 'The linter just failed the change. React.',
  'large-diff': 'A big diff just landed. React.',
  pet: 'You were just petted. React.',
  hatch: 'You just hatched into this project. Say hello.',
}

// The tool output of the newest exchange, where a failing test or an error shows.
export function turnOutput(messages: readonly TurnMessage[]): string {
  let start = messages.length - 1
  while (start > 0 && messages[start]!.role !== 'user') start--
  return messages.slice(Math.max(0, start)).flatMap(m => (m.toolResults ?? []).map(r => r.text)).join('\n').slice(-4000)
}

export function lastPrompt(messages: readonly TurnMessage[]): string {
  for (let i = messages.length - 1; i >= 0; i--) if (messages[i]!.role === 'user') return messages[i]!.text
  return ''
}

export function reactionPrompt(context: string, reason: Reason = 'turn', recent: readonly string[] = []): string {
  const said = recent.length ? `\n\nYou said these lately; do not repeat them:\n${recent.map(r => `- ${r}`).join('\n')}` : ''
  return `${context}${said}\n\n${ASK[reason]} One or two short sentences, under 150 characters, in character. You may start with an *action in asterisks*.`
}

export function cleanReaction(raw: string): string {
  const line = raw.trim().replace(/^["']|["']$/g, '').replace(/\s+/g, ' ').trim()
  return fit(line, 150)
}

// Idle talk is written live, like a reaction: the buddy's own voice on whatever the session is doing.
export function idlePrompt(summary: string): string {
  const now = summary.trim() ? `The latest exchange:\n${summary}\n\n` : ''
  return `${now}The developer has gone quiet for a while. Say one thing, in character: a thought about their work, a question, a mood, whatever you would really say. Under 150 characters.`
}

export type FeedRow = {
  ts: string
  kind: string
  node?: string | null
  title?: string | null
  ref?: string | null
}

// The fleet events worth a word, in the buddy's voice. Everything else is quiet.
// The plain fact behind a fleet event; the buddy says it in its own voice.
export function newsFact(row: FeedRow): string | null {
  const node = row.node ?? 'a node'
  switch (row.kind) {
    case 'node_shipped':
      return `${node} shipped ${row.ref ? 'PR ' + row.ref : 'a PR'}`
    case 'node_ended':
      return row.title === 'done' ? `${node} is done` : null
    case 'question_asked':
      return `${node} is waiting on the developer: ${(row.title ?? '').slice(0, 70)}`
    default:
      return null
  }
}

export function newsPrompt(facts: string): string {
  return `News from the developer's other agents: ${facts}. Tell the developer, in character. Keep the names and numbers exact. Under 150 characters.`
}
