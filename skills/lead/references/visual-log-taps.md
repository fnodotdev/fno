# Visual Log taps

Each project's Visual Log page carries an Approve and a Change button on every entry. A tap writes a row to the page's database. The lead reads new rows every 2 minutes, records each one, mails the PR's worker and lead, and writes an ack row back. The page then shows Recorded, Stale or Not recorded under the buttons. Before the ack it shows Waiting.

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

## The reader, every 2 minutes

Arm it once per lead session: `CronCreate` with `*/2 * * * *`, or `/loop 2m` on this procedure. For each page:

1. Read `taps`, `acks` and `config/approvers` (`db_list`, `db_get`).
2. Pick each tap row with no ack, or an ack older than the tap.
3. Check the viewer. The tap doc id is the viewer id. An id not in `config/approvers` gets ack `refused`. Name the viewer to the user once in chat, with the name `db_profiles` gives. When the user says it is their account, add the id to `config/approvers` with `db_write`. The next beat records the tap.
4. Read the PR head: `fno do pr info <pr> --repo <repo>`. A head that differs from the row's head gets ack `stale` with detail `the PR is now at <head 10>`. Record nothing.
5. Record the tap:
   - Approve: run the decide command below. It writes the same record a chat approval makes. The merge gate reads a role row at the PR's node. The rationale must say the user tapped Approve on the Visual Log. The text must name the head.
   - Change: record nothing in the decision store. The look stays unapproved, so the gate keeps holding.

   ```bash
   fno inbox decide <node> "visual approval PR <pr> at <head>: approved" --authority role \
     --rationale "User tapped Approve on the Visual Log (viewer <viewer id>)"
   ```

   A refused decide gets ack `refused`. Its detail is the refusal's first line.
6. Tell the worker and the lead with `fno backlog note <node> "Visual Log: <Approve|Change> at PR <pr> head <head 10>. <note>"`. The note mails the node's live holder and the epic's lead. When it exits 3 (no live holder), mail the PR author's lead by name.
7. Write the ack: `db_write` a merge on `acks/<viewer id>` that sets `rows.<key>` to `{state: "recorded", detail: "", at: <now ISO>}`.

A beat with no new tap writes nothing and mails nobody.
