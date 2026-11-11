//! Mods and layouts on the plugin host: the manifest, the switches, the URL
//! install's consent boundary, the gallery read and the feed renderer.

use super::install::{self, InstallError};
use super::manifest::PackManifest;
use super::registry::{Registry, Store};
use super::verify::{load_manifest, verify_pack};
use std::path::Path;

fn png(w: u32, h: u32) -> Vec<u8> {
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13];
    out.extend_from_slice(b"IHDR");
    out.extend_from_slice(&w.to_be_bytes());
    out.extend_from_slice(&h.to_be_bytes());
    out.extend_from_slice(&[8, 2, 0, 0, 0, 0, 0, 0, 0]);
    out
}

/// A mods-only pack: one feed mod, one command mod, one layout.
fn write_pack(dir: &Path, extra: &str, preview: &str) {
    std::fs::create_dir_all(dir.join("m")).unwrap();
    std::fs::write(dir.join("m/p.png"), png(320, 180)).unwrap();
    std::fs::write(dir.join("m/R.md"), "# recipe\n").unwrap();
    std::fs::write(
        dir.join("plugin.yaml"),
        format!(
            r#"id: demo
version: 0.1.0
footnote_compat: {{minimum: "0.1.0"}}
{extra}
mods:
  - id: table
    title: Table
    summary: a feed
    preview: {preview}
    recipe: m/R.md
    feed: {{argv: [fno, backlog, board, --json], columns: [{{key: /id, title: id}}]}}
  - id: tool
    title: Tool
    summary: runs code
    preview: m/p.png
    recipe: m/R.md
    run: [./tool.sh]
layouts:
  - id: desk
    title: Desk
    summary: two panes
    preview: m/p.png
    recipe: m/R.md
    tree: {{split: {{axis: Horizontal, children: [{{weight: 1, tree: {{slot: a}}}}, {{weight: 1, tree: {{slot: b}}}}]}}}}
    slots: [{{name: a, binding: anchor}}, {{name: b, binding: {{mod: table}}}}]
"#
        ),
    )
    .unwrap();
}

#[test]
fn a_mods_pack_verifies_and_an_escaping_preview_fails() {
    let tmp = tempfile::tempdir().unwrap();
    write_pack(tmp.path(), "", "m/p.png");
    let report = verify_pack(tmp.path(), &Default::default());
    let failing: Vec<_> = report.conditions.iter().filter(|c| !c.ok()).collect();
    assert!(report.ok(), "{failing:?}");
    let ids: Vec<_> = report.mods.iter().map(|m| m["id"].clone()).collect();
    assert_eq!(ids, ["table", "tool"]);
    assert_eq!(report.mods.get(1).expect("two mods")["runs_code"], true);

    let bad = tempfile::tempdir().unwrap();
    write_pack(bad.path(), "", "../outside.png");
    let report = verify_pack(bad.path(), &Default::default());
    let preview = report
        .conditions
        .iter()
        .find(|c| c.name == "preview:mod:table")
        .expect("preview condition");
    assert!(preview.checked && preview.result == "failed");
    assert!(preview
        .detail
        .as_deref()
        .unwrap()
        .contains("../outside.png"));
    assert!(!report.ok());

    // A feed starts on, so only a listed read with no writing flag passes.
    let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert!(super::ui::feed_is_read(&argv(&[
        "fno", "backlog", "board", "--json"
    ])));
    assert!(!super::ui::feed_is_read(&argv(&[
        "fno", "backlog", "done", "x-1"
    ])));
    assert!(!super::ui::feed_is_read(&argv(&[
        "fno",
        "backlog",
        "board",
        "--output=/x"
    ])));

    // A pack with no UI components hashes exactly as it did before them.
    let manifest = load_manifest(tmp.path()).unwrap();
    let plain = PackManifest {
        mods: Vec::new(),
        layouts: Vec::new(),
        ..manifest
    };
    assert_eq!(
        plain.digest(),
        crate::roles::models::canonical_digest("pack-manifest", &plain.to_json())
    );
}

#[test]
fn switches_default_by_kind_persist_and_an_unknown_pack_exits_2() {
    let tmp = tempfile::tempdir().unwrap();
    let pack = tmp.path().join("pack");
    write_pack(&pack, "", "m/p.png");
    let root = tmp.path().join("roles");
    // A registry written before switches existed still loads.
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join(".pack-registry.json"),
        r#"{"packs": [], "receipts": []}"#,
    )
    .unwrap();
    let store = Store::at_root(&root);
    let mut registry: Registry = store.load().unwrap();
    registry.install(&load_manifest(&pack).unwrap(), &pack.join("plugin.yaml"));
    store.save(&registry).unwrap();
    let loaded = store.load().unwrap();
    let installed = loaded.packs.first().expect("one pack");
    assert!(installed.enabled("table", false));
    assert!(!installed.enabled("tool", true), "code starts off");
    assert!(!registry.to_json()["packs"]
        .get(0)
        .expect("one pack")
        .as_object()
        .unwrap()
        .contains_key("toggles"));

    let args = |a: &[&str]| -> Vec<String> {
        ["--root", root.to_str().unwrap()]
            .iter()
            .chain(a)
            .map(|s| s.to_string())
            .collect()
    };
    assert_eq!(super::cli::run(&args(&["disable", "demo/table"])), 0);
    let reloaded = store.load().unwrap();
    assert!(!reloaded
        .packs
        .first()
        .expect("one pack")
        .enabled("table", false));
    assert_eq!(super::cli::run(&args(&["enable", "nope/x"])), 2);
    assert_eq!(super::cli::run(&args(&["enable", "demo/missing"])), 2);
}

