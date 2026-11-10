import { registerMailPane } from './mail-pane.mjs'
import { registerUserBandText, registerReplyInk } from './user-band-text.mjs'

export function register(on) {
  registerMailPane(on)
  registerUserBandText(on)
  registerReplyInk(on)
}
