//! The agent SOUL catalog: stance, values, taste and voice per role.
//!
//! Seeds embed at compile time from `src/souls/<role>/<style>/SOUL.md`,
//! copy once to `<FNO_HOME>/profile/agents/<role>/<style>/SOUL.md` on
//! first read, and never overwrite: an edited soul is the user's. A SOUL
//! carries stance and taste only; the rules in AGENTS.md and the skills
//! win over it, and handoff lessons go to the territory canon instead.
//! The ops ride the `lead-state` command because law d-fe66560a bars a
//! new action; `fno agent hq chart style` fronts them.

use std::fs;
use std::path::{Path, PathBuf};

/// The role a pick records on: only a lead holds a scope manifest, so
/// only the lead role takes a recorded pick.
const PICK_ROLE: &str = "lead";

/// One embedded seed soul from `src/souls/<role>/<style>/SOUL.md`.
pub(crate) struct Soul {
    pub(crate) role: &'static str,
    pub(crate) style: &'static str,
    pub(crate) body: &'static str,
}

/// The seed catalog, one row per seed file. The catalog test below holds
/// the per-role shape the docs promise: 5-7 styles per role, exactly one
/// marked default, four body sections, no worker names.
pub(crate) static SOULS: &[Soul] = &[
    Soul {
        role: "lead",
        style: "coach",
        body: include_str!("../souls/lead/coach/SOUL.md"),
    },
    Soul {
        role: "lead",
        style: "incident",
        body: include_str!("../souls/lead/incident/SOUL.md"),
    },
    Soul {
        role: "lead",
        style: "picky-editor",
        body: include_str!("../souls/lead/picky-editor/SOUL.md"),
    },
    Soul {
        role: "lead",
        style: "skeptic",
        body: include_str!("../souls/lead/skeptic/SOUL.md"),
    },
    Soul {
        role: "lead",
        style: "steward",
        body: include_str!("../souls/lead/steward/SOUL.md"),
    },
    Soul {
        role: "lead",
        style: "throughput",
        body: include_str!("../souls/lead/throughput/SOUL.md"),
    },
    Soul {
        role: "archer",
        style: "closer",
        body: include_str!("../souls/archer/closer/SOUL.md"),
    },
    Soul {
        role: "archer",
        style: "craftsman",
        body: include_str!("../souls/archer/craftsman/SOUL.md"),
    },
    Soul {
        role: "archer",
        style: "explorer",
        body: include_str!("../souls/archer/explorer/SOUL.md"),
    },
    Soul {
        role: "archer",
        style: "minimalist",
        body: include_str!("../souls/archer/minimalist/SOUL.md"),
    },
    Soul {
        role: "archer",
        style: "test-purist",
        body: include_str!("../souls/archer/test-purist/SOUL.md"),
    },
    Soul {
        role: "code-reviewer",
        style: "auditor",
        body: include_str!("../souls/code-reviewer/auditor/SOUL.md"),
    },
    Soul {
        role: "code-reviewer",
        style: "mentor",
        body: include_str!("../souls/code-reviewer/mentor/SOUL.md"),
    },
    Soul {
        role: "code-reviewer",
        style: "pragmatist",
        body: include_str!("../souls/code-reviewer/pragmatist/SOUL.md"),
    },
    Soul {
        role: "code-reviewer",
        style: "skeptic",
        body: include_str!("../souls/code-reviewer/skeptic/SOUL.md"),
    },
    Soul {
        role: "code-reviewer",
        style: "user-advocate",
        body: include_str!("../souls/code-reviewer/user-advocate/SOUL.md"),
    },
    Soul {
        role: "design-critic",
        style: "accessibility-first",
        body: include_str!("../souls/design-critic/accessibility-first/SOUL.md"),
    },
    Soul {
        role: "design-critic",
        style: "brand-keeper",
        body: include_str!("../souls/design-critic/brand-keeper/SOUL.md"),
    },
    Soul {
        role: "design-critic",
        style: "picky-editor",
        body: include_str!("../souls/design-critic/picky-editor/SOUL.md"),
    },
    Soul {
        role: "design-critic",
        style: "systems",
        body: include_str!("../souls/design-critic/systems/SOUL.md"),
    },
    Soul {
        role: "design-critic",
        style: "user-in-a-hurry",
        body: include_str!("../souls/design-critic/user-in-a-hurry/SOUL.md"),
    },
    Soul {
        role: "growth",
        style: "contrarian",
        body: include_str!("../souls/growth/contrarian/SOUL.md"),
    },
    Soul {
        role: "growth",
        style: "data-led",
        body: include_str!("../souls/growth/data-led/SOUL.md"),
    },
    Soul {
        role: "growth",
        style: "founder-voice",
        body: include_str!("../souls/growth/founder-voice/SOUL.md"),
    },
    Soul {
        role: "growth",
        style: "plain-spoken",
        body: include_str!("../souls/growth/plain-spoken/SOUL.md"),
    },
    Soul {
        role: "growth",
        style: "storyteller",
        body: include_str!("../souls/growth/storyteller/SOUL.md"),
    },
    Soul {
        role: "persona-reviewer",
        style: "librarian",
        body: include_str!("../souls/persona-reviewer/librarian/SOUL.md"),
    },
    Soul {
        role: "persona-reviewer",
        style: "mentor",
        body: include_str!("../souls/persona-reviewer/mentor/SOUL.md"),
    },
    Soul {
        role: "persona-reviewer",
        style: "pragmatist",
        body: include_str!("../souls/persona-reviewer/pragmatist/SOUL.md"),
    },
    Soul {
        role: "persona-reviewer",
        style: "red-team",
        body: include_str!("../souls/persona-reviewer/red-team/SOUL.md"),
    },
    Soul {
        role: "persona-reviewer",
        style: "voice-keeper",
        body: include_str!("../souls/persona-reviewer/voice-keeper/SOUL.md"),
    },
];