fn git(dir: &Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-C"])
        .arg(dir)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

fn repo(dir: &Path, extra: &str) -> String {
    write_pack(dir, extra, "m/p.png");
    std::fs::write(dir.join("tool.sh"), "#!/bin/sh\necho tool\n").unwrap();
    git(dir, &["init", "-q"]);
    git(dir, &["add", "."]);
    git(dir, &["commit", "-qm", "pack"]);
    dir.to_string_lossy().into_owned()
}

#[test]
fn a_url_install_waits_for_consent_records_it_and_refuses_an_edited_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let url = repo(&tmp.path().join("src"), "");
    let root = tmp.path().join("roles");

    let waiting = install::install_with(&url, &root, |_| {
        Err(InstallError::AwaitingConsent {
            request_digest: "d".into(),
        })
    });
    assert!(matches!(waiting, Err(InstallError::AwaitingConsent { .. })));
    let left: Vec<_> = std::fs::read_dir(install::packs_dir(&root))
        .unwrap()
        .collect();
    assert!(left.is_empty(), "staging is cleared and nothing moved");
    assert!(Store::at_root(&root).load().unwrap().packs.is_empty());

    let (card, dest) = install::install_with(&url, &root, |card| {
        assert!(card
            .lines()
            .iter()
            .any(|l| l.contains("RUNS CODE: ./tool.sh")));
        Ok(Some("test".into()))
    })
    .unwrap();
    let pack = Store::at_root(&root)
        .load()
        .unwrap()
        .packs
        .first()
        .expect("one pack")
        .clone();
    let source = pack.source.clone().expect("source recorded");
    assert_eq!(source.content_digest, card.content_digest);
    assert_eq!(source.commit.len(), 40);
    assert_eq!(source.consented_by, "test");
    assert!(!pack.enabled("tool", true));
    install::launch_check(&root, "demo").unwrap();
    std::fs::write(dest.join("tool.sh"), "#!/bin/sh\necho changed\n").unwrap();
    assert!(matches!(
        install::launch_check(&root, "demo"),
        Err(InstallError::Refused(_))
    ));

    // Another URL never takes over an installed id; its staging is cleared.
    let other = repo(&tmp.path().join("other"), "");
    let taken = install::install_with(&other, &root, |_| Ok(Some("test".into())));
    assert!(matches!(taken, Err(InstallError::Refused(m)) if m.contains("already installed from")));
    let staged = std::fs::read_dir(install::packs_dir(&root))
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".staging")
        })
        .count();
    assert_eq!(staged, 0);

    let agent = tmp.path().join("agent");
    std::fs::create_dir_all(agent.join("s")).unwrap();
    std::fs::write(agent.join("s/SKILL.md"), "x\n").unwrap();
    let url = repo(&agent, "skills: [{id: s, source: s/SKILL.md}]");
    let refused = install::install_with(&url, &root, |_| panic!("no consent card"));
    assert!(matches!(refused, Err(InstallError::Refused(m)) if m.contains("skills")));
}

#[test]
fn the_gallery_merges_topics_and_a_failed_gh_read_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let stub = |name: &str, body: &str| {
        let path = tmp.path().join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path.to_string_lossy().into_owned()
    };
    let ok = stub(
        "gh-ok",
        r#"echo '{"items": [{"full_name": "a/one", "owner": {"login": "a"}, "stargazers_count": 3, "clone_url": "https://github.com/a/one.git"}, {"full_name": "b/two", "owner": {"login": "b"}, "clone_url": "https://github.com/b/two.git"}]}'"#,
    );
    let rows = super::gallery::read(&ok).unwrap();
    assert_eq!(rows.len(), 2, "one row per repo across the three topics");
    let first = rows.first().expect("a row");
    assert_eq!(first.name, "a/one");
    assert_eq!(
        first.install,
        "fno config plugins install https://github.com/a/one.git"
    );
    let bad = stub("gh-bad", "echo 'HTTP 403: rate limited' >&2; exit 1");
    let err = super::gallery::read(&bad).unwrap_err().to_string();
    assert!(err.contains("HTTP 403: rate limited"), "{err}");
}

#[test]
fn the_review_ledger_feed_draws_newest_first_as_a_table() {
    let events = [
        r#"{"ts": "2026-11-01T00:00:00Z", "type": "review_attestation", "data": {"branch": "feature/a", "head_sha": "aaaaaaaaaaaaaaaa", "verdict": "fail", "review_round": 1, "findings_blocking": 2}}"#,
        r#"{"ts": "2026-11-03T00:00:00Z", "type": "review_attestation", "data": {"branch": "feature/b", "head_sha": "bbbbbbbbbbbbbbbb", "verdict": "pass", "review_round": 2}}"#,
        r#"{"ts": "2026-11-02T00:00:00Z", "type": "review_attestation", "data": {"branch": "feature/a", "head_sha": "cccccccccccccccc", "verdict": "pass", "review_round": 2}}"#,
    ]
    .join("\n");
    let rows = crate::gate::review_summary::ledger(&events, 40);
    let manifest = load_manifest(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../plugins/fno-panels")
            .as_path(),
    )
    .expect("first-party pack loads");
    let ledger = manifest
        .mods
        .iter()
        .find(|m| m.id == "review-ledger")
        .and_then(|m| m.feed.as_ref())
        .expect("review-ledger is a feed mod");
    let lines = super::open::table(ledger, &rows);
    let line = |i: usize| lines.get(i).map(String::as_str).unwrap_or("");
    assert!(line(0).starts_with("when"), "{lines:?}");
    assert!(
        line(1).contains("feature/b") && line(1).contains("pass"),
        "{lines:?}"
    );
    assert!(
        line(2).contains("feature/a") && line(2).contains("cccccccccccc"),
        "{lines:?}"
    );
    assert_eq!(lines.len(), 3, "the newest row per branch only");
}
