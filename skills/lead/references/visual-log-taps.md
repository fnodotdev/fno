# Visual Log taps

Each project's Visual Log page carries an Approve and a Change button on every entry. A tap writes a row to the page's database. The fleet's reader session reads new rows every 15 minutes, records each one, mails the PR's worker and lead, and writes an ack row back. The page then shows Recorded, Stale or Not recorded under the buttons. Before the ack it shows Waiting. The reader is fleet-owned: a liveness probe and its repairs live in skills/visual-log-reader/ (SKILL.md and scripts/ensure-reader.sh).

The page template is [../assets/visual-log-v3.html](../assets/visual-log-v3.html). The visual-log law (`visual-log:per-project`) names each project's current page.

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

## Add an entry

Prepend one object to the `LOG` array with `edit_version` (replace `const LOG = [\n` with the array opener plus the new line). `head` is the full sha of the PR head the shots show. Upload the shots with `publish_files` in patch mode first and cite them by relative path. Newest PR first. A push to the PR makes a new head. Add a new entry for it and keep the old one. Taps on the old entry read Stale.

## The reader

The reading loop is fleet-owned, not a lead duty. One spawned session runs it. The beat procedure and the repairs live in [skills/visual-log-reader/SKILL.md](../../visual-log-reader/SKILL.md). The probe is `ensure-reader.sh` beside it. Never arm a per-session reader: two readers double-record approvals, and a lead's cron dies with the lead.

## Before you rule a look

Read what the user already said about this surface. Run `fno backlog find --notes "Visual Log: Change"`. It prints every Change note, newest first, in the user's own words, with the node and the date.

Pick the notes that name the same surface as the PR under look. Write the ruling with one row per complaint. Quote the complaint verbatim, then give a verdict at the PR head and cite the shot that shows it. A table with a "User complaint" column and a "Verdict" column is the shape.

A ruling that lists no complaints is incomplete. Do not post the entry for it. When no Change note names the surface, say so in the ruling and name `docs/architecture/mux-design-system.md` as the rulebook you used.
