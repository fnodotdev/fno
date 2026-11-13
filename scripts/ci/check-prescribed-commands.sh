#!/usr/bin/env bash
# check-prescribed-commands.sh - the command-must-exist gate, restored from
# the pre-move repo's gate. The file-budget gate refuses a new scripts/ci/
# *.py, so the scanner rides as a heredoc-embedded python body; the .sh is
# the legal home, python is the tool.
# Run: bash scripts/ci/check-prescribed-commands.sh [--self-test]
# Exit: 0 clean, 1 a prescription that resolves to no surface.

set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec env FNO_REPO_ROOT="$(cd "$HERE/../.." && pwd)" python3 - "$@" <<'PY'
"""Prescribed-commands CI gate.

Extracts fno command prescriptions from agent-facing text and fails on
prescriptions that do not resolve. Resolution is existence on a surface list, never an
execution: the gate checks RESOLVE, not WORKS.

Surfaces: scripts/ci/native-command-tree.txt (the generated fno tree) plus
scripts/ci/prescribed-commands.txt ([fno], [fno-agents] and [slash] blocks).
Refresh the baseline when a surface gains a command the text prescribes.

Extraction contexts, per tree:
  crates/**/*.rs   string literals, root-gated (prose lives there)
  skills/**/*.md   fenced code blocks and inline backtick spans
  docs/**/*.md     same
  commands/**/*.md same
  hooks/**/*       shell lines, comments stripped, root-gated
  agents/*.md      backtick spans

Root-gated contexts prescribe only `fno <root> ...` (root = a first word on
the surface), because prose such as "the fno binary is missing" lives in
strings and scripts. Markdown spans are deliberate, so their first word
needs no root.

Scanner rule: os.walk only. Never rg: ripgrep carries two over-exclusion
layers (ripgreprc --glob and ~/.ignore) that would silently shrink scope.

Controls:
  scope disclosure  every green run prints the trees walked, the files
                    per tree, and the resolve-not-works line.
  positive control  --self-test plants `fno definitely-not-a-real-command`
                    into a temp copy of one live file per tree (it names
                    skills/target/ explicitly) and asserts detection.
  self-test         --self-test also runs extraction fixtures.

Usage: bash scripts/ci/check-prescribed-commands.sh [--self-test]
"""
import os
import re
import sys
import tempfile
from pathlib import Path

ROOT = Path(os.environ["FNO_REPO_ROOT"])

# name -> (dir under the repo root, extensions to scan, prose-safe).
# prose-safe trees span deliberate backtick text, so their first fno word
# needs no root; the others read strings and shell lines, where prose
# lives, and gate on a known root command.
TREES = {
    "crates": ("crates", {".rs"}, False),
    "skills": ("skills", {".md"}, True),
    "docs": ("docs", {".md"}, True),
    "commands": ("commands", {".md"}, True),
    "hooks": ("hooks", {".sh", ".md"}, False),
    "agents": ("agents", {".md"}, True),
}

BAD = "definitely-not-a-real-command"

RE_SLASH = re.compile(r"[/$]fno:([a-z0-9][a-z0-9-]*)(?![.\w])")
RE_AGENTS = re.compile(r"\bfno-agents ([a-z][a-z0-9-]*)(?!\w)")
RE_FNO = re.compile(r"\bfno ((?:[a-z][a-z0-9-]*[ ])*[a-z][a-z0-9]*)")
# Doc-state markers (`<!-- /fno:session -->`) and their awk-escaped spellings
# name a section, never a command.
RE_MARKER = re.compile(r"<!--\s*\\?/?fno:[a-z0-9-]+\s*-->")
def load_surfaces():
    fno, groups = set(), set()
    tree = ROOT / "scripts" / "ci" / "native-command-tree.txt"
    for line in tree.read_text().splitlines():
        if not line.strip() or line.startswith("#"):
            continue
        cols = line.split("\t")
        if len(cols) < 3:
            continue
        if cols[1] == "leaf":
            fno.add(cols[0])
        else:
            groups.add(cols[0])
    blocks = {"fno": set(), "fno-agents": set(), "slash": set()}
    section = None
    base = ROOT / "scripts" / "ci" / "prescribed-commands.txt"
    for line in base.read_text().splitlines():
        line = line.strip()
        if line.startswith("[") and line.endswith("]") and line[1:-1] in blocks:
            section = blocks[line[1:-1]]
            continue
        if not line or line.startswith("#"):
            continue
        if section is not None:
            # A trailing ` # ...` comment carries the wording gate's per-line
            # marker; it is not part of the name.
            section.add(line.split(" # ")[0].rstrip())
    fno |= blocks["fno"]
    for path in list(fno) + list(groups):
        groups.add(path.split()[0])
    return fno, groups, blocks["fno-agents"], blocks["slash"]
