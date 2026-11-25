//! The onboarding conversation as a pure module: nineteen screens in a
//! fixed order, three presets as per-screen stop flags, and a proposal
//! list the terminal host applies. No terminal, no subprocess and no
//! clock live here - a scripted prompter drives the same flow in tests,
//! and the terminal host in `wizard_term` supplies the real one.

use std::collections::{BTreeMap, VecDeque};

use crate::setup_autowire::HARNESS_LIST;
use crate::setup_run::Proposal;

/// One of the three onboarding postures. `slot` indexes the per-screen
/// stop table: the wizard only asks where the posture says to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    Aggressive,
    Balanced,
    UserReviewed,
}

impl Preset {
    pub fn slot(self) -> usize {
        match self {
            Preset::Aggressive => 0,
            Preset::Balanced => 1,
            Preset::UserReviewed => 2,
        }
    }

    fn from_slot(slot: usize) -> Preset {
        match slot {
            0 => Preset::Aggressive,
            2 => Preset::UserReviewed,
            _ => Preset::Balanced,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Preset::Aggressive => "aggressive",
            Preset::Balanced => "balanced",
            Preset::UserReviewed => "user-reviewed",
        }
    }
}

/// One harness found on this machine, and how.
#[derive(Debug, Clone)]
pub struct Found {
    pub name: &'static str,
    pub by: &'static str,
}

/// What the host measured before the flow starts. `env_keys` pairs a
/// harness name with the env var name that carries its key - never the
/// key itself.
#[derive(Debug, Clone)]
pub struct Detect {
    pub markers_present: bool,
    /// The machine's live `auto_merge.enabled`, when its config sets one.
    /// The screens preselect it, so finishing the wizard never flips a
    /// grant the person already made.
    pub auto_merge: Option<bool>,
    pub band: Option<i64>,
    pub harnesses: Vec<Found>,
    pub env_keys: Vec<(&'static str, &'static str)>,
    pub models: Vec<String>,
}

impl Detect {
    /// The empty fallback for a host that could not measure anything.
    pub fn fresh() -> Detect {
        Detect {
            markers_present: false,
            auto_merge: None,
            band: None,
            harnesses: Vec::new(),
            env_keys: Vec::new(),
            models: Vec::new(),
        }
    }
}

/// Whether a screen runs for the machine at hand.
#[derive(Debug, Clone, Copy)]
pub enum When {
    Always,
    FreshAndUnpreset,
    Rerun,
    ModelsPending,
    NeedsKey,
    JoinGiven,
}

impl When {
    fn holds(&self, state: &State) -> bool {
        match self {
            When::Always => true,
            When::FreshAndUnpreset => !state.detect.markers_present && state.preset.is_none(),
            When::Rerun => state.detect.markers_present,
            When::ModelsPending => {
                !state.picked_harnesses().is_empty() && !state.answers.contains_key("models")
            }
            When::NeedsKey => {
                !state.answers.contains_key("keys")
                    && state
                        .picked_harnesses()
                        .iter()
                        .any(|h| env_for(&state.detect, h).is_none())
            }
            When::JoinGiven => state.join,
        }
    }
}

/// One screen: its id, when it runs, and whether each preset stops to
/// ask on it (aggressive, balanced, user-reviewed).
#[derive(Debug, Clone, Copy)]
pub struct Screen {
    pub id: &'static str,
    pub when: When,
    pub stops: [bool; 3],
}

/// The fixed screen order. The wizard never reorders; a false `when`
/// records a skip and moves on.
pub const SCREENS: [Screen; 20] = [
    Screen {
        id: "preflight",
        when: When::Always,
        stops: [false, false, false],
    },
    Screen {
        id: "rerun",
        when: When::Rerun,
        stops: [false, true, true],
    },
    Screen {
        id: "preset",
        when: When::FreshAndUnpreset,
        stops: [false, true, true],
    },
    Screen {
        id: "look",
        when: When::Always,
        stops: [false, false, true],
    },
    Screen {
        id: "harnesses",
        when: When::Always,
        stops: [false, true, true],
    },
    Screen {
        id: "models",
        when: When::ModelsPending,
        stops: [false, false, true],
    },
    Screen {
        id: "keys",
        when: When::NeedsKey,
        stops: [false, true, true],
    },
    Screen {
        id: "github-app",
        when: When::Always,
        stops: [false, true, true],
    },
    Screen {
        id: "budget",
        when: When::Always,
        stops: [false, false, true],
    },
    Screen {
        id: "review-merge",
        when: When::Always,
        stops: [false, true, true],
    },
    Screen {
        id: "interview",
        when: When::Always,
        stops: [false, false, true],
    },
    Screen {
        id: "services",
        when: When::Always,
        stops: [false, true, true],
    },
    Screen {
        id: "claude-auto-mode",
        when: When::Always,
        stops: [true, true, true],
    },
    Screen {
        id: "usage-report",
        when: When::Always,
        stops: [true, true, true],
    },
    Screen {
        id: "messaging",
        when: When::Always,
        stops: [false, false, true],
    },
    Screen {
        id: "projects",
        when: When::Always,
        stops: [false, false, true],
    },
    Screen {
        id: "join",
        when: When::JoinGiven,
        stops: [false, true, true],
    },
    Screen {
        id: "review",
        when: When::Always,
        stops: [false, true, true],
    },
    Screen {
        id: "apply",
        when: When::Always,
        stops: [false, false, false],
    },
    Screen {
        id: "done",
        when: When::Always,
        stops: [false, false, false],
    },
];

/// An answer the flow keeps. A secret never lands here - the keys
/// screen stores an env var name or nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Yes,
    No,
    Picked(Vec<String>),
    Text(String),
}

/// Everything one run carries. A host restores a mid-run session by
/// building this and driving `step` again.
#[derive(Debug, Clone)]
pub struct State {
    pub preset: Option<Preset>,
    pub join: bool,
    pub detect: Detect,
    pub answers: BTreeMap<&'static str, Answer>,
    pub index: usize,
    pub writes_started: bool,
    pub skip_all: bool,
    /// (screen id, "shown" | "skipped") in walk order, for the report.
    pub screens: Vec<(&'static str, &'static str)>,
    /// The ids of the screens shown so far; back walks this in reverse.
    pub trail: Vec<&'static str>,
    /// (harness, "pass" | "fail:<reason>", milliseconds).
    pub checks: Vec<(&'static str, String, u64)>,
}

impl State {
    pub fn new(preset: Option<Preset>, detect: Detect, join: bool) -> State {
        State {
            preset,
            join,
            detect,
            answers: BTreeMap::new(),
            index: 0,
            writes_started: false,
            skip_all: false,
            screens: Vec::new(),
            trail: Vec::new(),
            checks: Vec::new(),
        }
    }

    /// The harness names the harnesses screen committed.
    pub fn picked_harnesses(&self) -> Vec<String> {
        match self.answers.get("harnesses") {
            Some(Answer::Picked(picked)) => picked.clone(),
            _ => Vec::new(),
        }
    }

    /// Back is live until the review screen says yes to a write.
    pub fn back_allowed(&self) -> bool {
        !self.writes_started
    }
}

/// What one `step` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    Forward,
    Back,
    Cancel,
    SkipToDone,
    Done,
}

