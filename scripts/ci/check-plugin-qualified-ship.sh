#!/usr/bin/env bash
# check-plugin-qualified-ship.sh - every /ship the shipped tree names is
# plugin-qualified. Claude Code now ships its own /ship (Claude Ship beta),
# so a bare /ship in skill text, docs, hooks or agent prose opens Claude's
# command instead of fno's. The one spelling is /fno:ship (codex $fno:ship);
# the docs skill is /fno:ship-docs.
#
# The gate scans the whole tracked tree with git grep, not a diff: the sweep
# is done, so any hit is new. The match is (^|[^:a-z])/ship\b: a /ship (or
# the /ship-docs prefix) that no /fno: or $fno: spelling already covers.
#
# Allowed:
#   this script                        it carries the pattern
#   CHANGELOG.md                       history records the launch spelling
#   the "$/ship" scoreboard header     a currency column (dollars per ship)
#
# An absence-only pass has two explanations ("clean" and "the instrument
# never matched anything"), so --self-test feeds the scan planted rows and
# asserts exactly the bare spellings are refused.
#
# Run:  bash scripts/ci/check-plugin-qualified-ship.sh [--self-test]
# Exit: 0 clean, 1 a bare /ship in the tree.

set -euo pipefail

scan() {
    # stdin: path:line:text rows (git grep -n). stdout: the refused rows.
    perl -ne '
        chomp;
        my ($f, $n, $t) = split /:/, $_, 3;
        next unless defined $t;
        next if $f eq "scripts/ci/check-plugin-qualified-ship.sh";
        next if $f eq "CHANGELOG.md";
        (my $s = $t) =~ s/"\$\/ship"//g;
        print "$_\n" if $s =~ /(^|[^:a-z])\/ship\b/;
    '
}

if [[ "${1:-}" == "--self-test" ]]; then
    rows=$'skills/ship/SKILL.md:1:run /ship to deliver it\n'
    rows+=$'skills/target/SKILL.md:2:the /ship-docs skill paints the docs\n'
    rows+=$'docs/x.md:3:run /fno:ship pr create to open the PR\n'
    rows+=$'docs/x.md:4:on codex run $fno:ship pr create\n'
    rows+=$'AGENTS.md:5:see [skills/ship/SKILL.md](skills/ship/SKILL.md)\n'
    rows+=$'crates/x/src/a.rs:6:        "$/ship",\n'
    rows+=$'CHANGELOG.md:7:* Added the `/ship` umbrella\n'
    got="$(printf '%s' "$rows" | scan)"
    want=$'skills/ship/SKILL.md:1:run /ship to deliver it\nskills/target/SKILL.md:2:the /ship-docs skill paints the docs'
    if [[ "$got" != "$want" ]]; then
        printf 'check-plugin-qualified-ship self-test FAILED\nwant:\n%s\ngot:\n%s\n' "$want" "$got" >&2
        exit 1
    fi
    echo "check-plugin-qualified-ship self-test: ok"
    exit 0
fi

cd "$(git rev-parse --show-toplevel)"
hits="$( { git grep -n -I -F '/ship' || true; } | scan )"
if [[ -n "$hits" ]]; then
    printf '%s\n' "$hits"
    echo 'check-plugin-qualified-ship: write /fno:ship (codex $fno:ship); the docs skill is /fno:ship-docs. Claude Code opens its own /ship on the bare spelling. A bare spelling that must stay (a recorded transcript, an input matcher): add its file or quoted literal to the Allowed list in this script.' >&2
    exit 1
fi
echo "check-plugin-qualified-ship: ok"
