# Visual Log taps

Each project's Visual Log page carries an Approve and a Change button on every entry. A tap writes a row to the page's database. The fleet's reader session reads new rows every 15 minutes, records each one, mails the PR's worker and lead, and writes an ack row back. It reads every registered lead review board the same way (see Review boards below). The page then shows Recorded, Stale or Not recorded under the buttons. Before the ack it shows Waiting. The reader is fleet-owned: a liveness probe and its repairs live in skills/visual-log-reader/ (SKILL.md and scripts/ensure-reader.sh).

The page template is [../assets/visual-log-v3.html](../assets/visual-log-v3.html). The visual-log decision (`visual-log:per-project`) names each project's current page.

## Why a new page

The claude.ai server declares a database only on a page that one account alone wrote. A page another account has edited can never gain one. So a new log page starts from the template, gets its capabilities declared before anyone else writes to it, and only then takes entries. The declaration survives later republishes and writes by other accounts.

Declare this, then seed `config/approvers` with the owner's viewer id (`db_whoami`):

```json
{"db": {"rules": [
  {"path": "taps", "read": "view", "write": "admin"},
  {"path": "taps/{self}", "read": "view", "write": "interact"},
  {"path": "acks", "read": "view", "write": "admin"},
  {"path": "config", "read": "admin", "write": "owner"}]},
 "user": {"scopes": ["profile", "email"]}}
```

The owner shares the page with the user's organization at Contributor level. A Viewer cannot write, so a Viewer's tap fails with "Not saved". Under the `{self}` rule a viewer writes only the tap doc that carries their own id. So the doc id proves who tapped.

## Data

- `taps/<viewer id>`: `{rows: {"<pr>-<head 12>": {pr, repo, node, head, verdict: "approve" | "change", note, at}}}`. The page writes it.
- `acks/<viewer id>`: `{rows: {"<pr>-<head 12>": {state: "recorded" | "stale" | "refused", detail, at}}}`. The reader writes it. An ack older than its tap does not show. A new tap reads Waiting again.
- `config/approvers`: `{ids: [...]}`. The viewer ids of the user's accounts. Only the owner writes it. A guest account shows no email, so the list holds ids, not emails.
- `verdicts/<entry id>` (a lead's review board, not a v3 page): `{at, by, entry, head, node, pr, verdict, note, reader_ack, lead_ack}`. The board page writes it. The reader writes `reader_ack` as `{state: "recorded" | "stale" | "refused", detail, at}`. The lead writes `lead_ack`.

## Review boards

A lead with a review board (D3482) registers it once with `fno backlog note <epic> "Review board: <slug>"`. If the board moves to a new slug, post it again. The reader runs `fno backlog find --notes "Review board:"` each beat, reads each slug's `verdicts`, and turns a new row into a `Board verdict:` note on the row's node. That note mails the node's holder and the epic's lead. A board the reader's account cannot read is reported by slug and never noted, so your own beat read is its only reader. Read the verdict notes back with `fno backlog find --notes "Board verdict:"`.

## Add an entry

An entry asks the user for a tap. Probe the reader first: `bash skills/visual-log-reader/scripts/ensure-reader.sh check`. Exit 1 means the reader is down and a tap lands nowhere. Do not post the entry. Run the repair the probe names, then probe again. The merge gate's look request runs the same probe. If the reader is down, the request says so.

Prepend one object to the `LOG` array with `edit_version` (replace `const LOG = [\n` with the array opener plus the new line). `head` is the full sha of the PR head the shots show. Publish in three steps. First, stage: run `mkdir -p .fno/visual-log`, then copy the shots there, inside your own checkout (git ignores `.fno`). The artifact client refuses a path under `$CLAUDE_JOB_DIR/tmp` (your job tmp dir) or under the vault root. The refusal reads "only files this session is allowed to read". A path inside your own checkout needs no extra grant. Second, upload the staged copies with `publish_files` in patch mode and cite them by relative path. Third, keep the head-keyed vault copies: `fno agent internal target visual shot-save <node> <png...>` puts each shot at `{vault}/mux/<node>-<head12>-<view>-<role>-<theme>.png`. Never ask the user to /add-dir a job tmp dir or a vault path. Only the user answers prompts, and a remote user cannot run /add-dir. A refusal means stage again inside your checkout. The next round reads its before pair from the vault at the base head instead of regenerating it. Newest PR first. A push to the PR makes a new head. Add a new entry for it and keep the old one. Taps on the old entry read Stale.

## The reader

The reading loop is fleet-owned, not a lead duty. One spawned session runs it. The beat procedure and the repairs live in [skills/visual-log-reader/SKILL.md](../../visual-log-reader/SKILL.md). The probe is `ensure-reader.sh` beside it. Never arm a per-session reader: two readers double-record approvals, and a lead's cron dies with the lead.

## Before you rule a look

Read what the user already said about this surface. Run `fno backlog find --notes "Visual Log: Change"`. It prints every Change note, newest first, in the user's own words, with the node and the date.

Pick the notes that name the same surface as the PR under look. Write the ruling with one row per complaint. Quote the complaint verbatim, then give a verdict at the PR head and cite the shot that shows it. A table with a "User complaint" column and a "Verdict" column is the shape.

A ruling that lists no complaints is incomplete. Do not post the entry for it. When no Change note names the surface, say so in the ruling and name `docs/architecture/mux-design-system.md` as the rulebook you used.

Check platform convention before ruling a literal build through. The user's words name the outcome they want, not the mechanism. When the literal mechanism fights a platform convention (menu accelerators, keyboard navigation, standard gestures), name the conflict and the convention in the ruling. Do not pass the literal build. The specimen class: a literal "prefix + ?" ask built as Cmd+1 through Cmd+4 bindings, against macOS convention.