/// The prompter's reply to a yes/no ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conf {
    Back,
    Cancel,
    Yes,
    No,
}

/// The prompter's reply to a single pick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    Back,
    Cancel,
    Ok(usize),
}

/// The prompter's reply to a checklist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ticks {
    Back,
    Cancel,
    Ok(Vec<usize>),
}

/// The prompter's reply to a secret ask. The pure flow never asks for
/// one; the terminal host owns the masked read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hidden {
    Back,
    Cancel,
    Skipped,
    Value(String),
}

/// One probe outcome: how long a check took, or why it failed.
#[derive(Debug, Clone, Copy)]
pub enum Probe {
    Pass { ms: u64 },
    Fail(&'static str),
}

/// The flow's view of the terminal. Every ask names its screen so a
/// script and the report can key on it.
pub trait Prompter {
    fn confirm(&mut self, screen: &'static str, prompt: &str, default: bool) -> Conf;
    fn select(
        &mut self,
        screen: &'static str,
        prompt: &str,
        options: &[&str],
        default: usize,
    ) -> Pick;
    fn checklist(
        &mut self,
        screen: &'static str,
        prompt: &str,
        options: &[&str],
        defaults: &[usize],
    ) -> Ticks;
    fn secret(&mut self, screen: &'static str, prompt: &str) -> Hidden;
    fn note(&mut self, screen: &'static str, text: &str);
    fn progress(&mut self, screen: &'static str, label: &str);
}

/// The flow's view of the real checks. The terminal host runs them;
/// tests answer with a fixed queue.
pub trait Probes {
    fn harness_check(&mut self, harness: &str) -> Probe;
    fn model_check(&mut self, harness: &str, model: &str) -> Probe;
}

enum Flow<T> {
    Answered(T),
    Back,
    Cancel,
}

enum Outcome {
    Forward,
    Back,
    Cancel,
    SkipToDone,
}

/// The shared confirm loop. A Back honours the latch: once writes have
/// started it says so and asks again.
fn confirm_flow(
    state: &State,
    pr: &mut dyn Prompter,
    screen: &'static str,
    prompt: &str,
    default: bool,
) -> Flow<bool> {
    loop {
        match pr.confirm(screen, prompt, default) {
            Conf::Yes => return Flow::Answered(true),
            Conf::No => return Flow::Answered(false),
            Conf::Cancel => return Flow::Cancel,
            Conf::Back => {
                if state.back_allowed() {
                    return Flow::Back;
                }
                pr.note(screen, "Setup already wrote changes. You cannot go back.");
            }
        }
    }
}

fn pick_flow(
    state: &State,
    pr: &mut dyn Prompter,
    screen: &'static str,
    prompt: &str,
    options: &[&str],
    default: usize,
) -> Flow<usize> {
    loop {
        match pr.select(screen, prompt, options, default) {
            Pick::Ok(i) => return Flow::Answered(i),
            Pick::Cancel => return Flow::Cancel,
            Pick::Back => {
                if state.back_allowed() {
                    return Flow::Back;
                }
                pr.note(screen, "Setup already wrote changes. You cannot go back.");
            }
        }
    }
}

fn ticks_flow(
    state: &State,
    pr: &mut dyn Prompter,
    screen: &'static str,
    prompt: &str,
    options: &[&str],
    defaults: &[usize],
) -> Flow<Vec<usize>> {
    loop {
        match pr.checklist(screen, prompt, options, defaults) {
            Ticks::Ok(v) => return Flow::Answered(v),
            Ticks::Cancel => return Flow::Cancel,
            Ticks::Back => {
                if state.back_allowed() {
                    return Flow::Back;
                }
                pr.note(screen, "Setup already wrote changes. You cannot go back.");
            }
        }
    }
}

fn slot(state: &State) -> usize {
    state.preset.map(Preset::slot).unwrap_or(1)
}

/// A screen asks the person when its preset stops there. The preset screen
/// always asks, because no preset exists before it answers.
fn asks(screen: &Screen, state: &State) -> bool {
    screen.id == "preset" || screen.stops.get(slot(state)).copied().unwrap_or(false)
}

/// (this step, all steps) over the screens that ask the person. The total
/// moves when a later answer adds or drops a screen, so the number is the
/// count under the answers given so far.
fn step_counts(state: &State) -> (usize, usize) {
    let done = state
        .trail
        .iter()
        .filter(|id| {
            SCREENS
                .iter()
                .any(|s| s.id == **id && asks(s, state) && s.id != "apply" && s.id != "done")
        })
        .count();
    let ahead = SCREENS
        .get(state.index + 1..)
        .unwrap_or_default()
        .iter()
        .filter(|s| s.when.holds(state) && asks(s, state) && s.id != "apply" && s.id != "done")
        .count();
    (done, done + ahead)
}

/// Walk one screen. Skipped screens record and fall through in the same
/// call; a shown screen dispatches to its handler.
pub fn step(state: &mut State, pr: &mut dyn Prompter, pb: &mut dyn Probes) -> Next {
    while state.index < SCREENS.len() {
        let screen = SCREENS[state.index];
        if screen.when.holds(state) {
            break;
        }
        state.screens.push((screen.id, "skipped"));
        state.index += 1;
    }
    if state.index >= SCREENS.len() {
        return Next::Done;
    }
    let screen = SCREENS[state.index];
    state.screens.push((screen.id, "shown"));
    state.trail.push(screen.id);
    let stops = screen.stops[slot(state)];
    if stops && !matches!(screen.id, "apply" | "done") {
        let (n, m) = step_counts(state);
        pr.note(screen.id, &format!("Step {n} of {m}"));
    }
    let outcome = match screen.id {
        "preflight" => screen_preflight(state, pr),
        "rerun" => screen_rerun(state, pr, stops),
        "preset" => screen_preset(state, pr),
        "look" => screen_look(state, pr, stops),
        "harnesses" => screen_harnesses(state, pr, pb, stops),
        "models" => screen_models(state, pr, pb, stops),
        "keys" => screen_keys(state, pr, stops),
        "github-app" => screen_github_app(state, pr, stops),
        "budget" => screen_budget(state, pr, stops),
        "review-merge" => screen_review_merge(state, pr, stops),
        "interview" => screen_interview(state, pr, stops),
        "services" => screen_services(state, pr, stops),
        "claude-auto-mode" => screen_claude_auto_mode(state, pr),
        "usage-report" => screen_usage_report(state, pr),
        "messaging" => screen_messaging(state, pr, stops),
        "projects" => screen_projects(state, pr, stops),
        "join" => screen_join(state, pr, stops),
        "review" => screen_review(state, pr, stops),
        "apply" => screen_apply(state, pr),
        _ => screen_done(state, pr),
    };
    match outcome {
        Outcome::Forward => {
            state.index += 1;
            Next::Forward
        }
        Outcome::Back => {
            state.trail.pop();
            if let Some(last) = state.trail.last() {
                if let Some(pos) = SCREENS.iter().position(|s| s.id == *last) {
                    state.index = pos;
                }
            }
            Next::Back
        }
        Outcome::Cancel => Next::Cancel,
        Outcome::SkipToDone => Next::SkipToDone,
    }
}

fn screen_preflight(state: &State, pr: &mut dyn Prompter) -> Outcome {
    if state.detect.markers_present {
        pr.note(
            "preflight",
            "This machine has a record of an earlier setup. Setup fills in only what is missing.",
        );
    } else {
        pr.note(
            "preflight",
            "No earlier setup found on this machine. Setup starts now.",
        );
    }
    Outcome::Forward
}

fn screen_rerun(state: &mut State, pr: &mut dyn Prompter, stops: bool) -> Outcome {
    let options = [
        "Verify and keep",
        "Fill missing only",
        "Review every screen",
        "One section",
    ];
    if !stops {
        state
            .answers
            .insert("rerun", Answer::Text(options[0].into()));
        pr.note(
            "rerun",
            "Setup runs every check again. It keeps each setting that passes.",
        );
        return Outcome::Forward;
    }
    match pick_flow(
        state,
        pr,
        "rerun",
        "How much should this pass redo?",
        &options,
        0,
    ) {
        Flow::Answered(i) => {
            let label = options.get(i).copied().unwrap_or(options[0]);
            state.answers.insert("rerun", Answer::Text(label.into()));
            Outcome::Forward
        }
        Flow::Back => Outcome::Back,
        Flow::Cancel => Outcome::Cancel,
    }
}

fn screen_preset(state: &mut State, pr: &mut dyn Prompter) -> Outcome {
    // This screen always asks: an auto-run cannot choose a posture
    // before the posture exists. The `when` gate keeps it off a rerun.
    // The one thing the three postures change for the person is who
    // merges, so the question asks that and each option says what happens.
    let options = [
        "Yes. Agents merge their own PRs once review and CI pass. Setup picks safe answers and asks little.",
        "No. I merge every PR myself. Setup asks the usual questions.",
        "No. I merge every PR myself. Setup asks every question and picks nothing for me.",
    ];
    let default = if state.detect.auto_merge == Some(true) {
        0
    } else {
        1
    };
    match pick_flow(
        state,
        pr,
        "preset",
        "Should agents merge their own PRs once review and CI pass?",
        &options,
        default,
    ) {
        Flow::Answered(i) => {
            let chosen = Preset::from_slot(i);
            state.preset = Some(chosen);
            state
                .answers
                .insert("preset", Answer::Text(chosen.name().into()));
            Outcome::Forward
        }
        Flow::Back => Outcome::Back,
        Flow::Cancel => Outcome::Cancel,
    }
}

fn screen_look(state: &mut State, pr: &mut dyn Prompter, stops: bool) -> Outcome {
    if !stops {
        state.answers.insert("look", Answer::Yes);
        pr.note(
            "look",
            &format!(
                "Setup already looked at this machine. It found {} harness(es).",
                state.detect.harnesses.len()
            ),
        );
        return Outcome::Forward;
    }
    match confirm_flow(state, pr, "look", "May setup look at this machine?", true) {
        Flow::Answered(true) => {
            state.answers.insert("look", Answer::Yes);
            Outcome::Forward
        }
        Flow::Answered(false) => {
            state.detect.band = None;
            state.detect.env_keys.clear();
            state.detect.harnesses.clear();
            state.answers.insert("look", Answer::No);
            pr.note(
                "look",
                "Setup dropped what it measured. It reads nothing more from this machine.",
            );
            Outcome::Forward
        }
        Flow::Back => Outcome::Back,
        Flow::Cancel => Outcome::Cancel,
    }
}

struct Opt {
    name: &'static str,
    label: String,
}

fn env_for(detect: &Detect, harness: &str) -> Option<&'static str> {
    detect
        .env_keys
        .iter()
        .find(|(h, _)| *h == harness)
        .map(|(_, e)| *e)
}

fn harness_options(detect: &Detect) -> Vec<Opt> {
    HARNESS_LIST
        .iter()
        .map(|h| {
            let by = detect
                .harnesses
                .iter()
                .find(|f| f.name == h.name)
                .map(|f| f.by)
                .unwrap_or("not looked for");
            Opt {
                name: h.name,
                label: format!("{} (found {})", h.label, by),
            }
        })
        .collect()
}

/// First pass: what the posture would tick. Re-show after a commit:
/// the committed ticks themselves.
fn harness_defaults(state: &State, options: &[Opt]) -> Vec<usize> {
    if let Some(Answer::Picked(picked)) = state.answers.get("harnesses") {
        return options
            .iter()
            .enumerate()
            .filter(|(_, o)| picked.iter().any(|p| p.as_str() == o.name))
            .map(|(i, _)| i)
            .collect();
    }
    options
        .iter()
        .enumerate()
        .filter(|(_, o)| {
            let found = state.detect.harnesses.iter().any(|f| f.name == o.name);
            let keyed = env_for(&state.detect, o.name).is_some();
            match state.preset {
                Some(Preset::Aggressive) => found,
                Some(Preset::UserReviewed) => false,
                Some(Preset::Balanced) | None => found && keyed,
            }
        })
        .map(|(i, _)| i)
        .collect()
}

fn screen_harnesses(
    state: &mut State,
    pr: &mut dyn Prompter,
    pb: &mut dyn Probes,
    stops: bool,
) -> Outcome {
    loop {
        let options = harness_options(&state.detect);
        let labels: Vec<&str> = options.iter().map(|o| o.label.as_str()).collect();
        let defaults = harness_defaults(state, &options);
        let idxs = if !stops {
            defaults
        } else {
            match ticks_flow(
                state,
                pr,
                "harnesses",
                "Which harnesses should footnote use?",
                &labels,
                &defaults,
            ) {
                Flow::Answered(v) => v,
                Flow::Back => return Outcome::Back,
                Flow::Cancel => return Outcome::Cancel,
            }
        };
        let names: Vec<&'static str> = idxs
            .iter()
            .filter_map(|i| options.get(*i).map(|o| o.name))
            .collect();
        // A zero-tick screen is a valid answer: footnote stays read-only
        // and the flow advances.
        if names.is_empty() {
            state
                .answers
                .insert("harnesses", Answer::Picked(Vec::new()));
            pr.note(
                "harnesses",
                "You picked no harness. Footnote stays read-only until you add one.",
            );
            return Outcome::Forward;
        }
        let mut passing: Vec<&'static str> = Vec::new();
        let mut queue: VecDeque<&'static str> = names.iter().copied().collect();
        let mut picked_another = false;
        while let Some(h) = queue.front().copied() {
            match pb.harness_check(h) {
                Probe::Pass { ms } => {
                    state.checks.push((h, "pass".into(), ms));
                    passing.push(h);
                    queue.pop_front();
                }
                Probe::Fail(reason) => {
                    state.checks.push((h, format!("fail:{reason}"), 0));
                    let mut opts: Vec<&str> = vec!["Retry", "Pick another", "Skip this one"];
                    if passing.is_empty() {
                        // With nothing passing yet, the person can save
                        // the run and finish later instead.
                        opts.push("Skip for now: save and finish later");
                    }
                    let pick = match pick_flow(
                        state,
                        pr,
                        "harnesses",
                        &format!("{h} check failed ({reason}). What now?"),
                        &opts,
                        0,
                    ) {
                        Flow::Answered(i) => i,
                        Flow::Back => return Outcome::Back,
                        Flow::Cancel => return Outcome::Cancel,
                    };
                    match pick {
                        0 => {} // probe the same harness again
                        1 => {
                            picked_another = true;
                            break;
                        }
                        2 => {
                            queue.pop_front();
                        }
                        _ => {
                            state.skip_all = true;
                            return Outcome::SkipToDone;
                        }
                    }
                }
            }
        }
        state.answers.insert(
            "harnesses",
            Answer::Picked(passing.iter().map(|h| h.to_string()).collect()),
        );
        if !picked_another {
            return Outcome::Forward;
        }
    }
}

fn screen_models(
    state: &mut State,
    pr: &mut dyn Prompter,
    pb: &mut dyn Probes,
    stops: bool,
) -> Outcome {
    if state.detect.models.is_empty() {
        state.answers.insert("models", Answer::Picked(Vec::new()));
        pr.note(
            "models",
            "Setup found no model list. Each harness keeps its current model.",
        );
        return Outcome::Forward;
    }
    let mut entries: Vec<String> = Vec::new();
    for h in state.picked_harnesses() {
        let mut opts = state.detect.models.clone();
        opts.push("Leave unchanged".to_string());
        let committed = match state.answers.get("models") {
            Some(Answer::Picked(picked)) => picked
                .iter()
                .filter_map(|e| e.split_once('='))
                .find(|(name, _)| *name == h.as_str())
                .map(|(_, model)| model.to_string()),
            _ => None,
        };
        let default = committed
            .and_then(|m| opts.iter().position(|o| *o == m))
            .unwrap_or(opts.len() - 1);
        let choice = if !stops {
            Some(state.detect.models[0].clone())
        } else {
            let refs: Vec<&str> = opts.iter().map(|o| o.as_str()).collect();
            match pick_flow(
                state,
                pr,
                "models",
                &format!("Default model for {h}"),
                &refs,
                default,
            ) {
                Flow::Answered(i) if i + 1 == opts.len() => None,
                Flow::Answered(i) => opts.get(i).cloned(),
                Flow::Back => return Outcome::Back,
                Flow::Cancel => return Outcome::Cancel,
            }
        };
        let Some(model) = choice else { continue };
        match pb.model_check(&h, &model) {
            Probe::Pass { ms: _ } => entries.push(format!("{h}={model}")),
            Probe::Fail(reason) => pr.note(
                "models",
                &format!("{h} keeps its current model. The check for {model} failed: {reason}"),
            ),
        }
    }
    state.answers.insert("models", Answer::Picked(entries));
    Outcome::Forward
}

fn screen_keys(state: &mut State, pr: &mut dyn Prompter, stops: bool) -> Outcome {
    let committed: Vec<String> = match state.answers.get("keys") {
        Some(Answer::Picked(picked)) => picked.clone(),
        _ => Vec::new(),
    };
    let mut stored: Vec<String> = Vec::new();
    for h in state.picked_harnesses() {
        let Some(env) = env_for(&state.detect, &h) else {
            pr.note(
                "keys",
                &format!(
                    "Setup found no API key for {h}. Add the key before you apply the changes."
                ),
            );
            continue;
        };
        let keep = if !stops {
            true
        } else {
            let default = if committed.is_empty() {
                true
            } else {
                committed.iter().any(|e| e == env)
            };
            match confirm_flow(
                state,
                pr,
                "keys",
                &format!("Use existing {env} for {h}?"),
                default,
            ) {
                Flow::Answered(true) => true,
                Flow::Answered(false) => false,
                Flow::Back => return Outcome::Back,
                Flow::Cancel => return Outcome::Cancel,
            }
        };
        if keep {
            stored.push(env.to_string());
        }
    }
    state.answers.insert("keys", Answer::Picked(stored));
    Outcome::Forward
}

/// The fleet's own GitHub App through GitHub's manifest flow. The screen
/// runs the exchange live because the redirect code is a credential and
/// never enters state; only the public App id rides the proposal, which
/// apply writes through the locked config writer. Default off.
fn screen_github_app(state: &mut State, pr: &mut dyn Prompter, stops: bool) -> Outcome {
    use crate::setup::github_app_flow;
    if !stops {
        return Outcome::Forward;
    }
    if github_app_flow::configured_app_id().is_some() {
        pr.note(
            "github-app",
            "A fleet GitHub App is already configured (github.app_id). Leaving it as it is.",
        );
        return Outcome::Forward;
    }
    let wants = match confirm_flow(
        state,
        pr,
        "github-app",
        "Give the fleet its own GitHub quota? Creates a GitHub App in your account through GitHub's manifest flow; without it fno keeps using your user token.",
        false,
    ) {
        Flow::Answered(true) => true,
        Flow::Answered(false) => false,
        Flow::Back => return Outcome::Back,
        Flow::Cancel => return Outcome::Cancel,
    };
    if !wants {
        state.answers.insert("github-app", Answer::No);
        return Outcome::Forward;
    }
    let conversion = match github_app_round_trip(pr) {
        RoundTrip::Created { app_id } => app_id,
        RoundTrip::Back => return Outcome::Back,
        RoundTrip::Cancel => return Outcome::Cancel,
        RoundTrip::Refused => {
            state.answers.insert("github-app", Answer::No);
            return Outcome::Forward;
        }
    };
    state.answers.insert("github-app", Answer::Text(conversion));
    Outcome::Forward
}

/// How the launcher round-trip ended. `Created` carries only the public
/// App id; the pem never leaves the flow module and the paste code never
/// enters state.
enum RoundTrip {
    Created { app_id: String },
    Back,
    Cancel,
    Refused,
}

/// One launcher round-trip: write the launcher, open it, collect the
/// paste, exchange the code, and store the key. Every refusal notes itself
/// and names what was written; Back and Cancel bubble untouched.
fn github_app_round_trip(pr: &mut dyn Prompter) -> RoundTrip {
    use crate::setup::github_app_flow as flow;
    let nonce = flow::state_nonce();
    let name = flow::app_name();
    let launcher = std::env::temp_dir().join("fno-fleet-app.html");
    if let Err(fault) = flow::write_launcher(&launcher, &nonce, &name) {
        pr.note(
            "github-app",
            &format!("Setup could not write the launcher page, so nothing was written: {fault}"),
        );
        return RoundTrip::Refused;
    }
    pr.note(
        "github-app",
        "GitHub's App creation page opens, prefilled for the fleet. Click Create; GitHub bounces to a page whose address bar carries ?code=... and your launcher's state.",
    );
    if !flow::open_in_browser(&launcher) {
        pr.note(
            "github-app",
            &format!(
                "No browser opened. Open {} in a browser on any machine, then click Create.",
                launcher.display()
            ),
        );
    }
    let code = match pr.secret(
        "github-app",
        "Paste the code= value from the redirected address bar",
    ) {
        Hidden::Value(code) => code,
        Hidden::Back => return RoundTrip::Back,
        Hidden::Cancel => return RoundTrip::Cancel,
        Hidden::Skipped => {
            pr.note(
                "github-app",
                "Skipped. Nothing was written; your user token stays the default.",
            );
            return RoundTrip::Refused;
        }
    };
    pr.progress("github-app", "Exchanging the code with GitHub");
    let conversion = match flow::exchange(code.trim()) {
        Ok(conversion) => conversion,
        Err(fault) => {
            pr.note(
                "github-app",
                &format!(
                    "The exchange failed, so nothing was written: {fault}. The App may exist on GitHub; delete it there or rerun setup later."
                ),
            );
            return RoundTrip::Refused;
        }
    };
    let key_path = match flow::default_key_path() {
        Ok(path) => path,
        Err(fault) => {
            pr.note(
                "github-app",
                &format!(
                    "Setup could not resolve the key path, so nothing was written: {fault}. The App exists on GitHub; download its key from the App settings page or rerun setup."
                ),
            );
            return RoundTrip::Refused;
        }
    };
    if let Err(fault) = flow::write_key(&key_path, &conversion.pem) {
        pr.note(
            "github-app",
            &format!(
                "Setup could not write the key, so config is untouched: {fault}. The App exists on GitHub; download its key from the App settings page or rerun setup."
            ),
        );
        return RoundTrip::Refused;
    }
    pr.note(
        "github-app",
        &format!(
            "The App {} is created and its key is stored. Install it on your repos: https://github.com/apps/{}/installations/new - the installation resolves by owner match; a github.app_installation_id pin is optional.",
            conversion.slug, conversion.slug
        ),
    );
    RoundTrip::Created {
        app_id: conversion.app_id,
    }
}

fn screen_budget(state: &mut State, pr: &mut dyn Prompter, stops: bool) -> Outcome {
    let Some(band) = state.detect.band else {
        state.answers.insert("budget", Answer::No);
        pr.note(
            "budget",
            "Setup could not measure how many workers this machine can run. The current limit stays.",
        );
        return Outcome::Forward;
    };
    let default = match state.answers.get("budget") {
        Some(Answer::No) => false,
        _ => true,
    };
    let apply = if !stops {
        true
    } else {
        match confirm_flow(
            state,
            pr,
            "budget",
            &format!("Live workers on this machine: apply the measured band of {band}?"),
            default,
        ) {
            Flow::Answered(true) => true,
            Flow::Answered(false) => false,
            Flow::Back => return Outcome::Back,
            Flow::Cancel => return Outcome::Cancel,
        }
    };
    if apply {
        state
            .answers
            .insert("budget", Answer::Text(band.to_string()));
    } else {
        state.answers.insert("budget", Answer::No);
    }
    Outcome::Forward
}

fn screen_review_merge(state: &mut State, pr: &mut dyn Prompter, stops: bool) -> Outcome {
    let posture_default = match state.answers.get("review") {
        Some(Answer::Text(t)) => t == "self_review",
        _ => true,
    };
    let posture = if !stops {
        true
    } else {
        match confirm_flow(
            state,
            pr,
            "review-merge",
            "Should each agent review its own code before it opens a PR?",
            posture_default,
        ) {
            Flow::Answered(true) => true,
            Flow::Answered(false) => false,
            Flow::Back => return Outcome::Back,
            Flow::Cancel => return Outcome::Cancel,
        }
    };
    let merge_default = match state.answers.get("merge") {
        Some(Answer::Yes) => true,
        Some(Answer::No) => false,
        _ => state
            .detect
            .auto_merge
            .unwrap_or(matches!(state.preset, Some(Preset::Aggressive))),
    };
    let merge = if !stops {
        merge_default
    } else {
        match confirm_flow(
            state,
            pr,
            "review-merge",
            "Merge PRs on their own when review and CI pass?",
            merge_default,
        ) {
            Flow::Answered(true) => true,
            Flow::Answered(false) => false,
            Flow::Back => return Outcome::Back,
            Flow::Cancel => return Outcome::Cancel,
        }
    };
    if posture {
        state
            .answers
            .insert("review", Answer::Text("self_review".into()));
    } else {
        // A prior pass may have seeded self_review; leaving it would keep
        // proposing the posture the user just declined.
        state.answers.remove("review");
        pr.note("review-merge", "Your review setting stays as it is.");
    }
    state
        .answers
        .insert("merge", if merge { Answer::Yes } else { Answer::No });
    Outcome::Forward
}

/// The review-lane options the interview offers. The first three are the
/// builtin reviewers the validated writer resolves (config_cli model,
/// BUILTIN_REVIEWERS); a tick past them names the canonical required bot,
/// whose login is the one fno's own schema documents as the built-in
/// default. Free text never enters this screen: the writer refuses an
/// unresolvable reviewer name, so the ask only ever offers known ones.
const REVIEW_LANE_OPTIONS: [&str; 3] = ["code-review", "declare", "chatgpt-codex-connector"];
const REVIEW_BOT_LOGIN: &str = "chatgpt-codex-connector";

/// The skills the target loop can reach mid-run; mirrors the toolbag table
/// in skills/target/references/toolbag.md.
const LOOP_SKILL_OPTIONS: [&str; 19] = [
    "fix",
    "tdd",
    "test-audit",
    "think",
    "verify-footnotes",
    "design-critic",
    "blueprint",
    "execute",
    "review",
    "ship",
    "ship-docs",
    "speculate",
    "triage",
    "audit",
    "intel",
    "law",
    "harness-commands",
    "setup",
    "using-fno",
];

/// The shipped skills a person can name as favorites; mirrors the `skills/`
/// directory of the plugin this binary ships in. One shipped skill is left
/// out: its name is the one string the tree's wording guard refuses outside
/// its own allowlist.
const FAVORITE_SKILL_OPTIONS: [&str; 29] = [
    "agent",
    "audit",
    "blueprint",
    "cache-keepalive",
    "design-critic",
    "dnd",
    "execute",
    "fix",
    "fno-me",
    "groom",
    "growth-launch",
    "harness-commands",
    "intel",
    "law",
    "lead",
    "mail",
    "persona-review",
    "review",
    "setup",
    "ship",
    "ship-docs",
    "speculate",
    "target",
    "tdd",
    "test-audit",
    "think",
    "triage",
    "using-fno",
    "verify-footnotes",
];

/// A list of strings as the config writer reads it: a TOML array literal.
/// The apply pass passes the text to `fno config set`, whose coercion reads
/// the array form first (setup_run `proposal_token`).
fn toml_array(values: &[String]) -> String {
    let items: Vec<String> = values
        .iter()
        .map(|v| format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\"")))
        .collect();
    format!("[{}]", items.join(", "))
}

/// The working-style interview: how the person works, asked once the
/// machine probing is done. Four asks, all pick-based - the pure flow has
/// no free-text prompter method, and the review-lane ask must only offer
/// names the validated writer resolves anyway. Under a preset that does
/// not stop here, the safe defaults apply without a prompt: the first
/// picked harness as the spawn lane, nothing else recorded.
fn screen_interview(state: &mut State, pr: &mut dyn Prompter, stops: bool) -> Outcome {
    let picked = state.picked_harnesses();
    // A preset that does not stop records the lane only when the run
    // actually picked harnesses; a default naming an unwired CLI is no
    // safe answer.
    let auto_lane = picked.first().cloned().unwrap_or_default();
    let lanes: Vec<String> = if picked.is_empty() {
        HARNESS_LIST.iter().map(|h| h.name.to_string()).collect()
    } else {
        picked
    };
    let lane = if !stops {
        auto_lane
    } else {
        let opts: Vec<&str> = lanes.iter().map(|s| s.as_str()).collect();
        let prompt = "Which harness lane should spawned agents default to?";
        let lane_default = state
            .answers
            .get("interview-lane")
            .and_then(|a| match a {
                Answer::Text(t) => lanes.iter().position(|l| l == t),
                _ => None,
            })
            .unwrap_or(0);
        match pick_flow(state, pr, "interview", prompt, &opts, lane_default) {
            Flow::Answered(i) => lanes.get(i).cloned().unwrap_or_default(),
            Flow::Back => return Outcome::Back,
            Flow::Cancel => return Outcome::Cancel,
        }
    };
    state.answers.insert("interview-lane", Answer::Text(lane));

    let review_names = match interview_ticks(
        state,
        pr,
        stops,
        "Which review lane should gate your PRs? (the first two are local reviewer skills; the last is the Codex review bot)",
        &REVIEW_LANE_OPTIONS,
        "interview-review",
    ) {
        Flow::Answered(v) => v,
        Flow::Back => return Outcome::Back,
        Flow::Cancel => return Outcome::Cancel,
    };
    state
        .answers
        .insert("interview-review", Answer::Picked(review_names));

    let favorite_names = match interview_ticks(
        state,
        pr,
        stops,
        "Which of the shipped skills are your favorites?",
        &FAVORITE_SKILL_OPTIONS,
        "interview-favorites",
    ) {
        Flow::Answered(v) => v,
        Flow::Back => return Outcome::Back,
        Flow::Cancel => return Outcome::Cancel,
    };
    state
        .answers
        .insert("interview-favorites", Answer::Picked(favorite_names));

    let loop_names = match interview_ticks(
        state,
        pr,
        stops,
        "Which skills should the target loop reach for by name?",
        &LOOP_SKILL_OPTIONS,
        "interview-loop",
    ) {
        Flow::Answered(v) => v,
        Flow::Back => return Outcome::Back,
        Flow::Cancel => return Outcome::Cancel,
    };
    state
        .answers
        .insert("interview-loop", Answer::Picked(loop_names));

    Outcome::Forward
}

/// One checklist ask of the interview: a non-stopping preset takes the
/// empty default with no prompt, a stopping preset runs the shared ticks
/// loop and maps indices to option names. A re-shown screen offers the
/// committed answer as the default, like every other screen.
fn interview_ticks(
    state: &State,
    pr: &mut dyn Prompter,
    stops: bool,
    prompt: &str,
    options: &[&str],
    answer_key: &'static str,
) -> Flow<Vec<String>> {
    if !stops {
        return Flow::Answered(Vec::new());
    }
    let defaults: Vec<usize> = match state.answers.get(answer_key) {
        Some(Answer::Picked(names)) => options
            .iter()
            .enumerate()
            .filter(|&(_, option)| names.contains(&option.to_string()))
            .map(|(i, _)| i)
            .collect(),
        _ => Vec::new(),
    };
    match ticks_flow(state, pr, "interview", prompt, options, &defaults) {
        Flow::Answered(v) => Flow::Answered(
            v.iter()
                .filter_map(|i| options.get(*i).map(|o| o.to_string()))
                .collect(),
        ),
        Flow::Back => Flow::Back,
        Flow::Cancel => Flow::Cancel,
    }
}

const SERVICE_IDS: [&str; 3] = ["pr-watcher", "auto-heal", "groom-agent"];
const SERVICE_LABELS: [&str; 3] = ["PR watcher", "Auto-heal", "Backlog groom"];

/// First pass: what the posture would tick. Re-show after a commit:
/// the committed ticks themselves.
fn service_defaults(state: &State) -> Vec<usize> {
    if let Some(Answer::Picked(picked)) = state.answers.get("services") {
        return SERVICE_IDS
            .iter()
            .enumerate()
            .filter(|(i, _)| picked.iter().any(|p| p.as_str() == SERVICE_IDS[*i]))
            .map(|(i, _)| i)
            .collect();
    }
    match state.preset {
        Some(Preset::Aggressive) => vec![0, 1, 2],
        Some(Preset::UserReviewed) => Vec::new(),
        Some(Preset::Balanced) | None => vec![0, 2],
    }
}

fn screen_services(state: &mut State, pr: &mut dyn Prompter, stops: bool) -> Outcome {
    let defaults = service_defaults(state);
    let idxs = if !stops {
        defaults
    } else {
        match ticks_flow(
            state,
            pr,
            "services",
            "Run these in the background?",
            &SERVICE_LABELS,
            &defaults,
        ) {
            Flow::Answered(v) => v,
            Flow::Back => return Outcome::Back,
            Flow::Cancel => return Outcome::Cancel,
        }
    };
    let picked: Vec<String> = idxs
        .iter()
        .filter_map(|i| SERVICE_IDS.get(*i))
        .map(|id| id.to_string())
        .collect();
    state.answers.insert("services", Answer::Picked(picked));
    Outcome::Forward
}

fn screen_claude_auto_mode(state: &mut State, pr: &mut dyn Prompter) -> Outcome {
    match confirm_flow(
        state,
        pr,
        "claude-auto-mode",
        "Add Footnote's Auto Mode rules to Claude Code user settings?",
        false,
    ) {
        Flow::Answered(true) => {
            state.answers.insert("claude-auto-mode", Answer::Yes);
            Outcome::Forward
        }
        Flow::Answered(false) => {
            state.answers.insert("claude-auto-mode", Answer::No);
            Outcome::Forward
        }
        Flow::Back => Outcome::Back,
        Flow::Cancel => Outcome::Cancel,
    }
}

/// The opt-in daily usage report. Every preset answers no; a yes here only
/// names `fno doctor telemetry on` as the next step, which shows the report
/// and asks at the terminal before anything is recorded.
fn screen_usage_report(state: &mut State, pr: &mut dyn Prompter) -> Outcome {
    match confirm_flow(
        state,
        pr,
        "usage-report",
        "Send fno a daily usage report? Counts and identifiers only; you see the exact report before you say yes.",
        false,
    ) {
        Flow::Answered(true) => {
            state.answers.insert("usage-report", Answer::Yes);
            Outcome::Forward
        }
        Flow::Answered(false) => {
            state.answers.insert("usage-report", Answer::No);
            Outcome::Forward
        }
        Flow::Back => Outcome::Back,
        Flow::Cancel => Outcome::Cancel,
    }
}

/// Chat channels for open questions. Every preset ticks none: a channel
/// needs a token only the person can make, so the aggressive pass skips it
/// and the setup at apply asks for each pick's details.
fn screen_messaging(state: &mut State, pr: &mut dyn Prompter, stops: bool) -> Outcome {
    let labels: Vec<&str> = crate::setup_messaging::CHANNELS
        .iter()
        .map(|(_, label)| *label)
        .collect();
    let defaults: Vec<usize> = match state.answers.get("messaging") {
        Some(Answer::Picked(picked)) => (0..labels.len())
            .filter(|i| {
                picked
                    .iter()
                    .any(|p| p == crate::setup_messaging::CHANNELS[*i].0)
            })
            .collect(),
        _ => Vec::new(),
    };
    let idxs = if !stops {
        defaults
    } else {
        match ticks_flow(
            state,
            pr,
            "messaging",
            "Send open questions to a chat? Pick none to skip.",
            &labels,
            &defaults,
        ) {
            Flow::Answered(v) => v,
            Flow::Back => return Outcome::Back,
            Flow::Cancel => return Outcome::Cancel,
        }
    };
    let picked: Vec<String> = idxs
        .iter()
        .filter_map(|i| crate::setup_messaging::CHANNELS.get(*i))
        .map(|(kind, _)| kind.to_string())
        .collect();
    if picked.is_empty() {
        pr.note(
            "messaging",
            &format!(
                "To set up messaging later, run: {}",
                crate::setup_messaging::REENTER
            ),
        );
    }
    state.answers.insert("messaging", Answer::Picked(picked));
    Outcome::Forward
}

fn screen_projects(state: &mut State, pr: &mut dyn Prompter, stops: bool) -> Outcome {
    // The project layer answers only inside a git repo: repo_root()
    // falls back to the cwd, and setup must not litter an arbitrary
    // directory with .fno/ - the same gate the batch path applies.
    let in_repo = crate::setup_run::repo_root().join(".git").exists();
    if !in_repo {
        state.answers.insert("projects", Answer::No);
        state.answers.insert("scan", Answer::No);
        pr.note(
            "projects",
            "This folder is not a git repository. Setup skips project registration.",
        );
        return Outcome::Forward;
    }
    let register_default = match state.answers.get("projects") {
        Some(Answer::No) => false,
        _ => true,
    };
    let register = if !stops {
        true
    } else {
        match confirm_flow(
            state,
            pr,
            "projects",
            "Register this repo?",
            register_default,
        ) {
            Flow::Answered(true) => true,
            Flow::Answered(false) => false,
            Flow::Back => return Outcome::Back,
            Flow::Cancel => return Outcome::Cancel,
        }
    };
    let scan_default = match state.answers.get("scan") {
        Some(Answer::Yes) => true,
        Some(Answer::No) => false,
        _ => matches!(state.preset, Some(Preset::Aggressive)),
    };
    let scan = if !register {
        false
    } else if !stops {
        scan_default
    } else {
        match confirm_flow(
            state,
            pr,
            "projects",
            "Scan your code folders for more repos?",
            scan_default,
        ) {
            Flow::Answered(true) => true,
            Flow::Answered(false) => false,
            Flow::Back => return Outcome::Back,
            Flow::Cancel => return Outcome::Cancel,
        }
    };
    if !register {
        pr.note(
            "projects",
            "Setup does not register this folder. It scans nothing else.",
        );
    }
    state
        .answers
        .insert("projects", if register { Answer::Yes } else { Answer::No });
    state
        .answers
        .insert("scan", if scan { Answer::Yes } else { Answer::No });
    Outcome::Forward
}

fn screen_join(state: &mut State, pr: &mut dyn Prompter, stops: bool) -> Outcome {
    pr.note("join", "Nothing has changed yet.");
    for row in pending(state) {
        pr.note("join", &row_line(&row));
    }
    let yes_default = !matches!(state.preset, Some(Preset::UserReviewed));
    let yes = if !stops {
        true
    } else {
        match confirm_flow(
            state,
            pr,
            "join",
            "Proceed with these changes?",
            yes_default,
        ) {
            Flow::Answered(true) => true,
            Flow::Answered(false) => false,
            Flow::Back => return Outcome::Back,
            Flow::Cancel => return Outcome::Cancel,
        }
    };
    if yes {
        Outcome::Forward
    } else {
        Outcome::Cancel
    }
}

fn screen_review(state: &mut State, pr: &mut dyn Prompter, stops: bool) -> Outcome {
    let rows = pending(state);
    if rows.is_empty() {
        pr.note("review", "Nothing to write. Your config stays as it is.");
        return Outcome::Forward;
    }
    pr.note("review", "Setup writes all lines below in one step.");
    for row in &rows {
        pr.note("review", &row_line(row));
    }
    let yes_default = !matches!(state.preset, Some(Preset::UserReviewed));
    let yes = if !stops {
        true
    } else {
        match confirm_flow(state, pr, "review", "Write these changes?", yes_default) {
            Flow::Answered(true) => true,
            Flow::Answered(false) => false,
            Flow::Back => return Outcome::Back,
            Flow::Cancel => return Outcome::Cancel,
        }
    };
    if yes {
        state.writes_started = true;
        Outcome::Forward
    } else {
        Outcome::Cancel
    }
}

fn screen_apply(state: &State, pr: &mut dyn Prompter) -> Outcome {
    for row in pending(state) {
        pr.progress("apply", &format!("Will apply: {}", row_line(&row)));
    }
    pr.note(
        "apply",
        "Setup now runs each step. It prints one result line per step.",
    );
    Outcome::Forward
}

fn screen_done(state: &State, pr: &mut dyn Prompter) -> Outcome {
    if state.skip_all {
        pr.note(
            "done",
            "No harness passed its check, so setup is not marked finished. Fix the harness, then run: fno do wizard",
        );
    } else {
        pr.note(
            "done",
            "Setup prints a report, the backup path and the next step.",
        );
    }
    Outcome::Forward
}

/// The rows a finished run would write, in the same shape setup run
/// applies. `skip_all` withholds everything - the harness gate never
/// passed. The keys row stores env var names only; a secret value never
/// appears here or anywhere in state.
pub fn pending(state: &State) -> Vec<Proposal> {
    let mut rows = Vec::new();
    if state.skip_all {
        return rows;
    }
    if let Some(Answer::Yes) = state.answers.get("claude-auto-mode") {
        rows.push(Proposal {
            id: "claude-auto-mode",
            key: "autoMode.allow".into(),
            value: "Footnote State Writes + Footnote Role Grants + Footnote Review Attestation"
                .into(),
            scope: "global",
            reason: "the person's explicit approval to add Footnote's Auto Mode rules".into(),
            source: "wizard",
        });
    }
    if let Some(Answer::Yes) = state.answers.get("usage-report") {
        rows.push(Proposal {
            id: "usage-report",
            key: String::new(),
            value: "fno doctor telemetry on".into(),
            scope: "global",
            reason: "the person's yes; the terminal door shows the report and asks once more"
                .into(),
            source: "wizard",
        });
    }
    if let Some(Answer::Text(n)) = state.answers.get("budget") {
        rows.push(Proposal {
            id: "concurrency-band",
            key: "agents.max_live".into(),
            value: n.clone(),
            scope: "global",
            reason: "the measured live-worker band".into(),
            source: "wizard",
        });
    }
    if let Some(Answer::Text(t)) = state.answers.get("review") {
        if t == "self_review" {
            rows.push(Proposal {
                id: "review-merge",
                key: "review.posture".into(),
                value: "self_review".into(),
                scope: "global",
                reason: "the review posture this run chose".into(),
                source: "wizard",
            });
        }
    }
    match state.answers.get("merge") {
        Some(Answer::Yes) => {
            rows.push(Proposal {
                id: "review-merge",
                key: "auto_merge.enabled".into(),
                value: "true".into(),
                scope: "global",
                reason: "merge on green review and CI".into(),
                source: "wizard",
            });
            rows.push(Proposal {
                id: "review-merge",
                key: "auto_merge.grant".into(),
                value: "dispatch".into(),
                scope: "global",
                reason: "the dispatch grant behind auto-merge".into(),
                source: "wizard",
            });
        }
        Some(Answer::No) => {
            // An explicit No must undo an existing auto-merge grant from
            // config or a joined profile: enabled=false gates the grant.
            rows.push(Proposal {
                id: "review-merge",
                key: "auto_merge.enabled".into(),
                value: "false".into(),
                scope: "global",
                reason: "the merge refusal this run recorded".into(),
                source: "wizard",
            });
        }
        _ => {}
    }
    if let Some(Answer::Text(app_id)) = state.answers.get("github-app") {
        rows.push(Proposal {
            id: "github-app",
            key: "github.app_id".into(),
            value: app_id.clone(),
            scope: "global",
            reason: "the fleet App this run created".into(),
            source: "wizard",
        });
    }
    // The interview's keyed rows; one nested constructor keeps the
    // struct-literal noise down across the four shapes.
    fn interview_row(key: &str, value: String, reason: &str) -> Proposal {
        Proposal {
            id: "interview",
            key: key.into(),
            value,
            scope: "global",
            reason: reason.into(),
            source: "wizard",
        }
    }
    if let Some(Answer::Text(t)) = state.answers.get("interview-lane") {
        if !t.is_empty() {
            rows.push(interview_row(
                "agents.defaults.provider",
                t.clone(),
                "the default spawn lane this run picked",
            ));
        }
    }
    if let Some(Answer::Picked(picked)) = state.answers.get("interview-review") {
        let reviewers: Vec<String> = picked
            .iter()
            .filter(|n| n.as_str() != REVIEW_BOT_LOGIN)
            .cloned()
            .collect();
        if !reviewers.is_empty() {
            rows.push(interview_row(
                "review.reviewers",
                toml_array(&reviewers),
                "the local review lane this run picked",
            ));
        }
        if picked.iter().any(|n| n.as_str() == REVIEW_BOT_LOGIN) {
            rows.push(interview_row(
                "review.github_apps",
                toml_array(&[REVIEW_BOT_LOGIN.to_string()]),
                "the review bot this run required",
            ));
        }
    }
    for (key, answer_key, reason) in [
        (
            "user.favorite_skills",
            "interview-favorites",
            "the skills this run marked as favorites",
        ),
        (
            "target.loop_skills",
            "interview-loop",
            "the skills this run expects the target loop to reach for",
        ),
    ] {
        if let Some(Answer::Picked(picked)) = state.answers.get(answer_key) {
            if !picked.is_empty() {
                rows.push(interview_row(key, toml_array(picked), reason));
            }
        }
    }
    if let Some(Answer::Picked(picked)) = state.answers.get("services") {
        for id in picked {
            let (id, key) = match id.as_str() {
                "pr-watcher" => ("pr-watcher", "pr_watch.enabled"),
                "auto-heal" => ("auto-heal", "auto_heal.enabled"),
                "groom-agent" => ("groom-agent", ""),
                _ => continue,
            };
            rows.push(Proposal {
                id,
                key: key.into(),
                value: "true".into(),
                scope: "global",
                reason: "the background service this run picked".into(),
                source: "wizard",
            });
        }
    }
    if let Some(Answer::Picked(picked)) = state.answers.get("messaging") {
        for kind in picked {
            rows.push(Proposal {
                id: "messaging",
                key: String::new(),
                value: kind.clone(),
                scope: "global",
                reason: "the chat channel this run picked; setup asks for its token".into(),
                source: "wizard",
            });
        }
    }
    if let Some(Answer::Yes) = state.answers.get("projects") {
        rows.push(Proposal {
            id: "projects",
            key: String::new(),
            value: String::new(),
            scope: "project",
            reason: "register this checkout".into(),
            source: "wizard",
        });
    }
    rows
}

/// One readable line for a proposed row. A row with a key reads `key = value`;
/// a runner-only row (no key) reads as the sentence for what it does.
pub fn row_line(row: &Proposal) -> String {
    if !row.key.is_empty() {
        return format!("{} = {}", row.key, row.value);
    }
    match row.id {
        "groom-agent" => "Run the backlog groom in the background".to_string(),
        "messaging" => format!("Set up messaging: {}", row.value),
        "projects" => "Register this checkout as a project".to_string(),
        "usage-report" => "Send the daily usage report".to_string(),
        other => format!("Run the {other} step"),
    }
}

/// Hold every proposed write that would replace a value the config already
/// carries behind one confirm line showing `old -> new`. A "no" (or a
/// cancel) drops the row, so the existing value stays; a row for a key the
/// config does not set yet, or sets to the same value, writes unasked.
pub fn confirm_overwrites(
    pr: &mut dyn Prompter,
    rows: Vec<Proposal>,
    current: &dyn Fn(&str) -> Option<String>,
) -> Vec<Proposal> {
    let mut kept = Vec::with_capacity(rows.len());
    for row in rows {
        let old = if row.scope == "global" {
            current(&row.key).filter(|old| *old != row.value)
        } else {
            None
        };
        let Some(old) = old else {
            kept.push(row);
            continue;
        };
        let prompt = format!("Change {}: {} -> {}?", row.key, old, row.value);
        if matches!(pr.confirm("confirm-changes", &prompt, false), Conf::Yes) {
            kept.push(row);
        } else {
            pr.note("confirm-changes", &format!("Kept {} = {}.", row.key, old));
        }
    }
    kept
}

/// What the skip door writes: the balanced preset's own answers, for every
/// key the config does not set already. A key the config sets stays as it is.
/// The rows carry the `review-merge` id because the apply step picks its runner
/// by id.
pub fn skip_defaults(current: &dyn Fn(&str) -> Option<String>) -> Vec<Proposal> {
    let rows = [
        (
            "auto_merge.enabled",
            "false",
            "the balanced preset leaves auto-merge off",
        ),
        (
            "review.posture",
            "self_review",
            "each agent reviews its own code before a PR opens",
        ),
    ];
    rows.iter()
        .filter(|(key, _, _)| current(key).is_none())
        .map(|(key, value, reason)| Proposal {
            id: "review-merge",
            key: (*key).into(),
            value: (*value).into(),
            scope: "global",
            reason: (*reason).into(),
            source: "wizard",
        })
        .collect()
}

#[cfg(test)]
#[path = "wizard_tests.rs"]
mod wizard_tests;
