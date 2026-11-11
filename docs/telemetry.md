# The daily usage report

fno can send its maintainers one usage report a day. It sends nothing until you opt in. This page lists every field the report holds, the gates that hold it, and the files it keeps. PRIVACY.md holds the short version.

## Commands

| Command | What it does |
|---|---|
| `fno doctor telemetry status` | Prints whether the report is on, and when it is off, the first gate that holds it. |
| `fno doctor telemetry show` | Prints the exact JSON body the next send posts. It sends nothing. |
| `fno doctor telemetry on` | Prints the report, then asks `Send this report once a day? [y/N]`. Only `y` or `yes` turns it on. It refuses with exit 2 when stdin or stderr is not a terminal, or when `CI` is set. |
| `fno doctor telemetry off` | Records no. It works from any context, a script included. |

The setup wizard asks the same question. Its default is no, and `fno config setup run --yes` never answers yes. A yes in the wizard records nothing: it names `fno doctor telemetry on` as your next step, so you see the report before you answer.

## What the report holds

Schema 1. One `fno_usage_report` event per UTC day, for the last complete day.

| Field | Example | Meaning |
|---|---|---|
| `schema` | `1` | The report's schema number. |
| `day` | `2026-11-10` | The UTC day the counts cover. |
| `install_id` | `0123456789abcdef0123456789abcdef` | 16 random bytes as hex, minted once per machine. It is not derived from anything about you. |
| `fno_version` | `0.4.9` | The fno-agents version. |
| `channel` | `binary` | How fno was installed, read from `~/.fno/install/channel`. |
| `os`, `arch` | `macos`, `aarch64` | Rust's `std::env::consts::OS` and `ARCH`. |
| `events` | `{"agent_spawned": 12}` | Counts of journal rows by kind, for the kinds the usage arm allowlists in `crates/fno-agents/src/eval/usage_events.rs`. |
| `harness` | `{"claude": 9, "codex": 3}` | Counts of those rows by harness name. |
| `model` | `{"claude-opus-5-5": 9}` | Counts of those rows by model name. |

If a harness or model value reads as one identifier, it counts. An identifier holds letters, digits and `-_.:/`, at most 128 characters, with no leading `/` or `~` and no `..`. Anything else is dropped. The report never holds a prompt, code, a path, a title, a hostname, a machine id or free text. The event also carries PostHog's `$process_person_profile: false`, so PostHog builds no person profile.

An example body, as `show` prints it:

```json
{"api_key":"phc_...","batch":[{"distinct_id":"install:0123456789abcdef0123456789abcdef","event":"fno_usage_report","properties":{"$process_person_profile":false,"arch":"aarch64","channel":"binary","day":"2026-11-10","events":{"agent_spawned":12},"fno_version":"0.4.9","harness":{"claude":9,"codex":3},"install_id":"0123456789abcdef0123456789abcdef","model":{"claude-opus-5-5":9},"os":"macos","schema":1},"timestamp":"2026-11-10T23:59:59Z","uuid":"..."}]}
```

## The gates

The send checks these in order. The first one that holds stops it, and `status` names it.

1. `DO_NOT_TRACK` or `OTEL_SDK_DISABLED` is `1` or `true`.
2. `CI` is set to any nonempty value.
3. `[telemetry.usage_report] enabled = false` is in config. This key can only force the report off. No key turns it on.
4. No answer is recorded, or the answer is no.
5. The recorded yes was for an older schema. A new field never rides an old yes.
6. The build carries no key. Only the release workflow embeds `FNO_POSTHOG_PUBLIC_KEY`, so source and crates.io builds never send.

## Files

All under the agents home (`~/.fno/agents` by default), in `otlp_export/usage_report/`:

- `consent.json`: `{answer, at, schema}`. Only `fno doctor telemetry on` and `off` write it.
- `sent/<day>.json`: the exact body each send posted. The last 30 stay.

The install id lives at `~/.fno/install/fno_install_seen.id`. The daemon checks once an hour. If the last complete day has no `sent/` file, it sends.