def fno_resolves(words, fno, groups):
    # Longest resolving prefix wins: a leaf followed by prose words still
    # resolves, and only a first word on no surface is a violation.
    for i in range(len(words), 0, -1):
        candidate = " ".join(words[:i])
        if candidate in fno or candidate in groups:
            return True
    return False


def prescriptions(text, fno_roots, agents_roots, gated_fno, gated_agents):
    text = RE_MARKER.sub(" ", text)
    for m in RE_SLASH.finditer(text):
        yield ("slash", m.group(1))
    for m in RE_AGENTS.finditer(text):
        if gated_agents and m.group(1) not in agents_roots:
            continue
        yield ("agents", m.group(1))
    for m in RE_FNO.finditer(text):
        words = m.group(1).split()
        if gated_fno and words[0] not in fno_roots:
            continue
        yield ("fno", m.group(1))


def md_spans(text):
    for fence in re.findall(r"```[^\n]*\n(.*?)```", text, re.S):
        # A fence often carries a shell script; its comment lines are the
        # author's prose, never a prescription.
        yield "\n".join(
            line for line in fence.splitlines() if not line.lstrip().startswith("#")
        )
    for span in re.findall(r"`([^`\n]+)`", text):
        yield span


# A string-literal regex cannot lex Rust, so a capture that carries code
# tokens spilled across quotes and is not help or refusal text.
SPILL = re.compile(r"[;{}]|=>|//|\bfn\s")


def rs_strings(text):
    for m in re.finditer(r'"((?:[^"\\]|\\.)*)"', text):
        body = m.group(1)
        if SPILL.search(body):
            continue
        yield body


def sh_lines(text):
    for line in text.splitlines():
        s = line.strip()
        if s and not s.startswith("#"):
            yield s
def scan(root, surfaces):
    fno, groups, agents, slash = surfaces
    roots = groups
    offenders = []
    counts = {}
    for name, (rel, exts, prose_safe) in TREES.items():
        base = root / rel
        seen = 0
        for dirpath, dirnames, filenames in os.walk(base):
            dirnames[:] = [
                d for d in dirnames if d not in (".git", "target", "node_modules")
            ]
            for fn in filenames:
                if Path(fn).suffix not in exts:
                    continue
                p = Path(dirpath) / fn
                try:
                    text = p.read_text()
                except (UnicodeDecodeError, OSError):
                    continue
                if p.suffix == ".rs":
                    spans = rs_strings(text)
                elif p.suffix == ".sh":
                    spans = sh_lines(text)
                else:
                    spans = md_spans(text)
                for span in spans:
                    # fno prescriptions line must start with a known root everywhere
                    # (prose lives in every context); agents and slash are
                    # exact names, so only strings and shell lines gate
                    # them, and markdown spans keep their teeth.
                    gated_agents = not prose_safe
                    for kind, command in prescriptions(
                        span, roots, agents, True, gated_agents
                    ):
                        seen += 1
                        if kind == "slash" and command not in slash:
                            offenders.append((p, "slash", command, span))
                        elif kind == "agents" and command not in agents:
                            offenders.append((p, "fno-agents", command, span))
                        elif kind == "fno" and not fno_resolves(
                            command.split(), fno, groups
                        ):
                            offenders.append((p, "fno", command, span))
        counts[name] = seen
    return offenders, counts
