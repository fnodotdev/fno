"""Capture pinned live evidence for the manual architect calibration."""

import argparse
import json
import subprocess
from pathlib import Path


def run(args):
    return subprocess.check_output(args, text=True, timeout=60)


def node(node_id):
    return json.loads(run(["fno", "backlog", "get", node_id, "--json"]))


def capture(pr, node_id, mission):
    entry = node(node_id)
    if entry.get("pr_number") != pr:
        raise ValueError(f"{node_id} does not own PR {pr}")
    root = Path(entry["cwd"])
    plan = (root / entry["plan_path"]).read_text()
    endpoint = f"repos/{{owner}}/{{repo}}/pulls/{pr}"
    facts = json.loads(run(["gh", "api", endpoint]))
    if not facts.get("merged_at"):
        raise ValueError(f"PR {pr} has not merged")
    if entry.get("pr_url", "").lower() != facts["html_url"].lower():
        raise ValueError("node and captured PR name different repositories")
    comparison = f"repos/{{owner}}/{{repo}}/compare/{facts['base']['sha']}...{facts['head']['sha']}"
    compared = json.loads(run(["gh", "api", comparison]))
    files = [row["filename"] for row in compared["files"]]
    if len(files) != facts["changed_files"]:
        raise ValueError(f"incomplete immutable changed-file list for PR {pr}")
    diff = run(["gh", "api", comparison, "--header", "Accept: application/vnd.github.diff"])
    if diff and not diff.lstrip().startswith("diff --git "):
        raise ValueError("comparison response is not a unified diff")
    after = json.loads(run(["gh", "api", endpoint]))
    if after["head"]["sha"] != facts["head"]["sha"]:
        raise ValueError(f"PR {pr} moved during capture")
    vision = {}
    if isinstance(mission, str) and mission.strip():
        vision["mission"] = mission
    current = entry
    for _ in range(4):
        path = current.get("vision_path")
        if path:
            vision[path] = (root / path).read_text()
        parent = current.get("parent")
        if not parent:
            break
        current = node(parent)
    return {
        "pr": pr,
        "head": facts["head"]["sha"],
        "node": entry,
        "plan": plan,
        "agents": (root / "AGENTS.md").read_text(),
        "vision": vision,
        "diff": diff[:80000],
        "diff_truncated": len(diff) > 80000,
        "files": files,
        "body": facts.get("body") or "",
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sample", action="append", required=True, metavar="PR:NODE")
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    if len(args.sample) != 3:
        parser.error("supply exactly three merged PR/node pairs")
    mission = json.loads(run(["fno", "config", "get", "project.vision", "--json"]))
    rows = []
    for sample in args.sample:
        pr, separator, node_id = sample.partition(":")
        if not separator or not pr.isdecimal() or not node_id:
            parser.error("each sample must be PR:NODE")
        row = capture(int(pr), node_id, mission.get("value"))
        rows.append(row)
        print(json.dumps({"pr": row["pr"], "head": row["head"], "files": len(row["files"])}))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(rows))


if __name__ == "__main__":
    main()