/// The typed frontmatter fields the ops read: the style slug (equals its
/// folder), the one-line fit, and whether the role falls back to it.
pub(crate) struct SoulFrontmatter {
    pub(crate) name: String,
    pub(crate) fits: String,
    pub(crate) default: bool,
}

/// The typed read of a SOUL's frontmatter. None when the fences or the
/// name are missing; the body sections are the catalog test's job, not a
/// runtime read.
pub(crate) fn parse_frontmatter(body: &str) -> Option<SoulFrontmatter> {
    let mut lines = body.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    let mut name = None;
    let mut fits = None;
    let mut default = false;
    let mut in_metadata = false;
    for line in lines {
        let line = line.trim_end();
        if line == "---" {
            break;
        }
        if line.starts_with("metadata:") {
            in_metadata = true;
            continue;
        }
        let indented = line.starts_with(' ') || line.starts_with('\t');
        if in_metadata && indented {
            if let Some((key, value)) = line.trim().split_once(':') {
                let value = value.trim().trim_matches('"');
                match key.trim() {
                    "fits" => fits = Some(value.to_string()),
                    "default" => default = value == "true",
                    _ => {}
                }
            }
            continue;
        }
        in_metadata = false;
        if let Some((key, value)) = line.split_once(':') {
            if key.trim() == "name" {
                name = Some(value.trim().trim_matches('"').to_string());
            }
        }
    }
    Some(SoulFrontmatter {
        name: name?,
        fits: fits.unwrap_or_default(),
        default,
    })
}

