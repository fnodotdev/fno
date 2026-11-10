# fno-mail

The fno mail protocol core: the envelope, the bus log, the address forms, the delivery states, the delivery-lane trait, and session ids.

A library crate with no binary. The fleet lives in `fno-agents` and `fno`. This crate names neither, so the compiler holds the wall between the protocol and the fleet.

`SPEC.md` is the wire contract. The pure pins live in `tests/conformance.rs`, and the door-level pins against the fleet binary live in `crates/fno-agents/tests/mail_conformance.rs`.
