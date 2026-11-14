# Decisions

Open it to see why an agent acted as it did, or to record a decision.

## What it shows

One table of decisions, newest first. A law is a standing rule that steers your agents. A ruling is a one-time answer to a question. Each row says what was decided and when.

## What it runs

Nothing of its own. It is a native mod: the mux paints this view itself. A feed mod draws a read-only table, and this view takes keys that run the law door, so it stays native. Open it from the mux Decisions tab, or read the same rows with `fno backlog decisions --json`.

Native views ship with fno, so they cannot be switched off.

## Maintainers: re-shoot the preview

The preview shows the snapshot fixture, which holds invented rows only. Re-shoot it with a branch-built fno:

```
scripts/ops/mux-demo-snapshot.sh --format png --view decisions --size 150x16 --out plugins/fno-panels/mods/decisions/preview.png
sips --cropToHeightWidth 508 2520 --cropOffset 34 0 plugins/fno-panels/mods/decisions/preview.png
```
