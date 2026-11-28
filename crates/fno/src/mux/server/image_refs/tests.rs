#![allow(clippy::indexing_slicing, clippy::unwrap_used)]
use super::*;
use crate::clipboard::base64;
use crate::harness::Harness;
use crate::server::pasted_images::{PaneSession, Stores};
use crate::vt::{frame_text, Pane};

const SESSION: &str = "0d1e2f30-1111-4222-8333-944455556666";
const PER_MESSAGE: &str = "7a8b9c0d-1111-4222-8333-944455556666";

fn png(w: u32, h: u32) -> Vec<u8> {
    // Noise does not compress, so a large fixture stays past the wire cap.
    let img = image::RgbImage::from_fn(w, h, |x, y| {
        let v = x
            .wrapping_mul(2_654_435_761)
            .wrapping_add(y.wrapping_mul(3_266_489_917))
            .wrapping_mul(668_265_263);
        image::Rgb([(v >> 8) as u8, (v >> 16) as u8, (v >> 24) as u8])
    });
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
    stores: Stores,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        for sub in ["work/docs", "home/shots", "projects/p", "tmp", "cache"] {
            std::fs::create_dir_all(root.join(sub)).unwrap();
        }
        let stores = Stores {
            chat: crate::chat_events::ChatRoots {
                transcripts: vec![root.join("projects")],
                opencode_stores: vec![root.join("opencode.db")],
            },
            claude_tmp: vec![root.join("tmp")],
            cache: root.join("cache"),
        };
        Self {
            _dir: dir,
            root,
            stores,
        }
    }

    fn put(&self, rel: &str, bytes: &[u8]) -> PathBuf {
        let path = self.root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn rows(&self, session: &str, rows: &[serde_json::Value]) {
        let text: String = rows.iter().map(|row| format!("{row}\n")).collect();
        self.put(&format!("projects/p/{session}.jsonl"), text.as_bytes());
    }

    /// Click `needle` in a pane that printed `output`.
    fn click(
        &self,
        cols: u16,
        output: &str,
        needle: &str,
        session: Option<&PaneSession>,
    ) -> Option<Result<PathBuf, ImageError>> {
        let mut pane = Pane::new(14, cols);
        pane.feed(output.as_bytes());
        let text = frame_text(&pane.frame());
        let (row, col) = text
            .lines()
            .enumerate()
            .find_map(|(row, line)| {
                line.find(needle)
                    .map(|at| (row, line[..at].chars().count()))
            })
            .unwrap_or_else(|| panic!("{needle:?} is not on screen:\n{text}"));
        let window = pane.link_window(row as u16, col as u16)?;
        let found = find(
            &window,
            &self.root.join("work"),
            Some(&self.root.join("home")),
            "here.local",
            probe,
        )?;
        let session = match found.source {
            ImageSource::Pasted(_) => Some(session?),
            ImageSource::File(_) => None,
        };
        Some(resolve(&found.source, session, &self.stores))
    }
}

/// Output above an agent's composer, which holds the cursor.
fn printed(text: &str) -> String {
    format!("{text}\r\n\r\n────────────\r\n❯ ")
}

fn agent(harness: Harness, id: &str) -> PaneSession {
    PaneSession {
        harness,
        id: id.into(),
    }
}

