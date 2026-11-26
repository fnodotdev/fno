"""Hook output strings must prescribe only fno commands that resolve.

The context-nudge Stop leg (`crates/fno-agent/src/runtime/hook/front/stop.rs`)
fires its block reasons into a live session under context pressure, so an agent reads them and
acts without a chance to check. Every `fno` command those strings name must
resolve against the checked-in command surface (`scripts/ci/command-baseline.txt`).
A hook string is the highest-cost instance of the prescribed-command class: a dead
command or a wrong-layer transport here is acted on blind.

Backstory: the hook once prescribed `fno agent role --succeed` (a deleted
succession shape) and `fno-agent mail-inject` (the daemon transport, not the
agent front door). Both were caught by a human noticing a worker echo them back,
not by any gate. The live succession form is `fno agent spawn --promote <scope>`;
the restored `fno agent role` command is human-attended promotion and must never
be prescribed to a running agent. The review front door is
`fno agent mail send '<command>' --to-self --raw`.

Scope is the `fno` surface that `command-baseline.txt` covers. `bash <script>` and
harness builtins like `/compact` are separate surfaces (a foreign-command allowlist
is wave 2) and are not asserted here.
"""
from __future__ import annotations

import re
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
HOOK = REPO_ROOT / "crates" / "fno-agent" / "src" / "runtime" / "hook" / "front" / "stop.rs"
BASELINE = REPO_ROOT / "scripts" / "ci" / "command-baseline.txt"

# Every emitted (user-facing) string is a Rust string literal: a line that
# opens with one (rustfmt puts a long format! literal on its own line), a
# format! call, or a push_str. Comments never open with a quote.
_OUTPUT_VAR = re.compile(r'^\s*(?:"|.*\bformat!\(\s*"|.*\bpush_str\(")', re.MULTILINE)
_FNO = re.compile(r"\bfno\b\s+")
_WORD = re.compile(r"[A-Za-z][\w-]*")


def _baseline_leaves() -> set[str]:
    leaves: set[str] = set()
    for line in BASELINE.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        toks = [t for t in line.split() if not t.startswith("!")]
        if toks:
            leaves.add(" ".join(toks))
    return leaves


def _output_lines(hook_text: str) -> list[str]:
    return [ln for ln in hook_text.splitlines() if _OUTPUT_VAR.match(ln)]


def _extract_fno_commands(line: str, leaves: set[str], first_words: set[str]) -> list[str]:
    """Resolve each `fno ...` in a line to its longest baseline-leaf prefix.

    Captures word tokens after `fno` until a non-command token (flag, quote,
    placeholder, path) and resolves to the longest prefix that is a real leaf,
    so prose like 'fno agent mail send is the door' resolves to 'mail send' rather
    than over-capturing the trailing words.

    A mention whose first token is not any leaf's first word is prose about
    the binary itself ('no fno on PATH'), not a prescribed command - skipped
    rather than flagged, since a leading-word check is enough to tell "fno" as
    a CLI name apart from "fno" as the sentence's subject.
    """
    resolved: list[str] = []
    for m in _FNO.finditer(line):
        pos = m.end()
        tokens: list[str] = []
        while pos < len(line) and len(tokens) < 3:
            wm = _WORD.match(line, pos)
            if not wm:
                break
            tokens.append(wm.group())
            nxt = wm.end()
            if nxt < len(line) and line[nxt] in " \t":
                pos = nxt + 1
            else:
                break
        if not tokens or tokens[0] not in first_words:
            continue
        hit: str | None = None
        for k in range(len(tokens), 0, -1):
            cand = " ".join(tokens[:k])
            if cand in leaves:
                hit = cand
                break
        resolved.append(hit if hit is not None else " ".join(tokens))
    return resolved


def test_hook_output_commands_resolve() -> None:
    leaves = _baseline_leaves()
    first_words = {leaf.split(" ", 1)[0] for leaf in leaves}
    lines = _output_lines(HOOK.read_text(encoding="utf-8"))
    assert lines, "no emitted string literals found"

    prescribed: list[str] = []
    for ln in lines:
        prescribed.extend(_extract_fno_commands(ln, leaves, first_words))

    # Positive control: the scan reached real content, not an empty parse.
    # An absence here has two explanations and only the positive marker tells
    # them apart (assert-a-positive-marker).
    emitted = "\n".join(lines)
    assert "fno agent mail send" in emitted, "expected the /compact front door in output"
    assert "backlog" in prescribed, "collapsed backlog dispatcher was not resolved"

    bad = [v for v in prescribed if v not in leaves]
    assert not bad, f"hook output prescribes non-resolving fno commands: {bad}"
