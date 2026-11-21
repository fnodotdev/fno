#!/usr/bin/env python3
"""Grade check for vision-detection-list-matches-judge.

The vision skill's step 1 detection list must equal, in order, the list
vision_file reads in crates/fno-agent/src/eval/blueprint_judge.rs. The
skill cites that Rust list as its authority; this check keeps the
citation honest.
"""
import re
import sys

SKILL = "skills/vision/SKILL.md"
RUST = "crates/fno-agent/src/eval/blueprint_judge.rs"


def main() -> int:
    skill = open(SKILL, encoding="utf-8").read()
    rust = open(RUST, encoding="utf-8").read()
    m = re.search(r"for rel in \[(.*?)\]", rust, re.S)
    if not m:
        print(f"fail: no vision_file list found in {RUST}", file=sys.stderr)
        return 1
    rust_list = re.findall(r'"([^"]+)"', m.group(1))
    section = skill.split("## 1. Detect")[1].split("\n## ")[0]
    skill_list = [p for p in re.findall(r"`([^`]+)`", section)
                  if p.endswith("VISION.md") or p.endswith("vision.md")]
    if skill_list[:3] != rust_list:
        print(
            "fail: detection lists drift: "
            f"skill={skill_list[:3]} judge={rust_list}",
            file=sys.stderr,
        )
        return 1
    print(f"ok: detection lists match: {rust_list}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
