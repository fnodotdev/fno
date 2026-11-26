# Connector eval gate

`fno doctor evals connector-gate` checks the rows a data connector built against rows a person checked by hand. It compares values, field by field. It does not check output shape. A caller runs it before it writes the rows, and refuses the write when the exit code is not 0.

## Run it

```bash
fno doctor evals connector-gate \
  --contract contract.toml \
  --rows rows.jsonl \
  --sample sample.jsonl \
  --source-count 8302 \
  --json
```

The same action runs as `fno-agent evals connector-gate`.

| Exit | Meaning |
|---|---|
| 0 | every check passed |
| 1 | a check failed; the receipt names it under `failed_checks` |
| 2 | an input is missing, unreadable or malformed; stderr names the file and line |

## The three inputs

The contract is a TOML file. It names the key that joins a row to a sample entry, the fields to compare, and the two provenance fields.

```toml
key = "entity_number"
source_document_field = "source_url"
code_version_field = "code_version"
max_mismatches = 0   # field mismatches allowed across the sample
min_sample = 30      # a smaller sample fails the gate
page_size = 25       # the rows one source page holds

[[fields]]
name = "operator_name"
normalize = ["collapse_ws", "casefold"]
```

Each field takes a list of normalizers, applied in order to both sides before the compare: `exact` (no change), `casefold` (lower case), `collapse_ws` (trim, and one space between words), and `date_iso` (a `M/D/YYYY` date or an ISO timestamp becomes `YYYY-MM-DD`). Text that `date_iso` cannot read stays as it is, so it mismatches instead of matching by accident.

The rows file is JSONL, one connector row per line, as the connector would write it.

The sample file is JSONL, one line per hand-checked row: `{"key": "...", "fields": {...}, "checked_by": "...", "checked_at": "YYYY-MM-DD"}`. The person draws the keys from the source, never from the connector's output. A key drawn from the output can never show a row the connector missed.

## The checks

The value check finds each sampled key's row and compares every contract field after normalization. The receipt gives each field's hits, its accuracy and a 95% Wilson interval. It fails when the mismatch total is above `max_mismatches`.

The completeness check fails when a sampled key has no row or more than one row. When you pass `--source-count`, it also fails when the row count differs from that number.

The provenance check fails when any row, sampled or not, has a blank source document field or a blank code version field.

The sample size check fails when the sample holds fewer than `min_sample` lines. A pass on three hand-checked rows is not evidence.

The receipt also records `sample_sha256`, `rows_sha256`, who checked the sample, and `gate_code_version` (the git HEAD of `--code-dir` when you pass it).

## Why a sample alone misses a dropped page

A dropped page of k rows out of N holds one of n sampled keys with probability 1-(1-k/N)^n. For the CA `sos_entity` table (N = 8,302), a 30-key sample and one dropped page of 25 rows, that is 8.6%. So the value check alone lets about 91 of 100 such defects through. Pass `--source-count` with the total the source asserts. When the source asserts no total, the receipt prints this bound as `dropped_page_detection_bound` and the check does not claim it caught the page.

## Prove the gate rejects defects

`--mutate wrong-field|dropped-page|stale-session|all` seeds a defect into a copy of the rows and runs the gate again. `--seed` (default 1) sets the offsets.

- `wrong-field` swaps two contract fields on every row, as a parser mapping bug would.
- `dropped-page` removes `page_size` rows in a row.
- `stale-session` blanks every field except the key from an offset to the end, as a blocked or expired session tail would.

With `--mutate`, the exit is 0 only when the clean run passes and every seeded defect is rejected. A gate that refuses everything cannot read green, because its clean run fails.

## Call it from a connector

Two patterns. A connector runs the gate as a subprocess before its upsert and refuses to write when the exit code is not 0. A connector plan also adds the gate command to its `done_probes`, so the done gate runs it on the PR.

## Write a contract for a new state

1. Pick the key the source and the rows share, such as the entity number.
2. List each field a customer reads, with the normalizers its source format needs.
3. Name the row fields that hold the source document and the code version.
4. Set `page_size` to the rows one source page or API call returns.
5. Draw at least `min_sample` keys from the source, check each row by hand, and write the sample file.
