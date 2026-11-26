# The activity feed

A browsable history of what happened across the fleet, in order, with a deep link per row into the session that produced it. Open it in the mux client with `e`.

## The stores, one timeline

Several stores hold one timeline and nothing joined them. The feed is one projection that joins them at read time.

| Store | Holds | Contributes |
|---|---|---|
| `~/.fno/db/events.db` | question and team lifecycle, spawn-gate refusals, stalls, distress, daemon starts, update runs - read typed through the `events_type_ts` index | `question_asked`, `question_closed`, `decision_recorded`, `day_boundary`, `team_granted`, `team_vacated`, `session_spawned`, `session_spawn_refused`, `worker_stalled`, `help_emitted`, `update_started`, `update_finished` |
| `~/.fno/spaces/<repo>/events.db` | one project's own journal, read typed per space | `mail_to_user` |
| `~/.fno/graph.db` | node lifecycle as fields: `created_at`, `sessions[].started_at`, a ship-phase row beside `pr_number`, `merge_status`, `completed_at` | `node_created`, `node_started`, `node_shipped`, `pr_merged`, `node_ended` |
| `~/.fno/agents/events.db` | the agents journal: mux close rows, spawn events, daemon starts | `session_spawned`, `session_spawn_refused`, `pane_closed`, `server_stopped`, `composer_shell_ran`, `composer_shell_refused`, `daemon_restarted` |
| `~/.fno/agents/reap-receipts/` | one durable receipt per removed registry row, each carrying the verbatim resume line | `session_reaped` |

A store qualifies for three reasons. It is durable. It does not rotate. It IS the record, not a restatement of one.

The durable db stores meet that test. `events.jsonl` rotates in about twelve hours, but its durable db copy keeps `retention_class = durable` rows long after the file rotates. The feed never reads the jsonl file: a typed read through the store skips the ticks and reaches rows from weeks back.

The feed reads the question, spawn and close kinds typed. The store's other kinds, 119k+ `attention_delivery` rows alone, never reach the parser. If the query's flags exclude every kind a leg can emit, the leg skips its read entirely.

The lifecycle kinds derive from the graph at query time, so the graph stays the one truth. No writer is added. A merged node reads `pr_merged`, not `node_ended`. The merge IS the end.

## The kinds and their areas

Every row carries an `area`: a pure function of the kind. The column renders in the panel and `--area` filters on it.

| Area | Kinds |
|---|---|
| `mail` | `question_asked`, `question_closed`, `decision_recorded`, `mail_to_user` |
| `backlog` | `node_created`, `node_started`, `node_ended` |
| `ship` | `node_shipped`, `pr_merged` |
| `agents` | `session_spawned`, `session_spawn_refused`, `session_reaped`, `worker_stalled`, `help_emitted`, `team_granted`, `team_vacated`, `agent_corrected`, `agent_corrected_agent`, `agent_corrected_machine`, `wrong_path` |
| `mux` | `pane_closed`, `server_stopped`, `composer_shell_ran`, `composer_shell_refused` |
| `fleet` | `day_boundary`, `daemon_restarted`, `update_started`, `update_finished` |
| `ci` | `main_ci_changed` |

`main_ci_changed` derives from a `main_ci_changed` event in the machine store. The writer lives in the live main-CI reading (`main_ci_reading`). Each reading compares its verdict with the newest stored row of the same repo. A flip between red and green appends one row. A pending reading appends nothing. A first reading of red appends with `previous_state: unknown`. A first reading of green appends nothing. The event carries `repo`, `sha`, `state`, `previous_state` and `failing_checks`, and it is durable. The key of the append names the row it follows, so two readers of one flip commit one row. The feed row reads `<repo> main CI red at <sha8> (<failing checks>)`.

`mail_to_user` derives from a `mail_escalation` event. That event marks agent mail that needs the user now. The row carries the sender, the recipient and the first line of the body. The event sits in the store of the project that sent the mail, so the feed reads each project space's store typed. The row's `cwd` is that space's repository path. The escalation runs under a per-pair debounce, so mail that lands without an escalation leaves no row. Every reason projects, a `question` mail included, because a mail question never becomes a `question_asked` row.

`worker_stalled` derives from a `worker_silent` event (the handle and its silent age). `help_emitted` derives from a `blocked` event (the distress kind and its reason). A spawn-gate refusal keeps the `session_spawn_refused` kind and carries the gate axis as its `reason`. The update trio folds into `update_started` and `update_finished`.

## Paging, and the panel's one read

