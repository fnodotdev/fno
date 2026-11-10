//! The composer's real-client contract: one test per reported
//! defect, driven on a real `fno` client over a PTY with a `cat -v` pane as
//! the leak detector. Every test asserts the FIXED behavior, so each one is
//! red on the branchpoint (AC0-REPRO) and green only once the composer owns
//! its input, shows its values, and opens as a draft card inside its
//! workspace's group. Lists unfold inside the card, no popovers.

mod common;
use common::{ClientHarness, Scratch};

use std::path::PathBuf;
use std::time::Duration;

const PREFIX: &[u8] = b"\x02";
const OPEN: &[u8] = b"i"; // prefix+i: toggle-composer
const DOWN: &[u8] = b"\x1b[B";
const UP: &[u8] = b"\x1b[A";

fn type_and_settle(h: &mut ClientHarness, bytes: &[u8]) {
    h.type_bytes(bytes);
    std::thread::sleep(Duration::from_millis(120));
}

fn wait_input(h: &mut ClientHarness) {
    h.wait_prompt(15);
}

/// A configured account row the isolated home exposes to the model tab.
fn seed_routing_config(scratch: &Scratch) {
    let dir = scratch.0.join("home").join(".fno");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("config.toml"),
        "[accounts]\n\
         active = \"zai-main\"\n\
         [[accounts.records]]\n\
         id = \"zai-main\"\n\
         harness = \"claude\"\n\
         route_provider_id = \"zai\"\n\
         model_name = \"glm-5.3-flash[1m]\"\n\
         route = \"zai/glm-5.3-flash[1m]\"\n",
    )
    .unwrap();
}

