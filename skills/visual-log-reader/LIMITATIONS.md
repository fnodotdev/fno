# Visual Log tap reader limitations

## Known Limitations and Deferred Work

- The probe reads one machine's registry. A reader row on another machine is invisible to a check that runs here.
- Freshness rides the registry's served last-activity age. A live reader idle past the 20 minute bound reads stale, and the repair is resume, never respawn.
- The beat still runs inside a Claude session, because only a session holds the artifact db tools. Nothing here moves that reading to Rust.
- The fleet machinery is per-machine. Taps written while every machine is off sit on the page until a machine with the reader returns.
- A lead review board the reader's account cannot read is reported each beat and never noted. The lead's own beat read (D3482) is then its only reader. One lead's board read `not found` to this account on 2026-11-24, a cross-org refusal. A board whose lead never posts its `Review board:` note is not read either.