The projection orders rows by a total key: parsed time, then kind, node, session, ref and title. Every row carries a `cursor` - that key as one opaque string. The client never parses anything else to page.

`fno agent fleet feed --json --limit 200` reads the newest page. `--before <cursor>` reads the page strictly below a cursor. `--after <cursor>` reads the page strictly above one. Rows sharing a timestamp still page cleanly. Pages concatenate with no row read twice or skipped.

The cursor pages the command. The panel does not page: each fold reads the whole history in one run (`--limit 10000000`) and replaces the rows it holds. Sort, filter and group then read every row, never a loaded page. A fold runs off the UI loop, and the last good rows stay on screen while it runs.

## The command and the panel

`fno agent fleet feed [--since-epoch <secs>] [--until-epoch <secs>] [--limit <n>] [--before <cursor>] [--after <cursor>] [--node <ids>] [--session <id>] [--kind <prefix>] [--area <names>] [--agent <name>] [--harness <names>] [--lead <name>] [--json]` is the projection. A missing or unreadable store is not fatal. The rows the other stores yielded still emit, with one stderr line naming the store skipped.

The filter flags AND together, and the values inside one flag are comma-OR. `--kind` matches by prefix, so an exact kind is its own prefix. An empty flag value is ignored. An unknown area returns an empty answer, not an error. Both epoch bounds reach every event-store leg as real store bounds.

The live panel holds no history. Opening it starts `fno agent fleet feed --serve`, a held reader that holds the whole history's rows and reads them again every 15 seconds. Each read runs in a child process, so the projection's working set is the child's and leaves with it. When the new rows start to stream, the reader drops its old rows. Two copies never coexist. An answer that lands in that parse waits on a lock and then reads the new rows. The panel sends one JSON request per line: the view, the query, and the window it paints. The reader answers one JSON line: the slot list, the rows of that window, and the value lists a filter picker reads. The grammar, the row and the table live once, in `fno_proto::feed`, and both sides run that copy. The panel keeps the rows within 600 slots of its top row. Closing the panel closes the reader's stdin, and the reader exits. A shot or a test holds its own rows and evaluates them in process.

`e` in the mux client toggles the full-height panel on the right edge. Rows render newest first as a table: time, area, harness, kind, node, session tail, lead, summary. Time, kind and summary never drop, and the summary keeps at least 12 columns. A short panel gives width in one order. Kind narrows to 6 columns with an ellipsis. Then the summary floor drops to 10, which keeps node. Then node goes. A wide panel dates every row older than today. At any width, the first row of each day shows its date, so a day boundary never reads as time running backward. A narrow panel shows that date in place of the time, so the time column keeps its width and node keeps its place. Past that, the panel adds node, then lead, harness, area and session as width allows. The border drags to a width that persists.

A refresh never moves the reader. The row under the cursor and the top row stay where they were. Rows newer than the newest one held count while you sit away from the top, and the footer shows `↑ N new`. A click on that marker, or `g`, returns you to the top and clears it.

The title row counts the rows: `activity feed · 3`, or `12 of 340` under a query or a filter. Focused, it also names the sort, the group and the filter count. The filter count stays down to a narrow panel, so a filter on a hidden column is still named. The bottom row carries the `↑ N new` marker, the active query, the keys, and the typed error with `r retry`.

### Keys

Focus with `E`. An unfocused panel takes no keys at all.

| Key | Does |
|---|---|
| Up, Down | select a row |
| Left, Right | pan the summary in display columns |
| PageUp, PageDown | select a viewport at a time |
| `g` | jump to the first slot and clear the `↑ N new` marker |
| `G` | jump to the last slot |
| `o` | open the sort picker |
| `W` | open the group picker |
| `f` | open the filter picker |
| `x` | clear every column filter; the query stays |
| `z` | fold every group, or unfold them all when all are folded |
| `r` | read the history again now |
| `?` | toggle the keys overlay |
| `/` | open the search bar |
| Enter | open the row's provenance; on a group header, fold it |
| Space | fold or unfold the group header under the cursor |
| Esc | close the picker or overlay in front, else close the panel |

`?` toggles the `feed keys` overlay. The panel's keys, as it renders them:

- `up/down row or group - enter details or fold`
- `o sort - W group - f filter - x clear filters`
- `z fold all - r retry`
- `g first - G last - arrows pan`
- `/ search - ? keys - esc close`

## Sort, filter and group

