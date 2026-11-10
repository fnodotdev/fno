# The bridge wheel's one console entry point. uv's tool upgrade resolves the
# candidate's executables from entry points via lazy metadata, so a
# shared-scripts-only wheel reads as "no executables" and uv refuses the
# upgrade that lands the move. The entry point is named fno-bridge to stay
# clear of the four binaries and of the retired fno-py name; it hands the
# command to the staged `fno` binary beside it.
import os
import sys


def main() -> int:
    exe = os.path.join(os.path.dirname(os.path.abspath(sys.argv[0])), "fno")
    try:
        os.execv(exe, [exe] + sys.argv[1:])
    except OSError as exc:
        print(f"fno-py: the staged fno binary is missing at {exe}: {exc}", file=sys.stderr)
        return 127
