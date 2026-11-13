# Decisions

Laws and rulings in one table, with the door that records a new law from your own terminal.

## What it runs

Nothing of its own. It is a native mod: the mux paints this view itself. A feed mod draws a read-only table, and this view takes keys that run the law door, so it stays native. Open it from the mux Decisions tab, or read the same rows with `fno backlog decisions --json`.

Native views are built in, so they cannot be switched off.

## Preview

The preview shows the snapshot fixture, which holds invented rows only. Re-shoot it with a branch-built fno:

```
scripts/ops/mux-demo-snapshot.sh --format png --view decisions --size 150x16 --out plugins/fno-panels/mods/decisions/preview.png
sips --cropToHeightWidth 508 2520 --cropOffset 34 0 plugins/fno-panels/mods/decisions/preview.png
```
