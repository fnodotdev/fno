//! `fno-agent evals connector-gate`: check a data connector's rows against a
//! hand-checked sample before a caller writes them.
//!
//! The gate compares values, never output shape. Three checks decide it:
//! value (every sampled row matches the person's values field by field),
//! completeness (every sampled key is present once, and the row count equals
//! a source-asserted total when one is passed) and provenance (every row
//! names its source document and the code version that built it).
//!
//! A sample alone cannot catch a dropped page: a page of k rows out of N
//! holds one of n sampled keys with probability 1-(1-k/N)^n, which is small
//! for a small sample. So with no `--source-count` the completeness check
//! prints that bound instead of claiming the page check passed.
//!
//! `--mutate` seeds a known defect into a copy of the rows and reruns the
//! gate, so a reviewer can prove the gate rejects each defect class.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

const USAGE: &str =
    "usage: fno-agent evals connector-gate --contract <toml> --rows <jsonl> --sample <jsonl>
    [--source-count <N>] [--code-dir <dir>] [--mutate wrong-field|dropped-page|stale-session|all]
    [--seed <n>] [--json]

Checks a connector's rows against a hand-checked sample, by value.
  --contract      TOML: key, fields (name + normalize), source_document_field,
                  code_version_field, max_mismatches (0), min_sample (30), page_size (25)
  --rows          JSONL, one connector row per line
  --sample        JSONL, one {key, fields, checked_by, checked_at} per line
  --source-count  the total row count the source asserts; without it a dropped
                  page is caught only when a sampled key falls in it, and the
                  receipt prints that detection bound 1-(1-page_size/rows)^n
  --code-dir      a git checkout whose HEAD is stamped as gate_code_version
  --mutate        seed one defect class (or all) into a copy of the rows and
                  rerun; exit 0 only when the clean run passes and every
                  seeded defect is rejected
  --seed          the mutation offset seed (default 1)
  --json          print the receipt as JSON

exit codes: 0 pass, 1 fail, 2 unreadable or malformed input";

const MUTATIONS: [&str; 3] = ["wrong-field", "dropped-page", "stale-session"];
const MISMATCH_LIST_CAP: usize = 20;

