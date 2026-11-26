# Connector eval gate

`fno doctor evals connector-gate` checks the rows a data connector built against rows a person checked by hand. It compares values, field by field. It does not check output shape. A caller runs it before it writes the rows. If the exit code is not 0, the caller refuses the write.

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

Each field takes a list of normalizers. The gate applies them in order to both sides before the compare. These are the four normalizers:

- `exact` changes nothing.
- `casefold` writes the text in lower case.
- `collapse_ws` trims the text and puts one space between words.
- `date_iso` writes a `M/D/YYYY` date or an ISO timestamp as `YYYY-MM-DD`.

Text that `date_iso` cannot read stays as it is. So it mismatches and never matches by accident.

The rows file is JSONL, with one connector row per line in the form the connector writes.

The sample file is JSONL, with one line per hand-checked row: `{"key": "...", "fields": {...}, "checked_by": "...", "checked_at": "YYYY-MM-DD"}`. The person draws the keys from the source, never from the connector's output. A key drawn from the output can never show a row the connector missed.

## The checks

The value check finds the row for each sampled key. It compares every contract field after normalization. The receipt gives each field's hits, its accuracy and a 95% Wilson interval. If the mismatch total is above `max_mismatches`, the value check fails.

If a sampled key has no row or more than one row, the completeness check fails. If you pass `--source-count` and the row count differs from it, the completeness check also fails.

If any row, sampled or not, has a blank source document field or a blank code version field, the provenance check fails.

If the sample holds fewer than `min_sample` lines, the sample size check fails. A pass on three hand-checked rows is not evidence.

The receipt also records `sample_sha256`, `rows_sha256` and who checked the sample. If you pass `--code-dir`, it records the git HEAD of that directory as `gate_code_version`.

## Why a sample alone misses a dropped page

A dropped page of k rows out of N holds one of n sampled keys with probability 1-(1-k/N)^n. Take the CA `sos_entity` table (N = 8,302), a 30-key sample and one dropped page of 25 rows. The probability is 8.6%. So the value check alone lets about 91 of 100 such defects through. Pass `--source-count` with the total the source asserts. If the source asserts no total, the receipt prints the bound as `dropped_page_detection_bound`. The check then does not claim it caught the page.

## Prove the gate rejects defects

`--mutate wrong-field|dropped-page|stale-session|all` seeds a defect into a copy of the rows and runs the gate again. `--seed` (default 1) sets the offsets.

- `wrong-field` swaps two contract fields on every row. A parser mapping bug does the same.
- `dropped-page` removes `page_size` rows in a row.
- `stale-session` blanks every field except the key, from an offset to the end. A blocked or expired session tail looks like this.

With `--mutate`, two things must be true for exit 0. The clean run passes, and the gate rejects every seeded defect. A gate that refuses everything cannot read green, because its clean run fails.

## Call it from a connector

There are two patterns. In the first, the connector runs the gate as a subprocess before its upsert. If the exit code is not 0, the connector writes nothing. In the second, a connector plan adds the gate command to its `done_probes`, so the done gate runs it on the PR.

## Write a contract for a new state

1. Pick the key the source and the rows share, such as the entity number.
2. List each field a customer reads, with the normalizers its source format needs.
3. Name the row fields that hold the source document and the code version.
4. Set `page_size` to the rows one source page or API call returns.
5. Draw at least `min_sample` keys from the source, check each row by hand, and write the sample file.