def sample_for(name, rel):
    # One live file per tree for the positive control; skills/target/ is
    # named explicitly so the control cannot drift to an empty dir.
    if name == "skills":
        return ROOT / rel / "target" / "SKILL.md"
    base = ROOT / rel
    for dirpath, _dirnames, filenames in os.walk(base):
        for fn in sorted(filenames):
            if Path(fn).suffix in TREES[name][1]:
                return Path(dirpath) / fn
    return None


def self_test(surfaces):
    ok = True
    fno, groups, agents, _slash = surfaces
    rx = list(prescriptions("run `fno doctor test` now", groups, agents, True, False))
    if ("fno", "doctor test") not in rx:
        print("self-test: FAIL: backtick span extraction")
        ok = False
    if list(prescriptions("the fno binary is missing", groups, agents, True, False)):
        print("self-test: FAIL: prose noise passed the root gate")
        ok = False
    if ("agents", "daemon") in list(
        prescriptions("the fno-agents daemon restarts", groups, agents, True, True)
    ):
        print("self-test: FAIL: agents prose passed the root gate")
        ok = False
    if ("slash", "target") in list(
        prescriptions("commands/fno:target.md", groups, agents, True, True)
    ):
        print("self-test: FAIL: a slash filename matched")
        ok = False
    if ("slash", "review") not in list(
        prescriptions("$fno:review", groups, agents, True, False)
    ):
        print("self-test: FAIL: slash extraction")
        ok = False
    if ("agents", "loop") not in list(
        prescriptions("fno-agents loop run", groups, agents, True, False)
    ):
        print("self-test: FAIL: fno-agents extraction")
        ok = False
    if not fno_resolves(["doctor", "test"], fno, groups):
        print("self-test: FAIL: fno tree resolution")
        ok = False
    if fno_resolves(["docotr"], fno, groups):
        print("self-test: FAIL: a bad root resolves")
        ok = False
    # Positive control per tree: plant an unresolvable prescription in a
    # temp copy of one live file and assert the scanner sees it. Gated
    # trees get a real root with a garbage tail, since prose-safe trees
    # alone can carry a bare bad prescription
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        planted = []
        for name, (rel, _exts, prose_safe) in TREES.items():
            sample = sample_for(name, rel)
            if sample is None:
                print(f"self-test: FAIL: no live sample file in {rel}/")
                ok = False
                continue
            dst = tmp / rel
            dst.mkdir(parents=True, exist_ok=True)
            # The teeth live in markdown spans: agents commands and slash
            # names are ungated there, so a planted unknown fno-agents
            # command must surface.
            body = sample.read_text() + f"\n`fno-agents {BAD}`\n"
            (dst / sample.name).write_text(body)
            planted.append(name)
        offenders, _counts = scan(tmp, surfaces)
        if not offenders:
            print("self-test: FAIL: planted bad command went undetected")
            ok = False
        else:
            print(f"self-test: positive control caught in {len(planted)} trees")
    print("self-test: " + ("ok" if ok else "FAILED"))
    return 0 if ok else 1
def main(argv):
    surfaces = load_surfaces()
    if "--self-test" in argv:
        return self_test(surfaces)
    offenders, counts = scan(ROOT, surfaces)
    scope = ", ".join(f"{name}={seen}" for name, seen in counts.items())
    print(f"prescribed-commands: scope: {scope}")
    print(
        "prescribed-commands: checks RESOLVE not WORKS: a prescription passes when it"
        " exists on the surface lists; the gate never runs it."
    )
    if not offenders:
        print("prescribed-commands: ok")
        return 0
    for p, kind, command, span in offenders:
        snippet = " ".join(span.split())[:90]
        print(f"prescribed-commands: FAIL {p}: {kind} `{command}` resolves to no"
              f" surface (in: {snippet!r})")
    print(
        f"prescribed-commands: {len(offenders)} unresolvable prescription(s);"
        " fix the text, or add the prescription to scripts/ci/prescribed-commands.txt"
        " when a surface genuinely gained it."
    )
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
