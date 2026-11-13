# Privacy

Footnote is an open-source workflow plugin. It stores project state, agent transcripts, and configuration on the user's machine. User prompts and selected project context go to the AI provider they invoke under that provider's terms. Footnote runs locally and does not operate a hosted service.

## The daily usage report: you opt in, or nothing goes

Footnote can send its maintainers one usage report a day, and only after you opt in. Nothing leaves your machine before you say yes. Run `fno doctor telemetry show` to see the exact report the next send posts. Run `fno doctor telemetry on` to read that report and answer yes or no at a terminal. The setup wizard offers the same question, and its default is no. An agent cannot say yes for you: `on` refuses without a person at a terminal, and no config key can turn the report on.

The report holds counts and identifiers only. It names the fno version, the install channel, the OS, the CPU arch, the UTC day and a random install id. It counts event kinds, harness names and model names. It never holds a prompt, code, a path, a title, a hostname, a machine id or free text. The full field list is in `docs/telemetry.md`. Each sent report stays on your machine under the agents home at `otlp_export/usage_report/sent/` (the last 30), so you can check what went.

The report goes to PostHog, a third-party analytics service, in a project kept for these reports. Only official release builds carry the PostHog key, so a source build or a crates.io build never sends. If `CI` is set, the report never sends.

You can turn it off at any time. Run `fno doctor telemetry off`, set `DO_NOT_TRACK=1` or `OTEL_SDK_DISABLED=true`, or set `[telemetry.usage_report] enabled = false` in config. When a later release adds a field, the report stops until you opt in again.

## Maintainer fleet arms

Two more arms exist for the maintainers' own machines: eval events and usage events. Both ship off. Nothing leaves before the machine's owner turns an arm on in config and adds a token. An enabled arm sends identifiers and counts only, never prompts, code, paths, titles or free text. The usage events arm also sends the machine name, which is why it stays a fleet arm. It sends each `fno` command's path, exit code and duration. The path holds menu words only, never an argument. It also sends the name of each `fno:` skill a session invokes. The kinds and fields it sends are listed in `docs/telemetry.md`. `fno-agents evals events` (and the same command with `--usage`) prints the exact batch the next beat posts, token redacted. The `OTEL_SDK_DISABLED` or `DO_NOT_TRACK` environment variable forces every arm off regardless of config.

Users control local state and can remove it from their machine. Source code and support requests are handled through the public GitHub repository.