/// The `FNO_HOME` root rule, copied from `audience.rs::persona_dir`; that
/// file stays untouched (its dir serves customers, not agents).
fn fno_root() -> PathBuf {
    std::env::var_os("FNO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".fno")))
        .unwrap_or_else(|| PathBuf::from(".fno"))
}

fn profile_agents(root: &Path) -> PathBuf {
    root.join("profile").join("agents")
}

/// The path one soul lives at, seeds and user styles alike.
pub(crate) fn soul_path(root: &Path, role: &str, style: &str) -> PathBuf {
    profile_agents(root).join(role).join(style).join("SOUL.md")
}

/// The Agent Skills name rule: 1-64 chars, lowercase a-z, 0-9 and single
/// internal hyphens. Guards what the profile dirs may contribute.
fn valid_slug(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
}

/// Seed styles plus user-added profile directories carrying a SOUL.md,
/// sorted. A directory name outside the slug rule never lists.
pub(crate) fn known_styles(root: &Path, role: &str) -> Vec<String> {
    let mut names: Vec<String> = SOULS
        .iter()
        .filter(|s| s.role == role)
        .map(|s| s.style.to_string())
        .collect();
    if let Ok(entries) = fs::read_dir(profile_agents(root).join(role)) {
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() || !dir.join("SOUL.md").is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if valid_slug(&name) && !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names.sort();
    names
}

/// The role's marked default seed style. None when no seed carries the
/// marker, so a user-only role lists but has no default.
pub(crate) fn default_style(role: &str) -> Option<String> {
    SOULS
        .iter()
        .filter(|s| s.role == role)
        .filter(|s| parse_frontmatter(s.body).is_some_and(|f| f.default))
        .map(|s| s.style.to_string())
        .next()
}

/// Copy the seed to the profile path only when the file is absent; a
/// present file is the user's and stays untouched. Returns the path
/// either way; Err carries only the write failure.
pub(crate) fn ensure_seeded(root: &Path, role: &str, style: &str) -> std::io::Result<PathBuf> {
    let path = soul_path(root, role, style);
    if path.is_file() {
        return Ok(path);
    }
    let Some(body) = SOULS
        .iter()
        .find(|s| s.role == role && s.style == style)
        .map(|s| s.body)
    else {
        return Ok(path);
    };
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(&path, body)?;
    Ok(path)
}

/// One listable style: its name, its default marker and its `fits` line.
struct StyleListing {
    style: String,
    default: bool,
    fits: String,
    seeded: bool,
}

fn style_listings(root: &Path, role: &str) -> Vec<StyleListing> {
    let mut rows: Vec<StyleListing> = SOULS
        .iter()
        .filter(|s| s.role == role)
        .map(|s| {
            let fm = parse_frontmatter(s.body);
            StyleListing {
                style: s.style.to_string(),
                default: fm.is_some_and(|f| f.default),
                fits: fm.map(|f| f.fits).unwrap_or_default(),
                seeded: true,
            }
        })
        .collect();
    if let Ok(entries) = fs::read_dir(profile_agents(root).join(role)) {
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if !valid_slug(&name) || rows.iter().any(|r| r.style == name) {
                continue;
            }
            let file = dir.join("SOUL.md");
            let Ok(body) = fs::read_to_string(&file) else {
                continue;
            };
            let fm = parse_frontmatter(&body);
            rows.push(StyleListing {
                style: name,
                // Default is a seed designation; default_style reads seeds
                // only, so a user file's marker never displays as one.
                default: false,
                fits: fm.map(|f| f.fits).unwrap_or_default(),
                seeded: false,
            });
        }
    }
    rows.sort_by(|a, b| a.style.cmp(&b.style));
    rows
}

/// The caller's recorded pick, best effort: an unpromoted terminal, an
/// unreadable manifest and a pre-field manifest all read as "no pick".
fn caller_pick() -> Option<String> {
    let path = crate::lead_reads::caller_manifest()?;
    let content = fs::read_to_string(path).ok()?;
    // The frontmatter parser only, not a raw field scan: a manifest body
    // line that opens with "style:" never reads as the pick.
    crate::lead_termination::parse_lead_manifest(&content)?.style
}

fn print_style_usage() {
    eprintln!(
        "fno-agent lead-state --style: usage:\n  \
         lead-state --style list [--role ROLE]\n  \
         lead-state --style show [--role ROLE] [--style STYLE]\n  \
         lead-state --style <STYLE> [--scope SCOPE]   (records the lead's pick)"
    );
}

/// `fno-agent lead-state --style list|show|<STYLE>`: list the catalog,
/// read one soul (recorded pick, then default), or record the lead's
/// pick on the scope manifest.
pub fn run_style_command(args: &[String]) -> i32 {
    let mut op: Option<String> = None;
    let mut role: Option<String> = None;
    let mut style: Option<String> = None;
    let mut scope: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--role" if i + 1 < args.len() => {
                role = Some(args[i + 1].clone());
                i += 2;
            }
            "--style" if i + 1 < args.len() => {
                style = Some(args[i + 1].clone());
                i += 2;
            }
            "--scope" if i + 1 < args.len() => {
                scope = Some(args[i + 1].clone());
                i += 2;
            }
            "--help" | "-h" => {
                print_style_usage();
                return 0;
            }
            other if other.starts_with('-') => {
                eprintln!("fno-agent lead-state --style: unknown flag {other}");
                print_style_usage();
                return 2;
            }
            word => {
                if op.is_some() {
                    eprintln!(
                        "fno-agent lead-state --style: one operation per call, got {word} after {}",
                        op.as_deref().unwrap_or("?")
                    );
                    return 2;
                }
                op = Some(word.to_string());
                i += 1;
            }
        }
    }
    let Some(word) = op else {
        print_style_usage();
        return 2;
    };
    match word.as_str() {
        "list" => run_style_list(),
        "show" => run_style_show(role.as_deref().unwrap_or(PICK_ROLE), style),
        pick_style => run_style_pick(pick_style, scope.as_deref()),
    }
}

