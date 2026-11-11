# Privacy

Footnote is an open-source workflow plugin. It stores project state, agent transcripts, and configuration on the user's machine. User prompts and selected project context go to the AI provider they invoke under that provider's terms. Footnote runs locally and does not operate a hosted service.

Telemetry that reaches Footnote's maintainers exists only as opt-in arms, and every arm ships off. Nothing leaves a machine before the user turns an arm on in config. An enabled arm sends identifiers and counts only: event kinds, command words, refusal codes, harness and model names, durations, exit codes, machine name. It never sends prompts, code, paths, titles or free text. `fno-agents evals events` (and the same command with `--usage`) prints the exact batch the next beat would post, token redacted, before anything goes. An arm can be turned off again at any time, and the `OTEL_SDK_DISABLED` or `DO_NOT_TRACK` environment variable forces every arm off regardless of config.

Users control local state and can remove it from their machine. Source code and support requests are handled through the public GitHub repository.