The panel is a table over every row it holds. One view decides what shows: one sort column, a filter per column, one grouping, and the folded groups. The view saves to the mux view store as `feed_view`, by name, so it survives a restart. A name a later build drops reads as that one choice's default. A client with no saved view reads the old `feed_order` once: `recent` opens ungrouped.

Sort. `o` lists all eight columns, hidden ones too. The active one wears its arrow. Picking it again flips the direction. A new column starts newest first for time and A to Z for the rest. An empty value sorts last in both directions. Ties fall to newest first. A click on a column title sorts by it the same way.

Filter. `f` lists the columns, with a dot and a count on each one that filters. Enter opens that column's values with their row counts, over the rows the query and the other filters leave. Typing narrows the list, case-blind. Space ticks a value. Enter makes the ticks the column's filter. With nothing ticked and no tick changed, it takes the value under the cursor. After a change, Enter applies the ticks exactly, so unticking every value clears that column. Values inside one column OR. Columns AND, and they AND with the query. A filtered column's title wears a dot and its value count, so color is never the only signal. The time column filters by local day, and the session column by the whole id.

Group. `W` lists `none`, `lead or epic` (the default) and the eight columns. `lead or epic` puts the teams band first, then one group per owner by its newest row, then `other`. A column groups by its value. Rows with no value group last, under `no <column>`, for example `no harness`. Picking the active grouping again flips the group order. A header shows a caret and its row count. Enter, Space or a click folds it, and `z` folds them all.

### Search

`/` opens a query bar on the footer line. The bar speaks the shared search grammar - the one parser the feed, the mux backlog search and the web board share. Space ANDs, comma ORs inside a key, `|` ORs across terms, and a leading `-` negates. A term the grammar cannot read shows the refusal in the footer verbatim and leaves the rows as they were.

The query keys the feed answers, as the `?` overlay renders them:

- `id: n: - value prefix`
- `session: sid: - value prefix`
- `spawner: by: - value prefix`
- `agent: a: - value prefix`
- `actor: - value prefix`
- `pr: - value prefix`
- `project: proj: - value prefix`
- `epic: e: - value prefix`
- `in: - value prefix`
- `lead: l: - value prefix`
- `area: ar: - value prefix`
- `harness: h: - value prefix`
- `model: m: - value prefix`
- `effort: ef: - value prefix`
- `phase: ph: - value prefix`
- `kind: k: - value prefix`
- `reason: - text`
- `ts: at: - date`
- `is: - exact word`
- `has: - exact word`
- `age: - age`
- `title: - text`
- `details: body: - text`

Worked examples: `n-1234` (a bare node id), `h:codex k:node stall`, `sid:00bde302`, `-k:question h:claude`, `h:codex | h:claude k:pr`, `ts:>=2026-10-01`. `s:ready` refuses here: `s:` is node-only.

The query matches in the client, over every row the panel holds. It reads nothing: a settled query only re-slots the rows. The projection's flags stay for the command line.

Tab completes the token before the cursor. A key prefix completes from the shared table's keys. A value after `id:`, `sid:`, `a:`, `h:`, `k:` or `l:` completes from the distinct values the panel holds. Esc in the bar clears the query and shows every row again. The panel stays open.

## Day boundaries

`fno inbox day start` and `fno inbox day end` fold the existing project journal, question lifecycle, graph completion records, decision retractions, review retractions, and lead check-ins. The native fold returns JSON or a short text readback. The native command's `--commit` writes one bounded `day_boundary` row to the project journal first. It then writes the same row to `~/.fno/questions.jsonl` for durable cross-rotation recall. The inbox relay only selects the destination, so the operator command lives under the inbox. The row carries ids and counts only. The writer sizes the row against the validated event limit before any write. If the row is over the limit, the writer refuses it and never substitutes.

The permanent question index stores the boundary reference because the project journal rotates at 8 MiB and keeps only one rotated file. The index is recall and provenance, not a second source of question truth. Open questions still come from the existing lifecycle fold, and a failed index append names the boundary id after the project append. The boundary id is stable per kind and local day. A retry after that failure recognizes its own journal row and appends only the index leg, so no orphan row accumulates.

Each boundary uses a half-open interval `[from, to)`. The first boundary starts at local midnight. Later boundaries start at the newest saved cutoff. Repeated `start` or `end` on the same local day returns the saved boundary and does not append a duplicate or advance attention. Up to five question ids are featured. Three come from the shared queue order. The rest are questions that have not appeared in an earlier saved boundary.

An unreadable question store exits 1 and prints no zero-open line. A missing question store is a successful, explicitly incomplete read. The first line says `open questions: unknown` and the receipt names the missing path. A saved boundary is historical evidence. It does not replace current queue or merge authority.