#[test]
fn every_image_reference_form_opens() {
    let w = World::new();
    let small = png(12, 8);
    let shot = w.put("work/docs/shot.png", &small);
    let bare = w.put("work/bare.png", &small);
    let tilde = w.put("home/shots/t.png", &small);
    let spaced = w.put("work/docs/Screen Shot 2026.png", &small);
    let wrapped = w.put(
        "work/docs/a-very-long-screenshot-file-name-of-the-page.png",
        &small,
    );
    let stored = w.put(&format!("tmp/some-slug/{SESSION}/images/1.png"), &small);
    let large = png(520, 520);
    assert!(large.len() > crate::proto::chat_event::CHAT_IMAGE_INLINE_MAX);
    let queued = png(16, 9);
    w.rows(
        SESSION,
        &[
            serde_json::json!({"type": "user", "timestamp": "2020-01-02T10:00:00Z", "imagePasteIds": [2],
                "message": {"role": "user", "content": [
                    {"type": "text", "text": "compare [Image #2] with main"},
                    {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": base64(&large)}}]}}),
            serde_json::json!({"type": "attachment", "attachment": {"type": "queued_command", "imagePasteIds": [3],
                "prompt": [
                    {"type": "text", "text": "and this one [Image #3]"},
                    {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": base64(&queued)}}]}}),
        ],
    );

    let claude = agent(Harness::Claude, SESSION);
    let link = format!("\x1b]8;;file://{}\x07picture\x1b]8;;\x07", shot.display());
    let spaced_link = format!(
        "\x1b]8;;file://localhost{}\x07picture\x1b]8;;\x07",
        spaced.display().to_string().replace(' ', "%20")
    );
    // Claude hard wraps a long path and indents the rest.
    let hard_wrap = "⏺ saved docs/a-very-long-screenshot-\r\n  file-name-of-the-page.png";
    let names = |prefix: &str, count: usize| {
        let all: Vec<String> = (0..count).map(|i| format!("{prefix}{i}.png")).collect();
        all.join(",")
    };
    // Two wrapped lists: their joins alone would spend every probe.
    let dense = format!("{}\r\nbare.png,{}", names("a", 8), names("b", 7));
    let cases: Vec<(&str, u16, String, &str, Option<PathBuf>, (u32, u32))> = vec![
        (
            "plain name under a dense row",
            60,
            printed(&dense),
            "bare.png",
            Some(bare.clone()),
            (12, 8),
        ),
        (
            "at mention",
            300,
            printed("read @docs/shot.png"),
            "shot.png",
            Some(shot.clone()),
            (12, 8),
        ),
        (
            "claude placeholder before send",
            300,
            "> look [Image #1]".into(),
            "[Image",
            Some(stored),
            (12, 8),
        ),
        (
            "transcript fallback past the wire cap",
            300,
            printed("> compare [Image #2] with main"),
            "[Image",
            None,
            (520, 520),
        ),
        (
            "transcript fallback, queued",
            300,
            printed("> and this one [Image #3]"),
            "[Image",
            None,
            (16, 9),
        ),
        (
            "osc 8 file link",
            300,
            printed(&link),
            "picture",
            Some(shot.clone()),
            (12, 8),
        ),
        (
            "osc 8 escaped file link",
            300,
            printed(&spaced_link),
            "picture",
            Some(spaced.clone()),
            (12, 8),
        ),
        (
            "absolute",
            300,
            printed(&format!("wrote {}.", shot.display())),
            "/docs/",
            Some(shot.clone()),
            (12, 8),
        ),
        (
            "tilde",
            300,
            printed("wrote ~/shots/t.png"),
            "~/",
            Some(tilde),
            (12, 8),
        ),
        (
            "relative",
            300,
            printed("wrote docs/shot.png:"),
            "docs/",
            Some(shot),
            (12, 8),
        ),
        (
            "bare name",
            300,
            printed("wrote `bare.png`"),
            "bare.png",
            Some(bare),
            (12, 8),
        ),
        (
            "path with spaces",
            300,
            printed("wrote docs/Screen Shot 2026.png today"),
            "Shot",
            Some(spaced),
            (12, 8),
        ),
        (
            "hard wrap, first row",
            40,
            printed(hard_wrap),
            "docs/",
            Some(wrapped.clone()),
            (12, 8),
        ),
        (
            "hard wrap, next row",
            40,
            printed(hard_wrap),
            "file-name",
            Some(wrapped),
            (12, 8),
        ),
    ];
    for (form, cols, output, needle, want, size) in cases {
        let path = w
            .click(cols, &output, needle, Some(&claude))
            .unwrap_or_else(|| panic!("{form}: no link"))
            .unwrap_or_else(|e| panic!("{form}: {e}"));
        if let Some(want) = want {
            assert_eq!(path, want, "{form}");
        }
        assert_eq!(image::image_dimensions(&path).unwrap(), size, "{form}");

        // The rest of the road: the send gate, the client route and the router.
        let uri = crate::link::file_uri(&path, None);
        assert_eq!(
            crate::link::file_uri_parts(&uri),
            Some((path.clone(), None)),
            "{form}"
        );
        let target = crate::open_router::target(
            &path,
            None,
            Default::default(),
            &crate::digest_overlay::ObsidianCfg {
                enabled: false,
                vault: None,
            },
            Default::default(),
        );
        assert!(
            matches!(target, crate::open_router::Target::Viewer { .. }),
            "{form}"
        );
    }
}

#[test]
fn image_references_refuse_what_is_not_a_local_image() {
    let w = World::new();
    let bytes = png(12, 8);
    let shot = w.put("work/shot.png", &bytes);
    let script = w.put("work/x.command", &bytes);
    let fake = w.put("work/fake.png", b"#!/bin/sh\necho owned\n");
    std::os::unix::fs::symlink(&script, w.root.join("work/alias.png")).unwrap();
    std::fs::create_dir(w.root.join("work/dir.png")).unwrap();
    let fifo = std::ffi::CString::new(w.root.join("work/pipe.png").to_str().unwrap()).unwrap();
    // SAFETY: `fifo` is a NUL-terminated path that outlives the call.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);

    let link = |uri: String| printed(&format!("\x1b]8;;{uri}\x07picture\x1b]8;;\x07"));
    let plain = [
        (
            "not an image extension",
            link(format!("file://{}", script.display())),
            "picture",
        ),
        (
            "another host",
            link(format!("file://there{}", shot.display())),
            "picture",
        ),
        (
            "not a file url",
            link("https://example.com/shot.png".into()),
            "picture",
        ),
        ("symlink to a script", printed("see alias.png"), "alias"),
        ("directory", printed("see dir.png"), "dir.png"),
        ("fifo", printed("see pipe.png"), "pipe"),
        ("missing", printed("see gone.png"), "gone"),
        ("longer extension", printed("see shot.png.bak"), "shot"),
    ];
    for (case, output, needle) in plain {
        assert!(w.click(300, &output, needle, None).is_none(), "{case}");
    }

    // The right extension and the wrong bytes spans, and opens nothing.
    let output = link(format!("file://{}", fake.display()));
    assert!(matches!(
        w.click(300, &output, "picture", None),
        Some(Err(ImageError::NotAnImage { .. }))
    ));
    assert!(matches!(
        w.click(300, &printed("see fake.png"), "fake", None),
        Some(Err(ImageError::NotAnImage { .. }))
    ));
}

#[test]
fn a_pasted_placeholder_opens_only_its_own_turns_image() {
    let w = World::new();
    let (first, second, third) = (png(11, 7), png(13, 7), png(17, 7));
    let claude_row = |text: &str, ids: &[u64], images: &[&Vec<u8>]| {
        let mut content = vec![serde_json::json!({"type": "text", "text": text})];
        for bytes in images {
            content.push(serde_json::json!({"type": "image",
                "source": {"type": "base64", "media_type": "image/png", "data": base64(bytes)}}));
        }
        serde_json::json!({"type": "user", "timestamp": "2020-01-02T10:00:00Z",
            "imagePasteIds": ids, "message": {"role": "user", "content": content}})
    };
    w.rows(
        SESSION,
        &[
            claude_row("old [Image #4]", &[4], &[&first]),
            // Images follow the ids, not the markers.
            claude_row(
                "new [Image #5] then [Image #4]",
                &[4, 5],
                &[&second, &third],
            ),
        ],
    );
    // The reused number replaced the stored file after both turns.
    w.put(&format!("tmp/slug/{SESSION}/images/4.png"), &png(19, 7));
    let codex_item = |text: &str, bytes: &Vec<u8>| {
        serde_json::json!({"type": "response_item", "timestamp": "2020-01-02T10:00:00Z", "payload": {
            "type": "message", "role": "user", "content": [
                {"type": "input_text", "text": "<image name=[Image #1] path=\"/tmp/a.png\">"},
                {"type": "input_image", "image_url": format!("data:image/png;base64,{}", base64(bytes))},
                {"type": "input_text", "text": "</image>"},
                {"type": "input_text", "text": text}]}})
    };
    w.rows(
        PER_MESSAGE,
        &[
            codex_item("alpha [Image #1]", &first),
            codex_item("beta [Image #1]", &second),
        ],
    );
    let db = rusqlite::Connection::open(w.root.join("opencode.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE session (id TEXT PRIMARY KEY, parent_id TEXT);
         CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);
         CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);
         INSERT INTO session VALUES ('ses_a', NULL);
         INSERT INTO message VALUES ('msg_1', 'ses_a', 1, 1, '{\"role\":\"user\"}');",
    )
    .unwrap();
    for (id, data) in [
        (
            "prt_01",
            serde_json::json!({"type": "text", "text": "gamma [Image 1]"}),
        ),
        (
            "prt_02",
            serde_json::json!({"type": "file", "mime": "image/png",
                "url": format!("data:image/png;base64,{}", base64(&third))}),
        ),
    ] {
        db.execute(
            "INSERT INTO part VALUES (?1, 'msg_1', 'ses_a', 10, 10, ?2)",
            rusqlite::params![id, data.to_string()],
        )
        .unwrap();
    }
    drop(db);

    let claude = agent(Harness::Claude, SESSION);
    let codex = agent(Harness::Codex, PER_MESSAGE);
    let opencode = agent(Harness::Opencode, "ses_a");
    let width = |session: &PaneSession, output: &str| {
        w.click(300, output, "[Image", Some(session))
            .expect("a placeholder spans")
            .map(|path| image::image_dimensions(path).unwrap().0)
    };
    assert_eq!(width(&claude, &printed("> old [Image #4]")).unwrap(), 11);
    assert_eq!(
        width(&claude, &printed("> new [Image #5] then")).unwrap(),
        17
    );
    assert_eq!(width(&codex, &printed("› beta [Image #1]")).unwrap(), 13);
    assert_eq!(width(&opencode, &printed("gamma [Image 1]")).unwrap(), 17);

    assert!(
        matches!(
            width(&claude, &printed("> [Image #4]")),
            Err(ImageError::NoMatch { .. })
        ),
        "two turns share the number and the row names neither"
    );
    assert!(
        matches!(
            width(&codex, "› [Image #1]"),
            Err(ImageError::NotSent { .. })
        ),
        "the composer never shows an older turn's image"
    );
    // The cursor is three rows and a blank line under the marker.
    assert!(
        matches!(
            width(&codex, "› [Image #1]\r\n  two\r\n\r\n  four"),
            Err(ImageError::NotSent { .. })
        ),
        "a draft of many lines is still the composer"
    );
    assert!(
        matches!(
            width(&codex, "› [Image #1]\r\n  › pasted\r\n  three"),
            Err(ImageError::NotSent { .. })
        ),
        "an indented prompt glyph is draft text, not an edge"
    );
    const OTHER: &str = "ffffffff-1111-4222-8333-944455556666";
    let other = agent(Harness::Claude, OTHER);
    w.put(&format!("tmp/slug/{OTHER}/images/7.png"), &png(21, 7));
    assert!(
        matches!(
            width(&other, &printed("quoted: look at [Image #7]")),
            Err(ImageError::NoMatch { .. })
        ),
        "only the draft takes the store without a turn"
    );
    assert_eq!(
        width(&other, &printed("> old [Image #4]"))
            .unwrap_err()
            .to_string(),
        "image #4 is no longer stored"
    );
}

#[test]
fn a_pane_answers_only_for_the_session_its_child_shows() {
    let mut core = crate::mux::server::tests::empty_core();
    let row = |attach: &str, session: &str| crate::agents_view::RegistryAgent {
        name: attach.into(),
        harness: Some("claude".into()),
        attach_id: Some(attach.into()),
        harness_session_id: Some(session.into()),
        ..Default::default()
    };
    core.agents = vec![row("aaaa1111", SESSION), row("bbbb2222", PER_MESSAGE)];
    core.attached.insert("aaaa1111".into(), 5);
    let mut hosted = row("cccc3333", "hosted");
    hosted.mux = Some((core.session_name.clone(), 6));
    core.agents.push(hosted);
    let shown = |pane: u64, argv: &[&str]| {
        let argv = argv.iter().map(|arg| arg.to_string()).collect();
        core.pane_session_with(pane, Some(argv))
            .map(|session| session.id)
    };
    for (pane, argv, want) in [
        (5, &["claude", "attach", "aaaa1111"][..], Some(SESSION)),
        (5, &["claude", "agents"][..], None),
        (5, &["claude", "attach", "bbbb2222"][..], Some(PER_MESSAGE)),
        (9, &["claude", "attach", "bbbb2222"][..], Some(PER_MESSAGE)),
        (9, &["/bin/zsh"][..], None),
        (6, &["claude"][..], Some("hosted")),
        (6, &["/opt/bin/claude", "agents"][..], None),
    ] {
        assert_eq!(shown(pane, argv).as_deref(), want, "pane {pane}: {argv:?}");
    }
}