#[derive(Debug, thiserror::Error)]
enum InputError {
    #[error("missing required flag {0}")]
    MissingFlag(&'static str),
    #[error("flag {0} needs a value")]
    NoValue(String),
    #[error("unknown flag {0}")]
    UnknownFlag(String),
    #[error("{flag} {value:?} is not a valid value")]
    BadValue { flag: &'static str, value: String },
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path}: {source}")]
    Contract {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("{path}: line {line}: {detail}")]
    Line {
        path: PathBuf,
        line: usize,
        detail: String,
    },
    #[error("contract names no fields")]
    NoFields,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Contract {
    key: String,
    fields: Vec<FieldSpec>,
    source_document_field: String,
    code_version_field: String,
    #[serde(default)]
    max_mismatches: usize,
    #[serde(default = "default_min_sample")]
    min_sample: usize,
    #[serde(default = "default_page_size")]
    page_size: usize,
}

fn default_min_sample() -> usize {
    30
}

fn default_page_size() -> usize {
    25
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FieldSpec {
    name: String,
    #[serde(default)]
    normalize: Vec<Normalize>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Normalize {
    Exact,
    Casefold,
    CollapseWs,
    DateIso,
}

#[derive(Debug, Deserialize)]
struct SampleLine {
    key: String,
    fields: Map<String, Value>,
    checked_by: String,
    checked_at: String,
}

struct Args {
    contract: PathBuf,
    rows: PathBuf,
    sample: PathBuf,
    source_count: Option<usize>,
    code_dir: Option<PathBuf>,
    mutate: Vec<&'static str>,
    seed: u64,
    json: bool,
}

type Row = Map<String, Value>;

pub fn run(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return 0;
    }
    let parsed = match parse_args(args).and_then(|a| load(&a).map(|l| (a, l))) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("connector-gate: {e}");
            if matches!(
                e,
                InputError::MissingFlag(_) | InputError::NoValue(_) | InputError::UnknownFlag(_)
            ) {
                eprintln!("{USAGE}");
            }
            return 2;
        }
    };
    let (args, (contract, rows, sample, sample_sha, rows_sha)) = parsed;
    let baseline = check(&contract, &rows, &sample, args.source_count);
    let mutations: Vec<Mutation> = args
        .mutate
        .iter()
        .map(|class| {
            let mutated = mutate(&contract, &rows, class, args.seed);
            let run = check(&contract, &mutated, &sample, args.source_count);
            Mutation { class: *class, run }
        })
        .collect();
    if args.json {
        let muts: Vec<Value> = mutations
            .iter()
            .map(|m| json!({"class": m.class, "rejected": !m.run.pass, "caught_by": m.run.failed}))
            .collect();
        let receipt = json!({
            "contract": args.contract.display().to_string(),
            "sample_sha256": sample_sha,
            "rows_sha256": rows_sha,
            "gate_code_version": args.code_dir.as_deref().map(git_head),
            "sample_checked_by": checked_by(&sample),
            "sample_checked_at_latest": sample.iter().map(|s| s.checked_at.as_str()).max(),
            "baseline": baseline.receipt,
            "mutations": (!muts.is_empty()).then_some(muts),
        });
        println!("{receipt:#}");
    } else {
        print_human(&baseline, &mutations);
    }
    let ok = baseline.pass && mutations.iter().all(|m| !m.run.pass);
    if ok {
        0
    } else {
        1
    }
}

fn parse_args(args: &[String]) -> Result<Args, InputError> {
    let mut contract = None;
    let mut rows = None;
    let mut sample = None;
    let mut source_count = None;
    let mut code_dir = None;
    let mut mutate = Vec::new();
    let mut seed = 1u64;
    let mut json = false;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        if flag == "--json" {
            json = true;
            continue;
        }
        let mut value = || it.next().ok_or_else(|| InputError::NoValue(flag.clone()));
        match flag.as_str() {
            "--contract" => contract = Some(PathBuf::from(value()?)),
            "--rows" => rows = Some(PathBuf::from(value()?)),
            "--sample" => sample = Some(PathBuf::from(value()?)),
            "--code-dir" => code_dir = Some(PathBuf::from(value()?)),
            "--source-count" => {
                let v = value()?;
                source_count = Some(v.parse().map_err(|_| InputError::BadValue {
                    flag: "--source-count",
                    value: v.clone(),
                })?);
            }
            "--seed" => {
                let v = value()?;
                seed = v.parse().map_err(|_| InputError::BadValue {
                    flag: "--seed",
                    value: v.clone(),
                })?;
            }
            "--mutate" => {
                let v = value()?;
                mutate = match v.as_str() {
                    "all" => MUTATIONS.to_vec(),
                    other => vec![*MUTATIONS.iter().find(|m| **m == other).ok_or_else(|| {
                        InputError::BadValue {
                            flag: "--mutate",
                            value: v.clone(),
                        }
                    })?],
                };
            }
            other => return Err(InputError::UnknownFlag(other.to_string())),
        }
    }
    Ok(Args {
        contract: contract.ok_or(InputError::MissingFlag("--contract"))?,
        rows: rows.ok_or(InputError::MissingFlag("--rows"))?,
        sample: sample.ok_or(InputError::MissingFlag("--sample"))?,
        source_count,
        code_dir,
        mutate,
        seed,
        json,
    })
}

type Loaded = (Contract, Vec<Row>, Vec<SampleLine>, String, String);