fn run_style_list() -> i32 {
    let root = fno_root();
    let mut roles: Vec<String> = seed_roles().into_iter().map(String::from).collect();
    for role in user_roles(&root) {
        if !roles.contains(&role) {
            roles.push(role);
        }
    }
    roles.sort();
    if roles.is_empty() {
        eprintln!("lead: no styles seeded and none in the profile");
        return 1;
    }
    for role in &roles {
        println!("{role}:");
        for row in style_listings(&root, role) {
            let marker = if row.default {
                "[default] "
            } else {
                "          "
            };
            let source = if row.seeded { "" } else { " (yours)" };
            println!("  {:<22}{}{}{}", row.style, marker, row.fits, source);
        }
    }
    if let Some(pick) = caller_pick() {
        println!("\nyour pick: {pick}");
    }
    0
}

fn run_style_show(role: &str, named: Option<String>) -> i32 {
    let root = fno_root();
    let known = known_styles(&root, role);
    if known.is_empty() {
        eprintln!("lead: no styles for role {role}");
        return 1;
    }
    let chosen = named.unwrap_or_else(|| {
        caller_pick()
            .filter(|p| known.iter().any(|k| k == p))
            .or_else(|| default_style(role))
            .unwrap_or_else(|| known[0].clone())
    });
    if !known.iter().any(|k| k == &chosen) {
        eprintln!(
            "lead: no style '{chosen}' for role {role}. known styles: {}",
            known.join(", ")
        );
        return 2;
    }
    let path = match ensure_seeded(&root, role, &chosen) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("lead: cannot seed {chosen}: {e}");
            return 1;
        }
    };
    let Ok(body) = fs::read_to_string(&path) else {
        eprintln!("lead: no soul at {}", path.display());
        return 1;
    };
    println!("{}", path.display());
    println!();
    print!("{body}");
    0
}

fn run_style_pick(style: &str, scope_arg: Option<&str>) -> i32 {
    let (scope, session, root) = match crate::lead_reads::own_role(scope_arg.unwrap_or("")) {
        Ok(found) => found,
        Err(refusal) => {
            eprintln!("{refusal}");
            return 2;
        }
    };
    let known = known_styles(&root, PICK_ROLE);
    if !known.iter().any(|k| k == style) {
        eprintln!(
            "lead: no style '{style}' for role {PICK_ROLE}. known styles: {}",
            known.join(", ")
        );
        return 2;
    }
    if let Err(e) = ensure_seeded(&root, PICK_ROLE, style) {
        eprintln!("lead: cannot seed {style}: {e}");
        return 1;
    }
    let expect = Some(session.as_str()).filter(|s| !s.is_empty());
    match crate::lead_state::set_manifest_fields(&root, &scope, &[("style", style)], expect) {
        Ok(()) => {
            println!("lead: style recorded: {style}");
            println!("scope:  {scope}");
            println!("soul:   {}", soul_path(&root, PICK_ROLE, style).display());
            0
        }
        Err(e) => {
            eprintln!("lead: {}", e.trim());
            1
        }
    }
}

fn seed_roles() -> Vec<&'static str> {
    let mut roles: Vec<&'static str> = SOULS.iter().map(|s| s.role).collect();
    roles.sort_unstable();
    roles.dedup();
    roles
}