/// Fake `claude`/`codex` bins on the client's PATH: the harness catalog's
/// installed flag is a PATH presence check, and a clean CI home has neither
/// binary. Presence is all the catalog reads.
fn with_fake_harnesses(scratch: &Scratch) -> Vec<(&'static str, String)> {
    let bin = scratch.0.join("fakebin");
    std::fs::create_dir_all(&bin).unwrap();
    for name in ["claude", "codex"] {
        let path = bin.join(name);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = std::env::var("PATH").unwrap_or_default();
    let augmented = format!("{}:{}", bin.display(), path);
    vec![("PATH", augmented)]
}

fn open_composer(h: &mut ClientHarness) {
    type_and_settle(h, PREFIX);
    type_and_settle(h, OPEN);
}

/// A harness picker row paints its bare name, with a `•` only on rows other
/// than the selected one (the band marks the selection). A chip line such as
/// `harness claude` never equals the bare name, so it never matches.
fn harness_row(s: &str, name: &str) -> bool {
    // A screen line also carries the sideline and pane borders, so test each
    // `│`-separated cell.
    s.lines().flat_map(|l| l.split('\u{2502}')).any(|cell| {
        cell.trim()
            .trim_start_matches(['\u{2713}', '\u{2022}', ' '])
            == name
    })
}

/// Pick Claude through the harness chip's picker, then open the model
/// picker. Chip-row grammar: Enter on a chip opens its picker, typing
/// filters, Enter commits and closes.
fn pick_claude_and_open_model_picker(h: &mut ClientHarness) {
    // A fresh open focuses the input; two Tabs walk Message ->
    // Permission -> Harness.
    type_and_settle(h, b"\t\t");
    type_and_settle(h, b"\r");
    // The picker lists the catalog's selectable (installed) harnesses once
    // the read lands. A clean CI home has only the fake bins, so the draft
    // default may have no row at all: wait for installed rows, under either
    // glyph.
    h.wait_screen(35, |s| harness_row(s, "claude") && harness_row(s, "codex"));
    type_and_settle(h, b"claude");
    // codex vanishing proves the query narrowed the picker; claude alone
    // stays. Neither side of this test leans on the draft default.
    h.wait_screen(35, |s| harness_row(s, "claude") && !harness_row(s, "codex"));
    type_and_settle(h, b"\r");
    // The committed chip reads `claude` with its caret padding; the filter
    // title never does.
    h.wait_screen(35, |s| s.contains("claude \u{b7} model"));
    // One Tab lands on the Model chip; Enter drops its picker.
    type_and_settle(h, b"\t");
    type_and_settle(h, b"\r");
    h.wait_screen(35, |s| s.contains("default (claude"));
}

fn pane_region(screen: &str) -> String {
    // The content area right of the 28-column sideline: where a leaked byte
    // would print. Tests assert on absence, so an over-wide region is safe.
    screen
        .lines()
        .map(|l| l.chars().skip(28).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn composer_from_sidebar_opens_the_card_with_full_values() {
    // AC1-HP: from the regular sidebar the composer is the draft card
    // with its summary lines: the Where token reads Local, the input asks the
    // working question, and the harness token carries its value untruncated.
    let scratch = Scratch::new("composer-sheet");
    seed_routing_config(&scratch);
    let envs = with_fake_harnesses(&scratch);
    let env_refs: Vec<(&str, &str)> = envs.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let mut h = ClientHarness::spawn_sized_with(&scratch, 40, 120, &env_refs);
    wait_input(&mut h);
    open_composer(&mut h);
    let screen = h.wait_screen(10, |s| {
        s.contains("[start]") && s.contains("prompt \u{b7} +flag")
    });
    assert!(
        screen.contains("[start]"),
        "the card's buttons paint: {screen}"
    );
    // At rest the card is the prompt and the command line; the chips wait
    // behind Tab.
    assert!(
        screen.contains("prompt \u{b7} +flag"),
        "the placeholder paints: {screen}"
    );
    assert!(
        !screen.contains("[less]"),
        "the chip rows stay folded at rest: {screen}"
    );
    // Tab unfolds the chips. The harness token's value lands when the
    // catalog read does; the first-run preselect is claude, per the
    // harness-preselect ruling.
    type_and_settle(&mut h, b"\t");
    let screen = h.wait_screen(35, |s| s.contains("claude \u{b7} model"));
    assert!(
        screen.contains("auto"),
        "the unfolded card paints the permission token: {screen}"
    );
    assert!(
        screen.contains("claude \u{b7} model"),
        "the harness token shows its value: {screen}"
    );
    // The tab strip is gone: no axis names paint in the content area (the
    // sideline's own strip row legitimately reads "Messages").
    let content = pane_region(&screen);
    for tab in ["Harness", "Flags", "Message"] {
        assert!(!content.contains(tab), "no tab label {tab}: {screen}");
    }
}

#[test]
fn project_picker_lists_projects_and_enter_never_launches() {
    // AC2-HP: Enter on the Project chip drops its picker; Enter on a row
    // sets the project without launching.
    let scratch = Scratch::new("composer-project");
    let mut h = ClientHarness::spawn_sized(&scratch, 24, 120);
    wait_input(&mut h);
    open_composer(&mut h);
    // Wait out the facts probe so the Branch/Worktree chips (painted while
    // facts are unread) no longer sit between Project and the input; this
    // scratch is not a git repo, so the pair hides once the probe lands.
    type_and_settle(&mut h, b"\t");
    h.wait_screen(35, |s| s.contains("auto") && !s.contains("worktree"));
    type_and_settle(&mut h, b"\x1b[Z");
    // Shift-Tab from the input lands on the Directory chip; Enter drops its
    // picker (title `directory`).
    type_and_settle(&mut h, b"\x1b[Z");
    type_and_settle(&mut h, b"\r");
    let screen = h.wait_screen(35, |s| s.contains("directory"));
    assert!(
        !screen.contains("starting..."),
        "opening the project picker never launches: {screen}"
    );
}

#[test]
fn agent_list_offers_default_rows_and_no_free_text_model_row() {
    // AC5-HP / open question 2: every launchable model comes from a
    // configured account row; the Model tab carries the harness default and
    // no "type a model..." free-text entry, and a typed query can never
    // become the value - it only narrows the body.
    let scratch = Scratch::new("composer-agent-list");
    seed_routing_config(&scratch);
    let envs = with_fake_harnesses(&scratch);
    let env_refs: Vec<(&str, &str)> = envs.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let mut h = ClientHarness::spawn_sized_with(&scratch, 40, 120, &env_refs);
    wait_input(&mut h);
    open_composer(&mut h);
    pick_claude_and_open_model_picker(&mut h);
    let screen = h.wait_screen(35, |s| s.contains("default (claude"));
    assert!(
        screen.contains("default (claude"),
        "the model body names the harness default: {screen}"
    );
    assert!(
        !screen.contains("type a model"),
        "no free-text model row: {screen}"
    );
    // A typed query narrows the body in place (the rows vanish) and never
    // lands in the values strip.
    type_and_settle(&mut h, b"fddd");
    let screen = h.wait_screen(10, |s| !s.contains("default (claude"));
    assert!(
        !screen.contains("default (claude"),
        "the junk query narrows the rows away: {screen}"
    );
    assert!(
        !screen.contains("model fddd"),
        "junk never becomes a value: {screen}"
    );
}

#[test]
fn focus_report_then_arrow_never_lands_as_text() {
    // AC6-EDGE: an unknown CSI (a focus report ESC [ I) is dropped whole; a
    // following arrow still navigates and no `[`/`I` byte reaches a field.
    let scratch = Scratch::new("composer-focus-in");
    let mut h = ClientHarness::spawn_sized(&scratch, 24, 120);
    wait_input(&mut h);
    open_composer(&mut h);
    // The input already holds focus; a focus report is dropped whole and a
    // following arrow still navigates (cursor move, never a launch).
    type_and_settle(&mut h, b"\x1b[I");
    type_and_settle(&mut h, DOWN);
    std::thread::sleep(Duration::from_millis(300));
    let screen = h.screen();
    assert!(
        !screen.contains("[B") && !screen.contains("[I"),
        "no sequence byte lands in a field: {screen}"
    );
}

#[test]
fn arrows_inside_the_repeat_window_type_letters_not_resizes() {
    // AC6-HP, keys half: a bare H/J/K/L typed into the composer inside a
    // resize repeat window is text for the draft, never a pane resize. The
    // composer stays open and the pane's columns never change (the prompt
    // does not redraw mid-line).
    let scratch = Scratch::new("composer-repeat");
    let mut h = ClientHarness::spawn_sized(&scratch, 24, 120);
    wait_input(&mut h);
    // Arm the resize window: prefix+L.
    type_and_settle(&mut h, PREFIX);
    type_and_settle(&mut h, b"L");
    open_composer(&mut h);
    // The input holds focus; holding L inside the window is draft text.
    type_and_settle(&mut h, b"L");
    std::thread::sleep(Duration::from_millis(300));
    let screen = h.screen();
    assert!(
        screen.contains("L"),
        "the letter lands in the draft: {screen}"
    );
}

#[test]
fn project_chip_focus_shows_the_working_directory_line() {
    // AC3-HP: focusing the Project token paints `working directory` beside
    // the path.
    let scratch = Scratch::new("composer-cwd-line");
    let mut h = ClientHarness::spawn_sized(&scratch, 24, 120);
    wait_input(&mut h);
    open_composer(&mut h);
    // While the facts probe is in flight the Branch/Worktree chips paint
    // between Project and the input and would eat the BackTab; this scratch
    // is not a git repo, so the pair hides once the probe lands.
    h.wait_screen(35, |s| !s.contains("worktree"));
    type_and_settle(&mut h, b"\x1b[Z"); // Project chip
    let screen = h.wait_screen(35, |s| s.contains("working directory"));
    assert!(
        screen.contains("working directory"),
        "the cwd line paints: {screen}"
    );
}

#[test]
fn where_chip_opens_the_local_and_placement_picker() {
    // AC4-HP: the Where picker lists Local under `Run on` and the four
    // placement rows under `Open as` - no Cloud, Remote Control or SSH row.
    let scratch = Scratch::new("composer-where");
    let mut h = ClientHarness::spawn_sized(&scratch, 40, 120);
    wait_input(&mut h);
    open_composer(&mut h);
    // Wait out the facts probe so the Branch/Worktree chips (painted while
    // facts are unread) no longer sit between Project and the input; this
    // scratch is not a git repo, so the pair hides once the probe lands.
    type_and_settle(&mut h, b"\t");
    h.wait_screen(35, |s| s.contains("auto") && !s.contains("worktree"));
    type_and_settle(&mut h, b"\x1b[Z");
    // BackTab BackTab walks Message -> Project -> Where.
    type_and_settle(&mut h, b"\x1b[Z\x1b[Z");
    type_and_settle(&mut h, b"\r");
    let screen = h.wait_screen(10, |s| s.contains("Run on"));
    for want in ["Run on", "Local", "Open as", "thread", "pane: active tab"] {
        assert!(
            screen.contains(want),
            "the Where picker lists {want}: {screen}"
        );
    }
    assert!(
        !screen.contains("Cloud") && !screen.contains("SSH"),
        "no substrate rows are invented: {screen}"
    );
}

#[test]
fn composer_keybar_names_the_keys() {
    // AC8-HP: the keybar names the chip-row grammar: Enter launches from
    // the prompt line (or names the chip it opens), Tab moves, ^j is a
    // newline in the input, esc closes.
    let scratch = Scratch::new("composer-hint");
    let mut h = ClientHarness::spawn_sized(&scratch, 24, 120);
    wait_input(&mut h);
    open_composer(&mut h);
    // The key line unfolds with the chip rows, so Tab shows it.
    type_and_settle(&mut h, b"\t");
    let screen = h.wait_screen(10, |s| s.contains("tab next"));
    assert!(
        screen.contains("\u{21b5} open"),
        "enter names the chip it opens: {screen}"
    );
    assert!(screen.contains("tab next"), "tab is named: {screen}");
    assert!(screen.contains("esc close"), "esc is named: {screen}");
}

#[test]
fn short_terminal_refuses_the_card_with_a_notice() {
    // AC1-ERR: a terminal shorter than 12 rows opens nothing and names why.
    let scratch = Scratch::new("composer-short");
    let mut h = ClientHarness::spawn_sized(&scratch, 10, 120);
    wait_input(&mut h);
    open_composer(&mut h);
    let screen = h.wait_screen(10, |s| s.contains("terminal too short"));
    assert!(
        screen.contains("terminal too short for the composer"),
        "the refusal names the limit: {screen}"
    );
    assert!(!screen.contains("[start]"), "nothing opened: {screen}");
}

#[test]
fn a_terminal_too_narrow_for_the_sideline_refuses_the_card_with_a_notice() {
    // AC9-EDGE: 60 columns leave no room for the sideline the card lives
    // in; nothing opens, so no draft takes the keyboard unseen.
    let scratch = Scratch::new("composer-narrow");
    let mut h = ClientHarness::spawn(&scratch); // 24x60: panel hidden
    wait_input(&mut h);
    open_composer(&mut h);
    let screen = h.wait_screen(10, |s| s.contains("terminal too narrow"));
    assert!(
        screen.contains("terminal too narrow for the composer"),
        "the refusal names the limit: {screen}"
    );
    assert!(!screen.contains("[start]"), "nothing opened: {screen}");
}

#[test]
fn prefix_hint_bar_shows_at_once() {
    // AC8-EDGE: the short hint bar paints the moment the prefix is pending.
    let scratch = Scratch::new("composer-prefix-hint");
    let mut h = ClientHarness::spawn_sized(&scratch, 24, 120);
    wait_input(&mut h);
    type_and_settle(&mut h, PREFIX);
    let screen = h.wait_screen(10, |s| s.contains("esc cancel"));
    assert!(
        screen.contains("all keys"),
        "the bar sends the reader to ?: {screen}"
    );
}

#[test]
fn agent_list_shows_route_hint_for_a_routing_row() {
    // AC4-HP: the model tab uses the configured account row and shows its
    // route as the hint, so a glm launch is reachable from the composer.
    // The door under the list is `fno config get accounts.records`, which
    // shells to the installed python CLI; the CI mux job deliberately ships
    // none, and a cold install pays the CLI's bootstrap inside the client's
    // 30s read bound. So the body answers one of two contract surfaces: the
    // configured row with its route hint when the door lands, or the named
    // unavailable row - whose LABEL the tab body never ellipsizes - beside
    // the standing default when it does not. The unit suite pins the row
    // and hint rendering either way.
    let scratch = Scratch::new("composer-route-hint");
    seed_routing_config(&scratch);
    let envs = with_fake_harnesses(&scratch);
    let env_refs: Vec<(&str, &str)> = envs.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let mut h = ClientHarness::spawn_sized_with(&scratch, 40, 120, &env_refs);
    wait_input(&mut h);
    open_composer(&mut h);
    pick_claude_and_open_model_picker(&mut h);
    // The door's read bound is 30s; 35s covers it plus render.
    let screen = h.wait_screen(35, |s| {
        (s.contains("glm-5.3-flash[1m]") && s.contains("zai/glm-5.3-flash"))
            || (s.contains("model list unavailable") && s.contains("default (claude"))
    });
    assert!(
        screen.contains("default (claude"),
        "the default row stands under either door answer: {screen}"
    );
    assert!(
        screen.contains("glm-5.3-flash[1m]") || screen.contains("model list unavailable"),
        "a configured row or the named unavailable surface: {screen}"
    );
    // Escape closes the open model list, then the card itself; the draft
    // is retained.
    type_and_settle(&mut h, b"\x1b");
    type_and_settle(&mut h, b"\x1b");
    let screen = h.wait_screen(10, |s| !s.contains("[start]"));
    assert!(!screen.contains("[start]"), "the composer closes: {screen}");
}

#[test]
fn arrows_in_the_model_body_move_and_up_never_launches() {
    // AC3-HP, list grammar: Up/Down move inside the body; nothing in the
    // body launches. Up from the first row stays put; the pane still prints
    // nothing.
    let scratch = Scratch::new("composer-list-nav");
    seed_routing_config(&scratch);
    let envs = with_fake_harnesses(&scratch);
    let env_refs: Vec<(&str, &str)> = envs.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let mut h = ClientHarness::spawn_sized_with(&scratch, 40, 120, &env_refs);
    wait_input(&mut h);
    open_composer(&mut h);
    // Three tabs land on the Model chip; Enter drops its picker.
    type_and_settle(&mut h, b"\t\t\t");
    type_and_settle(&mut h, b"\r");
    std::thread::sleep(Duration::from_millis(400));
    type_and_settle(&mut h, UP);
    type_and_settle(&mut h, DOWN);
    type_and_settle(&mut h, DOWN);
    std::thread::sleep(Duration::from_millis(300));
    let screen = h.screen();
    assert!(
        !screen.contains("starting..."),
        "arrow navigation never launches: {screen}"
    );
    let pane = pane_region(&screen);
    assert!(
        !pane.contains("^[[B"),
        "arrows never reach the pane: {pane}"
    );
}

/// The regression behind the model-floor contract: with claude chosen, the
/// Model tab listed
/// only "harness default" because no account record pinned a model. Each
/// harness now floors its list off the capability table; configured rows
/// merge over. One test per harness: the launcher retains focus across a
/// close/reopen, so each composer runs from a fresh client.
#[test]
fn claude_model_tab_lists_the_claude_families() {
    let scratch = Scratch::new("composer-model-floor");
    seed_routing_config(&scratch);
    let envs = with_fake_harnesses(&scratch);
    let env_refs: Vec<(&str, &str)> = envs.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let mut h = ClientHarness::spawn_sized_with(&scratch, 40, 120, &env_refs);
    wait_input(&mut h);
    open_composer(&mut h);
    pick_claude_and_open_model_picker(&mut h);
    // The floor lands with the catalog read; the accounts door may take up
    // to 30s in CI, so wait like the route-hint test does.
    let screen = h.wait_screen(35, |s| {
        s.contains("default (claude")
            && s.contains("opus")
            && s.contains("sonnet")
            && s.contains("haiku")
    });
    for want in ["default (claude", "opus", "sonnet", "haiku"] {
        assert!(screen.contains(want), "claude floor lists {want}: {screen}");
    }
    // The card unfolds ten picker rows and scrolls past them, so the last
    // family comes into view through the filter.
    type_and_settle(&mut h, b"fable");
    let screen = h.wait_screen(10, |s| s.contains("fable"));
    assert!(
        screen.contains("fable"),
        "claude floor lists fable: {screen}"
    );
}

#[test]
fn codex_model_tab_lists_the_codex_slugs() {
    let scratch = Scratch::new("composer-model-floor-codex");
    seed_routing_config(&scratch);
    let envs = with_fake_harnesses(&scratch);
    let env_refs: Vec<(&str, &str)> = envs.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let mut h = ClientHarness::spawn_sized_with(&scratch, 40, 120, &env_refs);
    wait_input(&mut h);
    open_composer(&mut h);
    // Narrow the harness picker to codex, commit it, then Tab to the Model
    // chip and drop its picker. Same grammar the claude helper exercises.
    type_and_settle(&mut h, b"\t\t");
    type_and_settle(&mut h, b"\r");
    h.wait_screen(35, |s| harness_row(s, "claude") && harness_row(s, "codex"));
    type_and_settle(&mut h, b"codex");
    h.wait_screen(35, |s| harness_row(s, "codex") && !harness_row(s, "claude"));
    type_and_settle(&mut h, b"\r");
    h.wait_screen(35, |s| s.contains("codex \u{b7} model"));
    type_and_settle(&mut h, b"\t");
    type_and_settle(&mut h, b"\r");
    let screen = h.wait_screen(35, |s| {
        s.contains("default (codex") && s.contains("gpt-6-luna")
    });
    for want in ["gpt-6-luna", "gpt-6-astra", "gpt-5.6-luna"] {
        assert!(screen.contains(want), "codex floor lists {want}: {screen}");
    }
}

#[allow(dead_code)]
fn unused_path_helper(_: PathBuf) {}
#[test]
fn model_picker_shows_the_flagship_and_the_seeded_provider_group() {
    // AC5-HP / AC7-HP preamble: a deepseek provider record and a fresh
    // models.dev cache land in the scratch home, so the claude Model
    // picker paints the flagship row under `harness default` and a
    // deepseek group without a network fetch. The CI mux job ships no
    // Python CLI, so the config reads may not land; the assertion accepts
    // the named unavailable surface (the two-surface rule).
    let scratch = Scratch::new("composer-model-picker-rows");
    let dir = scratch.0.join("home").join(".fno");
    std::fs::create_dir_all(dir.join("cache")).unwrap();
    std::fs::write(
        dir.join("config.toml"),
        "[model_routing.providers.deepseek]\n\
         protocol = \"anthropic\"\n\
         base_url = \"https://api.deepseek.com/anthropic\"\n\
         api_key_env = \"FNO_TEST_DS_KEY\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("cache").join("models-dev.json"),
        r#"{"deepseek":{"name":"DeepSeek","env":["DEEPSEEK_API_KEY"],"api":"https://api.deepseek.com","npm":"@ai-sdk/anthropic","models":{"deepseek-chat":{"id":"deepseek-chat","name":"DeepSeek V3"}}}}"#,
    )
    .unwrap();
    let mut envs = with_fake_harnesses(&scratch);
    envs.push(("FNO_TEST_DS_KEY", "x".to_string()));
    let env_refs: Vec<(&str, &str)> = envs.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let mut h = ClientHarness::spawn_sized_with(&scratch, 40, 120, &env_refs);
    wait_input(&mut h);
    open_composer(&mut h);
    pick_claude_and_open_model_picker(&mut h);
    let screen = h.wait_screen(35, |s| s.contains("default (claude"));
    if screen.contains("model list unavailable") {
        return; // the config reads never landed; the notice is the surface
    }
    assert!(
        screen.contains("deepseek"),
        "the seeded provider group renders: {screen}"
    );
    assert!(
        screen.contains("flagship"),
        "the flagship row paints under the default: {screen}"
    );
    assert!(
        screen
            .lines()
            .any(|l| l.contains("flagship") && !l.contains("opus")),
        "the flagship row has its own group header: {screen}"
    );
    assert!(
        screen.contains("opus"),
        "the flagship names the floor lead: {screen}"
    );
}

#[test]
fn flag_prefixes_open_the_picker_and_a_pick_becomes_a_pill() {
    for (index, prefix) in ["--", "—", "+"].into_iter().enumerate() {
        let scratch = Scratch::new(&format!("composer-flags-{index}"));
        let mut h = ClientHarness::spawn_sized(&scratch, 40, 120);
        wait_input(&mut h);
        open_composer(&mut h);
        type_and_settle(&mut h, prefix.as_bytes());
        let screen = h.wait_screen(35, |s| s.contains("--agent"));
        assert!(
            screen.contains("--agent"),
            "the flags picker lists the captured flags: {screen}"
        );
        // Narrow to --agent and pick it; the pill paints with its value slot.
        type_and_settle(&mut h, b"agent");
        type_and_settle(&mut h, b"\r");
        let screen = h.wait_screen(10, |s| s.contains("--agent <value>"));
        assert!(
            screen.contains("--agent <value>"),
            "the pill paints with its value slot: {screen}"
        );
    }
}
