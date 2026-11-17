//! `observe` door tests: identity + usage readback from the attempt's own
//! transcript store. Tempdir fixtures only; the real store is never touched.

use super::observe;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn fld<'a>(v: &'a Value, k: &str) -> &'a Value {
    v.get(k).unwrap_or(&Value::Null)
}

fn observe_payload(lane: Value, extra: Value) -> Value {
    let mut p = json!({
        "lane": lane,
        "attempted": true,
        "spawned": true,
        "workdir": "/tmp/attempt-wd",
        "started_epoch": 1_700_000_000.0_f64,
        "now_epoch": 1_700_000_100.0_f64,
    });
    if let Some(obj) = extra.as_object() {
        for (k, v) in obj {
            p[k.as_str()] = v.clone();
        }
    }
    p
}

fn plant_jsonl(root: &Path, dir_name: &str, name: &str, body: &str, mtime: u64) -> PathBuf {
    let dir = root.join(dir_name);
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&p)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(mtime))
        .unwrap();
    p
}

fn claude_line(cwd: &str, model: &str, inp: u64, out: u64, cr: u64, cw: u64) -> String {
    json!({
        "type": "assistant",
        "cwd": cwd,
        "message": {"role": "assistant", "model": model,
                     "usage": {"input_tokens": inp, "output_tokens": out,
                                "cache_read_input_tokens": cr,
                                "cache_creation_input_tokens": cw}},
    })
    .to_string()
}

const WD: &str = "/tmp/attempt-wd";

fn observe_claude_finds_transcript_by_workdir_and_sums_usage() {
    let tmp = tempfile::TempDir::new().unwrap();
    let uuid = "d8996f9b-8854-4f22-8c28-c7819c6d0316";
    let body = format!(
        "{}\n{}\n{}\n",
        json!({"type": "user", "cwd": WD, "message": {"role": "user"}}),
        claude_line(WD, "glm-5.3-flash", 100, 20, 30, 5),
        claude_line(WD, "glm-5.3-flash", 50, 10, 0, 0),
    );
    plant_jsonl(
        tmp.path(),
        "-repo-wt",
        &format!("{uuid}.jsonl"),
        &body,
        1_700_000_050,
    );
    let lane =
        json!({"name": "glm", "harness": "claude", "model": "glm-5.3-flash", "effort": "high"});
    let out = observe(&observe_payload(
        lane,
        json!({"projects_root": tmp.path().to_str().unwrap()}),
    ));
    assert_eq!(out["lane_status"], "ok", "out: {out}");
    assert_eq!(out["observed_model"], "glm-5.3-flash");
    assert_eq!(out["observed_session_id"], uuid);
    assert_eq!(out["substituted"], false);
    assert_eq!(out["usage"]["input"], 150);
    assert_eq!(out["usage"]["output"], 30);
    assert_eq!(out["usage"]["cache_read"], 30);
    assert_eq!(out["usage"]["cache_write"], 5);
    assert_eq!(out["usage_source"], "claude-transcript");
}

fn observe_claude_no_transcript_reads_unverified_with_null_usage() {
    let tmp = tempfile::TempDir::new().unwrap();
    let lane =
        json!({"name": "glm", "harness": "claude", "model": "glm-5.3-flash", "effort": "high"});
    let out = observe(&observe_payload(
        lane,
        json!({"projects_root": tmp.path().to_str().unwrap()}),
    ));
    assert_eq!(out["lane_status"], "unverified");
    assert!(
        out["lane_reason"]
            .as_str()
            .unwrap()
            .contains("no claude transcript"),
        "out: {out}"
    );
    assert_eq!(out["usage"], Value::Null);
    // A harness with no reader reads the same way.
    let lane = json!({"name": "x", "harness": "phantom", "model": "m", "effort": "high"});
    let out = observe(&observe_payload(lane, json!({})));
    assert_eq!(out["lane_status"], "unverified");
    assert!(
        out["lane_reason"]
            .as_str()
            .unwrap()
            .contains("no transcript reader for harness 'phantom'"),
        "out: {out}"
    );
    assert_eq!(out["usage"], Value::Null);
    // pi reads its own store now: an empty store is unverified with a pi reason.
    let lane = json!({"name": "x", "harness": "pi", "model": "m", "effort": "high"});
    let out = observe(&observe_payload(lane, json!({})));
    assert_eq!(out["lane_status"], "unverified");
    assert!(
        out["lane_reason"]
            .as_str()
            .unwrap()
            .contains("no readable pi session"),
        "out: {out}"
    );
    assert_eq!(out["usage"], Value::Null);
}

fn footnote_line(seq: u64, ty: &str, data: Value) -> String {
    json!({"v": 1, "seq": seq, "id": format!("r{seq}"),
        "ts": "2026-10-09T00:00:00.000Z", "session_id": "fn-s1", "type": ty, "data": data})
    .to_string()
}

fn footnote_body(provider_id: Value) -> String {
    format!(
        "{}\n{}\n{}\n{}\n",
        footnote_line(0, "header", json!({"cwd": WD, "session_id": "fn-s1"})),
        footnote_line(
            1,
            "model_request",
            json!({"turn": 1, "provider_id": provider_id})
        ),
        footnote_line(
            2,
            "model_response",
            json!({"reported_model": "glm-5.3-flash"})
        ),
        footnote_line(
            3,
            "usage",
            json!({"input_tokens": 100, "output_tokens": 20,
            "cache_read_tokens": 3, "cache_write_tokens": 1})
        ),
    )
}

fn observe_footnote_without_provider_stays_bare_and_substituted() {
    let tmp = tempfile::TempDir::new().unwrap();
    plant_jsonl(
        tmp.path(),
        "proj",
        "fn-s1.jsonl",
        &footnote_body(Value::Null),
        1_700_000_050,
    );
    let lane = json!({"name": "fn", "harness": "footnote", "model": "zai/glm-5.3-flash"});
    let out = observe(&observe_payload(
        lane,
        json!({"footnote_sessions_root": tmp.path().to_str().unwrap()}),
    ));
    assert_eq!(out["observed_model"], "glm-5.3-flash");
    assert_eq!(out["lane_status"], "substituted", "out: {out}");
}

#[test]
fn all_contracts_in_one_declaration() {
    observe_claude_finds_transcript_by_workdir_and_sums_usage();
    observe_claude_no_transcript_reads_unverified_with_null_usage();
    observe_footnote_without_provider_stays_bare_and_substituted();
}