## An actor is not a session

The questions store puts a MECHANISM in the field a session goes in. `operator_decision.decided_by` is the literal string `fno agent audit stale-escalate` on every row. `operator_question_closed.closed_by` is `stale-escalate` on most rows. Projecting those into `session_id` made the panel offer an attach the server answers with `no such agent`.

So the SHAPE decides. A value that reads as an fno session id (`20260904T151442Z-cl54345-58af0c`) becomes `session_id`. So does a harness uuid. Every other value becomes `actor`, which is provenance and never an attach target.

A closure and a decision carry only their `question_id`. The asking row is the one place their node association exists, so the projection indexes the asking rows and fills `node` from them.

It does NOT borrow the asking row's session. That session asked the question. It did not close it. A borrowed session in `session_id` is a guess wearing the clothes of provenance.

## The harness and the lead

A row with a session and no harness reads the lane its session ran. The projection builds one session-to-harness map from the graph's `sessions[]` rows and the spawn events. It joins on the session id. A session in neither source stays absent. No guess.

A row that rolls up to a held team scope carries `lead`: the role holder's name, beside the `owner` spelling the panel groups on. A row with no node whose parent session IS a held role holder's session rolls up to that holder.

## A click opens provenance, not a deep link

A click on a row opens that row's PROVENANCE view. It shows the nine facts the operator asked for, in their order, each saying how it is known.

Three of those nine are measured to be mostly unrecorded. So the view separates three silences and never leaves a cell blank:

| Marker | Means |
|---|---|
| `NOT RECORDED` | the source lacks the fact |
| `NOT APPLICABLE` | positive evidence the concept does not apply, as for a row that records no session at all |
| a named live state | a reading, not an absence: `not in the live roster`, `no seat · retarget portal 0`, the session was removed |

Which silence a cell shows is read from the ROW, never from its kind. A `node_ended` on a node that ran carries the last do or ship session, so its blank lane is `NOT RECORDED`. A `node_ended` on a node nothing ever ran carries no session, and only then is the lane inapplicable.

Pane, parent and lead are a live lookup against the roster, joined on the exact `harness_session_id`. Never on the row NAME. A later worker can reuse a name, and the view then answers about a different session.

The deep link is this view's footer ACTION, not the gesture that opened it. Inspecting attaches and resumes nothing on its own.

Enter resolves from the same evidence the footer named. For a row seated in a pane it sends `FocusPane`. Pane ids allocate from zero, so pane 0 is a real seat and the check is an equality against the `Option`. For a live paneless row it sends `AttachAgent` on portal 0. For a `session_reaped` row it hands over the receipt's verbatim resume line. A removal is a normal outcome with a recovery path, not an error.

For a `node_created` row with a node id, the footer offers `b: blueprint`. Pressing `b` opens the node-bound launch composer with `/fno:blueprint <id>`. When the row has a recorded cwd, the composer selects that project. If it is absent, choose the project in the composer. The harness, model and placement choices remain available, and the composer shows the normal launch receipt. Other row kinds and id-less creation rows do not offer this action.

## Open questions in the sideline

Open questions also show as a block in the sideline, pinned above the org block. The block reads `fno-agent needs --items`, not the feed: the feed shows a question's history, and the block shows only what is open. An answer picked in the block's overlay records `sink: mux`, and the row shows the delivery rung for 15 minutes. See [attention-items](attention-items.md) for the delivery ladder.

## OTLP export

The feed is a local view. When you opt in, it can also feed an OpenTelemetry collector. The exporter is a daemon arm. Every five minutes it collects the feed rows of the last 24 hours and posts them as one OTLP/HTTP JSON logs batch. A batch is oldest first, at most 1000 rows.

```toml
[telemetry.otlp]
enabled = true
endpoint = "https://collector.example.com"  # /v1/logs is appended when absent
log_content = false                         # titles and free text stay local
```

Auth headers come from an env var, never from the config file: `FNO_OTLP_HEADERS`, comma-separated `name=value` pairs. An unset or empty variable refuses the export rather than shipping an unauthenticated payload. The variable name itself is overridable with `headers_env`.

With `forward_requests = true`, the same beat also forwards the masked `api_request` records from otel.db. Each record carries cost, tokens, model and skill/plugin/agent names, under the `fno.api_requests` scope. The key defaults to false. Unset means those records never leave the machine. See [cost accuracy](cost-accuracy.md).

