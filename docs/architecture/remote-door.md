# The remote door: a phone reads the fleet over the tailnet

A phone on the same tailnet as your Mac can read the fleet: which agents run, their state, and what is stuck. The phone talks to a small door process on the Mac. The door forwards four read methods to the fno-agent daemon and refuses every other method. Nothing else leaves the Mac, and nothing on the phone can start, stop or message an agent.

The rulings behind this page: D2503 (Noise IK through `snow`, QR pairing, the allow list), D2513 (tailnet only, no relay) and D2524 (the same allow list). `fno backlog decisions` prints each one.

## Set it up

1. Install the door as a launchd agent: `fno agent workspace remote serve --install`. It binds `127.0.0.1:8725` and comes back after a reboot. `--port` picks another port.
2. Put the door on the tailnet once. The install prints the line: `tailscale serve --bg --tcp=8725 tcp://127.0.0.1:8725`. Tailscale keeps a `--bg` serve config across reboots, so you run it one time.
3. Pair the phone: `fno agent workspace remote pair`. It prints a QR and the same offer as a URL. Scan it within 10 minutes. The offer works one time.
4. Check it: `fno agent workspace remote status` prints the bind, the tailnet name, the paired device count and the age of the last beat.

`fno agent workspace remote devices` lists paired devices. `fno agent workspace remote revoke <id>` revokes one. A revoked key is never accepted again, even with a fresh offer: that phone makes a new key and pairs again. `serve --uninstall` removes the launchd agent.

## Where the door listens

The door binds loopback only and refuses any other address before it opens a socket. `tailscale serve --tcp` carries tailnet traffic to it. This is the house rule for remote reach, the same one the attention door follows ([attention items](attention-items.md#the-answer-endpoint)): fno never binds a LAN or public address. A phone off the tailnet cannot reach the door. That is the D2513 trade: no relay to run, and no relay to trust.

## The wire

The phone opens one TCP connection to `<tailnet-dns>:<port>`. Every message on it is a 4-byte little-endian length, then that many bytes, at most 65,535.

The handshake is Noise `Noise_IK_25519_ChaChaPoly_BLAKE2s` with the prologue `fno-remote/1`. The door is the responder. The phone is the initiator and already holds the door's static public key from the offer.

- Message 1 (phone to door) carries the phone's claim as its payload. A paired phone sends an empty payload. A new phone sends `{"pair": "<secret>", "name": "<label>"}`, where the secret is base64url without padding.
- Message 2 (door to phone) carries the verdict: `{"ok": true}`, or `{"ok": false, "reason": "..."}` and then the door closes the connection.

After the handshake, the plaintext stream is the daemon's own frame format: a 4-byte little-endian length, then that many bytes of JSON. The sender seals that stream in chunks of at most 65,519 bytes, one Noise transport message each. A frame over 16 MiB is refused. The receiver opens chunks until it holds the whole frame. A chunk that fails to open ends the session and yields nothing.

Each frame the phone sends is one daemon request, `{"id": n, "method": "...", "params": {...}}`. Each frame the door sends back is the daemon's response, unchanged: `{"id": n, "result": ...}` or `{"id": n, "error": {"code": "...", "message": "..."}}`. One session carries many requests, one at a time. A session with no request for 120 seconds closes.

## The offer

`fno-remote://<tailnet-dns>:<port>#<blob>`. The blob is base64url without padding over 72 bytes: the door's 32-byte public key, a 32-byte one-time secret, then the expiry as an 8-byte big-endian unix time. The Mac stores only the secret's SHA-256. The first successful pairing marks it used.

## The allow list

`agent.list`, `agent.watch`, `agent.status` and `agent.truth`. Any other method gets an `unknown_method` error from the door, and the door never opens the daemon socket for it. When the daemon is down, an allowed method gets an `internal` error whose message starts with `daemon_down`. The door never starts a daemon.

## The threat model

WireGuard already encrypts tailnet traffic and names the device. Noise IK adds what the tailnet login cannot give: a key per phone, held in its keychain, and a revoke that drops one phone without touching the tailnet. A tailnet peer that holds no paired key reaches the door and gets a refusal, nothing more. A lost phone is revoked by id. The door's key and the device store are mode 0600 under a 0700 directory, and the door refuses to read either at a looser mode.

## What the phone client needs

- The Tailscale app, signed in to the same tailnet as the Mac.
- A QR scanner that reads the offer URL.
- A Noise IK initiator for `25519_ChaChaPoly_BLAKE2s`: Rust `snow` through UniFFI, or a native Noise library.
- Keychain storage for its own static key and the door's public key.
- A TCP socket to `<tailnet-dns>:<port>`, and the 4-byte framing above.
- A 2-second `agent.watch` poll with `since` to draw the fleet. `agent.watch` answers at once, with `doc: null` when nothing changed.

`crates/fno-agent/tests/remote_door.rs` is a working reference client: it pairs, opens a session and reads the fleet through a real door.

## Files

| Path | What |
|---|---|
| `~/.fno/agents/remote/daemon.key` | The door's static key (0600) |
| `~/.fno/agents/remote/devices.json` | Paired devices and open offers (0600) |
| `~/.fno/agents/remote/door-status.json` | The beat: `state`, `bind`, `port`, `tailnet_dns`, `pid`, `updated_at`, `devices`, rewritten every 60 seconds |
| `~/.fno/agents/remote/door.log` | The launchd agent's log: one line per session, with the device id and why it ended |
