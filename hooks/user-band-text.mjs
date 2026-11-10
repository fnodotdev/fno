// Outside the mux, Claude Code paints the user message row on a band whose
// background comes from the theme while the text keeps the base theme's
// color: Paper reads dark on dark (#373737 band), Superscript white on
// white (#ffffff band). The mux repaints those cells (band_remap.rs); this
// module is the outside-the-mux match. It draws the row as Text in the
// band's contrast color, keyed to the pinned theme (FNO_CLAUDE_THEME).
// Without a pinned Footnote theme the engine draws as it always did.
const BANDS = {
  'footnote-paper': { bg: '#373737', text: '#f7f7f7' },
  'footnote-superscript': { bg: '#ffffff', text: '#141414' },
}

export function registerUserBandText(on) {
  on('ui.render', { component: 'UserMessage' }, async ($, e, next) => {
    if (e.surface !== 'terminal') return next(e)
    // Inside the mux the band repaint already owns this row, down to the
    // code-span accent; drawing here would flatten that accent to one ink.
    if ((await $.env.get('FNO_PANE')) || (await $.env.get('FNO_SERVER'))) {
      return next(e)
    }
    const pinned = await $.env.get('FNO_CLAUDE_THEME')
    const name = pinned ? pinned.replace(/^custom:fno:/, '') : undefined
    const band = name ? BANDS[name] : undefined
    if (!band) return next(e)
    const { Box, Text } = $.ui.resolve(e)
    return h(
      Box,
      { backgroundColor: band.bg, width: '100%' },
      h(Text, { color: band.text }, '> ' + e.props.text),
    )
  })
}

// Outside the mux, a dark-based theme on a light ground draws Claude Code's
// dark-preset reply colors: a white bullet, a pale code span, a faint turn
// line - all near-invisible on the light field. A mod subprocess has no
// controlling terminal, so the ground cannot be asked from here; it rides
// FNO_CLAUDE_GROUND=light beside the theme pin, the same keys the mux pane
// and the shell hook already set. On the mismatched pair (Footnote
// Superscript on a light ground) the reply row and the turn line redraw in
// inks that read on it. Inside the mux the repaint owns the field, and a
// dark-based theme on its own dark ground is the design; an unset or dark
// ground redraws nothing, so the engine's drawing is the default.
async function groundIsLight($) {
  return (await $.env.get('FNO_CLAUDE_GROUND')) === 'light'
}

async function pinnedTheme($) {
  const pinned = await $.env.get('FNO_CLAUDE_THEME')
  return pinned ? pinned.replace(/^custom:fno:/, '') : undefined
}

// The turn line redraws in an ink that reads on the light field. The reply
// row has no hook on this build: AssistantMessage never dispatches here
// (zero dispatches across a full debug-log session), and Claude Code reads
// no theme-file token for its bullet or code-span colors, so those two stay
// the engine's dark-preset colors on a light ground until a build exposes
// the row or honors the tokens.
let lastDoneAt = null

async function turnInk($, e, next) {
  if (e.surface !== 'terminal') return next(e)
  if (e.props.isExpanded) return next(e)
  if ((await pinnedTheme($)) !== 'footnote-superscript') return next(e)
  if (!(await groundIsLight($))) return next(e)
  const { Text } = $.ui.resolve(e)
  const seconds = Math.round(e.props.durationMs / 1000)
  let tail = ''
  if (lastDoneAt) {
    const d = new Date(lastDoneAt)
    const h12 = d.getHours() % 12 || 12
    const mm = String(d.getMinutes()).padStart(2, '0')
    const ampm = d.getHours() >= 12 ? 'PM' : 'AM'
    tail = ` · done ${h12}:${mm} ${ampm}`
  }
  return h(Text, { color: '#6b6b6b' }, `${e.props.word} for ${seconds}s${tail}`)
}

export function registerReplyInk(on) {
  on('turn.complete', async ($, e, next) => {
    lastDoneAt = await $.clock.now()
    return next(e)
  })
  on('ui.render', { component: 'TurnDuration' }, ($, e, next) =>
    turnInk($, e, next),
  )
}