fn load(args: &Args) -> Result<Loaded, InputError> {
    let text = read(&args.contract)?;
    let contract: Contract = toml::from_str(&text).map_err(|source| InputError::Contract {
        path: args.contract.clone(),
        source,
    })?;
    if contract.fields.is_empty() {
        return Err(InputError::NoFields);
    }
    let rows_text = read(&args.rows)?;
    let rows = parse_jsonl::<Row>(&args.rows, &rows_text)?;
    let sample_text = read(&args.sample)?;
    let sample = parse_jsonl::<SampleLine>(&args.sample, &sample_text)?;
    Ok((
        contract,
        rows,
        sample,
        sha256(&sample_text),
        sha256(&rows_text),
    ))
}

fn read(path: &Path) -> Result<String, InputError> {
    std::fs::read_to_string(path).map_err(|source| InputError::Read {
        path: path.to_path_buf(),
        source,
    })
}

fn parse_jsonl<T: serde::de::DeserializeOwned>(
    path: &Path,
    text: &str,
) -> Result<Vec<T>, InputError> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let v = serde_json::from_str(line).map_err(|e| InputError::Line {
            path: path.to_path_buf(),
            line: i + 1,
            detail: e.to_string(),
        })?;
        out.push(v);
    }
    Ok(out)
}