The export is off unless `enabled = true`. Two env kill switches outrank the config: `OTEL_SDK_DISABLED` or `DO_NOT_TRACK` set to `true` or `1` force it off. The daemon reads its own environment at start. Set kill switches and the headers var where the daemon sees them. Restart the daemon after a config change.

One row becomes one log record. Always-on attributes: `fno.kind`, `fno.area`, `session.id`, `event.name`, and the stable `fno.row.id`. The row id is a sha256 over kind, timestamp, node, session, ref, team, holder and name. The title is excluded, so a title edit does not resend the row. Conditional attributes: `fno.node.id`, `fno.harness`, `fno.model`, `fno.effort`, `fno.phase`, `fno.team`, `fno.actor`, `fno.parent.session.id`, `fno.ref`. With `log_content = true` the record also carries free text: `fno.title`, `fno.cwd`, `fno.reason`, `fno.detail`, `fno.url`, `fno.name`, `fno.holder`, `fno.owner`, `fno.lead`.

Delivery is at-least-once keyed on `fno.row.id`. Deduplicate on that attribute at the backend. A 2xx marks the batch delivered. A 4xx other than 401, 403, 408 and 429 drops the batch, because the endpoint said never. Anything else holds it for the next beat, and the held error shows in the status line. Undelivered ids expire after 48 hours.

State and receipts live under `~/.fno/agents/otlp_export/`: `state.json` holds the delivery memory, `receipts.jsonl` holds one line per post attempt.

`fno agent fleet feed --otlp` previews, never sends: it prints the batch the next beat will post to stdout, and the resolved posture to stderr. It combines with the filter flags and refuses `--json`.

One risk to know: the OTLP spec does not require a server to accept the JSON encoding over HTTP. Most backends do. If yours does not, point `endpoint` at a local Collector and let it convert and forward.

## Deploy rule

The feed reads the durable stores and the graph for lifecycle. No writer is added.

When you want to surface a new operator-facing event, add it to a store that passes the test above. A typed row in db/events.db or the agents journal qualifies - give the type a required data shape and the projection a derived kind. So does a stamped graph field beside the row that produced it, and so does a durable receipt store. Never emit it into events.jsonl expecting the feed to mine the rotating file.

## Measuring the panel on a real store

The panel holds the whole history in the client. Read its memory and its reslot time on a real store, never on a fixture. Read them on a private copy. Take the copy with `sqlite3 .backup`, and delete it after the read. A binary built in a feature worktree refuses the live store.

The client logs one line per reslot to `$FNO_MUX_DIR/client.log`: `feed reslot rows=<n> slots=<n> ms=<n>`. A row count above 0 tells you the feed landed. Read the client's RSS with `ps -o rss=` before the feed key, at its peak, and 30 seconds after the line lands. Read the held reader the same way, and read its footprint beside the RSS with `footprint -p <pid>`. On macOS the footprint is the true figure. libmalloc keeps freed pages resident. So `ps` RSS climbs about 7 MB per refresh, while `heap` shows the same malloc node count and the footprint stays flat.

Drive the client in a pty that you own. macOS `script` fails under a harness with `tcgetattr/ioctl: Operation not supported on socket`, so open the pty from code. A runner needs four things, or the read is wrong:

- Unset every harness identity variable before the server starts, as `scripts/ops/mux-demo-snapshot.sh` does, and set `FNO_OWNER_PID` to the runner's pid. With a session id still set, the server sees its sandbox owner gone and shuts down after the first read.
- Set the pty size before the client starts. Set it on the child side before exec, for example 100x40.
- Answer the terminal queries. The client waits for a reply to each query in this table. Without the replies the client paints only its splash. Then `client.log` reads `error attach-no-paint` after 5 seconds.

| Query | Sent as | Reply |
|---|---|---|
| DA1 | `ESC [ c` | `ESC [ ? 62 ; 22 c` |
| DSR | `ESC [ 5 n` | `ESC [ 0 n` |
| cell size | `ESC [ 16 t` | `ESC [ 6 ; 16 ; 8 t` |
| OSC 11 background | `ESC ] 11 ; ? ST` | `ESC ] 11 ; rgb:1414/1414/1414 ST` |

The client also sends a kitty graphics query. It needs no reply. Spaces in the table are for reading only. Send the bytes with no spaces.
- Open the feed with Ctrl-b (0x02) then `e`.

Take a read only with no shot rig running on the machine and the 1-minute load under 8. Throw out a read whose window spans a pause of another process. Kill only the pids the runner started, by pid.
