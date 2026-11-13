# Review ledger

The newest review verdict on each branch, newest first. Each row shows the time, the branch, the reviewed head, the verdict, the round and the finding count.

## What it runs

Nothing of its own. It is a feed mod. The host runs one fno read and draws the rows as a table:

```
fno-agents review-summary --ledger
```

That read prints JSON and changes nothing.

## Open it

```
fno config plugins activate plugins/fno-panels
fno config plugins open fno-panels/review-ledger
```

On a terminal the table redraws every 5 seconds. Piped, it prints once.

## Preview

The preview shows invented rows in a throwaway demo root. Re-shoot it with a branch-built fno:

```
scripts/ops/mux-demo-snapshot.sh --format png --mod-pane review-ledger --size 200x44 --out plugins/fno-panels/mods/review-ledger/preview.png
```