fn sha256(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn git_head(dir: &Path) -> Option<String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(dir).args(["rev-parse", "HEAD"]);
    let out = crate::bounded_cmd::output_with_timeout(cmd, 10)?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn checked_by(sample: &[SampleLine]) -> Vec<String> {
    let mut who: Vec<String> = sample.iter().map(|s| s.checked_by.clone()).collect();
    who.sort();
    who.dedup();
    who
}

/// A cell as text: a string as itself, null as blank, anything else as JSON.
fn text(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

fn normalize(value: &str, steps: &[Normalize]) -> String {
    let mut out = value.to_string();
    for step in steps {
        out = match step {
            Normalize::Exact => out,
            Normalize::Casefold => out.to_lowercase(),
            Normalize::CollapseWs => out.split_whitespace().collect::<Vec<_>>().join(" "),
            Normalize::DateIso => date_iso(&out),
        };
    }
    out
}

/// A US `M/D/YYYY` date or an ISO date or timestamp as `YYYY-MM-DD`. Any
/// other text passes through unchanged, so it mismatches instead of matching
/// by accident.
fn date_iso(value: &str) -> String {
    let v = value.trim();
    for fmt in ["%m/%d/%Y", "%Y-%m-%d"] {
        if let Ok(d) = chrono::NaiveDate::parse_from_str(v, fmt) {
            return d.format("%Y-%m-%d").to_string();
        }
    }
    v.get(..10)
        .and_then(|head| chrono::NaiveDate::parse_from_str(head, "%Y-%m-%d").ok())
        .map_or_else(|| v.to_string(), |d| d.format("%Y-%m-%d").to_string())
}

struct Run {
    pass: bool,
    failed: Vec<&'static str>,
    rows: usize,
    sample_n: usize,
    page_size: usize,
    bound: Option<f64>,
    receipt: Value,
}

struct Mutation {
    class: &'static str,
    run: Run,
}

struct KeyMatch<'a> {
    missing: Vec<String>,
    duplicate: Vec<String>,
    matched: Vec<(&'a SampleLine, &'a Row)>,
}

/// Join each sampled key to its rows through one index over the rows.
fn match_keys<'a>(contract: &Contract, rows: &'a [Row], sample: &'a [SampleLine]) -> KeyMatch<'a> {
    let mut by_key: std::collections::HashMap<String, Vec<&Row>> = Default::default();
    for r in rows {
        by_key
            .entry(text(r.get(&contract.key)))
            .or_default()
            .push(r);
    }
    let mut out = KeyMatch {
        missing: Vec::new(),
        duplicate: Vec::new(),
        matched: Vec::new(),
    };
    for s in sample {
        match by_key.get(&s.key).map(Vec::as_slice).unwrap_or_default() {
            [] => out.missing.push(s.key.clone()),
            [one] => out.matched.push((s, *one)),
            _ => out.duplicate.push(s.key.clone()),
        }
    }
    out
}

/// The value check: every contract field of every matched row against the
/// person's value, after normalization.
fn value_check(contract: &Contract, matched: &[(&SampleLine, &Row)]) -> (bool, Value) {
    let mut field_rows = Vec::new();
    let mut mismatches = Vec::new();
    let mut mismatch_total = 0usize;
    for f in &contract.fields {
        let mut hits = 0usize;
        for (s, row) in matched {
            let want = normalize(&text(s.fields.get(&f.name)), &f.normalize);
            let got = normalize(&text(row.get(&f.name)), &f.normalize);
            if want == got {
                hits += 1;
            } else {
                mismatch_total += 1;
                if mismatches.len() < MISMATCH_LIST_CAP {
                    mismatches.push(json!({
                        "key": s.key, "field": f.name, "expected": want, "got": got,
                    }));
                }
            }
        }
        let (rate, low, high) = crate::evals_trend::wilson_rate(matched.len(), hits);
        field_rows.push(json!({
            "field": f.name, "n": matched.len(), "hits": hits,
            "accuracy": rate, "wilson_low": low, "wilson_high": high,
        }));
    }
    let ok = !matched.is_empty() && mismatch_total <= contract.max_mismatches;
    let receipt = json!({
        "pass": ok,
        "fields": field_rows,
        "mismatch_total": mismatch_total,
        "max_mismatches": contract.max_mismatches,
        "mismatches": mismatches,
    });
    (ok, receipt)
}

/// One-based line numbers of rows that fail a predicate.
fn row_lines(rows: &[Row], bad: impl Fn(&Row) -> bool) -> Vec<usize> {
    rows.iter()
        .enumerate()
        .filter(|(_, r)| bad(r))
        .map(|(i, _)| i + 1)
        .collect()
}

fn blank(r: &Row, field: &str) -> bool {
    text(r.get(field)).trim().is_empty()
}

fn check(
    contract: &Contract,
    rows: &[Row],
    sample: &[SampleLine],
    source_count: Option<usize>,
) -> Run {
    let mut failed = Vec::new();
    let n = sample.len();
    let sample_ok = n >= contract.min_sample;
    if !sample_ok {
        failed.push("sample_size");
    }

    let keys = match_keys(contract, rows, sample);
    let (value_ok, value) = value_check(contract, &keys.matched);
    if !value_ok {
        failed.push("value");
    }

    let count_check = match source_count {
        Some(want) if want == rows.len() => "match",
        Some(_) => "mismatch",
        None => "absent",
    };
    let bound = (source_count.is_none() && !rows.is_empty()).then(|| {
        let k = contract.page_size.min(rows.len()) as f64;
        1.0 - (1.0 - k / rows.len() as f64).powi(n as i32)
    });
    // A row with every contract field blank is a husk: a blocked or expired
    // response the connector kept. Unsampled husks are invisible to the
    // value check, so this one reads every row.
    let empty = row_lines(rows, |r| {
        let mut data = contract.fields.iter().filter(|f| f.name != contract.key);
        data.all(|f| blank(r, &f.name))
    });
    let complete_ok = keys.missing.is_empty()
        && keys.duplicate.is_empty()
        && empty.is_empty()
        && count_check != "mismatch";
    if !complete_ok {
        failed.push("completeness");
    }

    let unproven = row_lines(rows, |r| {
        blank(r, &contract.source_document_field) || blank(r, &contract.code_version_field)
    });
    let provenance_ok = unproven.is_empty();
    if !provenance_ok {
        failed.push("provenance");
    }

    let pass = failed.is_empty();
    let receipt = json!({
        "verdict": if pass { "pass" } else { "fail" },
        "failed_checks": failed,
        "sample": { "n": n, "min_sample": contract.min_sample, "pass": sample_ok },
        "value": value,
        "completeness": {
            "pass": complete_ok,
            "rows": rows.len(),
            "source_count": source_count,
            "count_check": count_check,
            "missing_keys": keys.missing,
            "duplicate_keys": keys.duplicate,
            "empty_rows": empty.len(),
            "first_empty_rows": empty.iter().take(10).collect::<Vec<_>>(),
            "dropped_page_detection_bound": bound,
            "page_size": contract.page_size,
        },
        "provenance": {
            "pass": provenance_ok,
            "rows_missing": unproven.len(),
            "first_rows_missing": unproven.iter().take(10).collect::<Vec<_>>(),
        },
    });
    Run {
        pass,
        failed,
        rows: rows.len(),
        sample_n: n,
        page_size: contract.page_size,
        bound,
        receipt,
    }
}

/// splitmix64: a fixed seed gives the same offsets on every machine.
fn next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn mutate(contract: &Contract, rows: &[Row], class: &str, seed: u64) -> Vec<Row> {
    let mut state = seed;
    let mut out = rows.to_vec();
    if out.is_empty() {
        return out;
    }
    match class {
        // A mapping defect hits every row, as a parser bug would. The pair
        // is the first adjacent one, from a seeded start, whose values
        // differ on some row, so the swap is never a silent no-op.
        "wrong-field" => {
            let names: Vec<&str> = contract.fields.iter().map(|f| f.name.as_str()).collect();
            let pairs: Vec<(&str, &str)> = names
                .windows(2)
                .filter_map(|w| match w {
                    [a, b] => Some((*a, *b)),
                    _ => None,
                })
                .collect();
            if pairs.is_empty() {
                return out;
            }
            let start = (next(&mut state) as usize) % pairs.len();
            let pair = pairs
                .iter()
                .cycle()
                .skip(start)
                .take(pairs.len())
                .find(|(a, b)| out.iter().any(|r| text(r.get(*a)) != text(r.get(*b))))
                .copied();
            if let Some((a, b)) = pair {
                for r in &mut out {
                    let va = r.remove(a).unwrap_or(Value::Null);
                    let vb = r.remove(b).unwrap_or(Value::Null);
                    r.insert(a.to_string(), vb);
                    r.insert(b.to_string(), va);
                }
            }
        }
        "dropped-page" => {
            let k = contract.page_size.min(out.len());
            let at = (next(&mut state) as usize) % (out.len() - k + 1);
            out.drain(at..at + k);
        }
        // A blocked or expired session tail: rows keep their key and the
        // provenance the connector stamps, and every contract cell comes
        // back blank. Keeping provenance means only the data checks can
        // catch it.
        _ => {
            let at = (next(&mut state) as usize) % out.len();
            for r in out.iter_mut().skip(at) {
                for f in contract.fields.iter().filter(|f| f.name != contract.key) {
                    r.insert(f.name.clone(), Value::String(String::new()));
                }
            }
        }
    }
    out
}

fn print_human(base: &Run, mutations: &[Mutation]) {
    println!(
        "connector-gate: {} (sample {}, rows {})",
        if base.pass { "pass" } else { "fail" },
        base.sample_n,
        base.rows,
    );
    for (label, key) in [
        ("sample", "sample_size"),
        ("value", "value"),
        ("completeness", "completeness"),
        ("provenance", "provenance"),
    ] {
        let failed = base.failed.contains(&key);
        println!("  {label}: {}", if failed { "FAIL" } else { "pass" });
    }
    if let Some(b) = base.bound {
        println!(
            "  no --source-count: a dropped page of {} rows is caught with p={b:.3}",
            base.page_size
        );
    }
    for m in mutations {
        let verdict = if m.run.pass {
            "NOT rejected"
        } else {
            "rejected"
        };
        println!(
            "  mutation {}: {verdict} (caught by {})",
            m.class,
            m.run.failed.join(", ")
        );
    }
}