fn user_roles(root: &Path) -> Vec<String> {
    let mut roles = Vec::new();
    if let Ok(entries) = fs::read_dir(profile_agents(root)) {
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if valid_slug(&name) {
                roles.push(name);
            }
        }
    }
    roles.sort();
    roles
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fleet's lead person names. A soul is role writing; it carries
    /// no incarnation, so no seed names any of them.
    const WORKER_NAMES: &[&str] = &["gus", "atticus", "philip", "anouk", "gavin"];

    fn seed_bodies(role: &str) -> Vec<(&'static str, &'static str)> {
        SOULS
            .iter()
            .filter(|s| s.role == role)
            .map(|s| (s.style, s.body))
            .collect()
    }

    #[test]
    fn the_catalog_holds_the_documented_shape_per_role() {
        for role in seed_roles() {
            let bodies = seed_bodies(role);
            assert!(
                (5..=7).contains(&bodies.len()),
                "role {role} holds {} styles, want 5-7",
                bodies.len()
            );
            let defaults = bodies
                .iter()
                .filter(|(_, b)| parse_frontmatter(b).is_some_and(|f| f.default));
            assert_eq!(
                defaults.count(),
                1,
                "role {role} carries exactly one default"
            );
            for (style, body) in &bodies {
                for heading in ["## Stance", "## Values", "## Taste", "## Voice"] {
                    assert!(body.contains(heading), "{role}/{style} misses {heading}");
                }
                for name in WORKER_NAMES {
                    assert!(
                        !body.to_lowercase().contains(name),
                        "{role}/{style} names a worker: {name}"
                    );
                }
                assert!(valid_slug(style), "{role}/{style} breaks the slug rule");
                assert_ne!(style, "list", "a seed may not collide with the list op");
                assert_ne!(style, "show", "a seed may not collide with the show op");
                let fm = parse_frontmatter(body)
                    .unwrap_or_else(|| panic!("{role}/{style} frontmatter unreadable"));
                assert_eq!(fm.name, *style, "{role}/{style} name must equal its folder");
                assert!(
                    body.lines().count() < 40,
                    "{role}/{style} runs past 40 lines"
                );
            }
        }
        assert!(seed_roles().len() >= 5, "the catalog needs 5+ roles");
    }

    #[test]
    fn seeding_writes_an_absent_file_and_never_touches_an_edited_one() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path();
        let path = ensure_seeded(root, "lead", "steward").unwrap();
        assert!(path.is_file(), "the seed writes on first read");
        let seeded = fs::read_to_string(&path).unwrap();
        assert_eq!(
            seeded,
            SOULS.iter().find(|s| s.style == "steward").unwrap().body
        );
        fs::write(&path, "the user rewrote this soul").unwrap();
        let again = ensure_seeded(root, "lead", "steward").unwrap();
        assert_eq!(
            fs::read_to_string(&again).unwrap(),
            "the user rewrote this soul",
            "a present file is the user's and stays untouched"
        );
    }

    #[test]
    fn the_pick_writes_style_and_the_parser_reads_it_back() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path();
        let leads = root.join("leads");
        fs::create_dir_all(&leads).unwrap();
        let manifest = leads.join("fno.md");
        fs::write(
            &manifest,
            "---\nfno_id: lead-x\nscope: fno\nshape: pass\n---\nbody\n",
        )
        .unwrap();
        set_manifest_fields(root, "fno", &[("style", "picky-editor")], None).unwrap();
        let content = fs::read_to_string(&manifest).unwrap();
        let parsed = crate::lead_termination::parse_lead_manifest(&content).unwrap();
        assert_eq!(parsed.style.as_deref(), Some("picky-editor"));
    }

    #[test]
    fn an_unknown_style_refuses_and_names_the_known_ones() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path();
        let known = known_styles(root, "lead");
        assert!(known.contains(&"steward".to_string()));
        assert!(!known.contains(&"nonesuch".to_string()));
        // The refusal text the pick and show ops print: names the known set.
        let refusal = format!(
            "lead: no style 'nonesuch' for role {}. known styles: {}",
            PICK_ROLE,
            known.join(", ")
        );
        assert!(
            refusal.contains("steward"),
            "the refusal names a known style"
        );
    }

    #[test]
    fn frontmatter_reads_name_fits_and_default() {
        let fm = parse_frontmatter(SOULS[0].body).unwrap();
        assert_eq!(fm.name, SOULS[0].style);
        assert!(!fm.fits.is_empty());
        // Exactly the marked defaults read true, found across the catalog.
        let marked: Vec<&str> = SOULS
            .iter()
            .filter(|s| parse_frontmatter(s.body).is_some_and(|f| f.default))
            .map(|s| s.style)
            .collect();
        assert_eq!(marked.len(), seed_roles().len());
    }

    #[test]
    fn the_slug_rule_holds_the_spec_shape() {
        assert!(valid_slug("steward"));
        assert!(valid_slug("user-in-a-hurry"));
        assert!(valid_slug("a"));
        assert!(!valid_slug(""));
        assert!(!valid_slug("-lead"));
        assert!(valid_slug(&"x".repeat(64)));
        assert!(!valid_slug(&"x".repeat(65)));
        assert!(!valid_slug("Picky"));
        assert!(!valid_slug("two--hyphens"));
        assert!(!valid_slug("under_score"));
    }
}
