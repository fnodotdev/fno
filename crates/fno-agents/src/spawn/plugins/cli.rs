//! `fno config plugins <command>`: `ls`, `inspect`, `verify`, `gallery` and
//! `open` read; `activate`, `deactivate`, `install`, `enable` and
//! `disable` write. Every command emits a
//! versioned `--json` envelope, on failure paths too, so the skill layer and
//! CI parse rather than scrape.
//!
//! The group options precede the command, as the retired Python group read
//! them: `--json`/`-J` and `--root PATH`. Usage errors keep the click wording
//! and exit 2.

use super::activate::{self, Failure};
use super::registry::Store;
use super::verify::{load_manifest, verify_pack};
use crate::roles::{context, registry::default_role_root};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const GROUP: &str = "fno config plugins";
const RESPONSE_VERSION: u64 = 1;

const GROUP_HELP: &str = "Usage: fno config plugins [OPTIONS] COMMAND [ARGS]...

  Install, verify, activate, and inspect function packs.

Options:
  -J, --json   Emit stable structured JSON.
  --root PATH  Role root (default: conventional).
  --help       Show this message and exit.

Commands:
  verify      Verify a pack on two axes; exit non-zero unless every...
  activate    Activate a verified pack into the plugin role layer.
  deactivate  Remove only the definition paths this pack's receipt recorded.
  ls          List installed packs with version, activation state, and...
  inspect     Print a pack's manifest with declarations labeled as...
  install     Install a pack from a git URL behind the consent card.
  enable      Switch on a pack, or one of its mods or layouts.
  disable     Switch off a pack, or one of its mods or layouts.
  gallery     List published packs from GitHub topics. Installs nothing.
  open        Show one mod in this terminal: a feed as a table, a command...
";

const OPTIONS_HELP: &str = "Options:
  -J, --json  Emit stable structured JSON.
  --help      Show this message and exit.
";

struct Command {
    name: &'static str,
    summary: &'static str,
    /// The positional's metavar and help, or none.
    argument: Option<(&'static str, &'static str)>,
}

const COMMANDS: [Command; 10] = [
    Command {
        name: "verify",
        summary: "Verify a pack on two axes; exit non-zero unless every condition passed.",
        argument: Some(("PATH", "Pack directory or plugin.yaml file.")),
    },
    Command {
        name: "activate",
        summary: "Activate a verified pack into the plugin role layer. Grants nothing.",
        argument: Some(("PATH", "Pack directory or plugin.yaml file.")),
    },
    Command {
        name: "deactivate",
        summary: "Remove only the definition paths this pack's receipt recorded.",
        argument: Some(("PACK_ID", "Installed pack id.")),
    },
    Command {
        name: "ls",
        summary:
            "List installed packs with version, activation state, and declared effect\n  ceiling.",
        argument: None,
    },
    Command {
        name: "inspect",
        summary: "Print a pack's manifest with declarations labeled as declarations.",
        argument: Some(("PACK_ID", "Installed pack id.")),
    },
    Command {
        name: "install",
        summary: "Install a pack from a git URL behind the consent card.",
        argument: Some(("URL", "A git URL the pack's repo clones from.")),
    },
    Command {
        name: "enable",
        summary: "Switch on a pack, or one of its mods or layouts.",
        argument: Some(("TARGET", "PACK_ID or PACK_ID/COMPONENT_ID.")),
    },
    Command {
        name: "disable",
        summary: "Switch off a pack, or one of its mods or layouts.",
        argument: Some(("TARGET", "PACK_ID or PACK_ID/COMPONENT_ID.")),
    },
    Command {
        name: "gallery",
        summary: "List published packs from GitHub topics. Installs nothing.",
        argument: None,
    },
    Command {
        name: "open",
        summary: "Show one mod in this terminal: a feed as a table, a command mod after its digest check.",
        argument: Some(("TARGET", "PACK_ID/MOD_ID.")),
    },
];

fn usage_line(command: &Command) -> String {
    match command.argument {
        Some((metavar, _)) => format!("{GROUP} {} [OPTIONS] {metavar}", command.name),
        None => format!("{GROUP} {} [OPTIONS]", command.name),
    }
}

fn command_help(command: &Command) -> String {
    let mut out = format!(
        "Usage: {}\n\n  {}\n\n",
        usage_line(command),
        command.summary
    );
    if let Some((metavar, help)) = command.argument {
        out.push_str(&format!("Arguments:\n  {metavar}  {help}  [required]\n\n"));
    }
    out.push_str(OPTIONS_HELP);
    out
}

/// A click usage error on stderr, exit 2.
fn usage_error(usage: &str, help_command: &str, message: &str) -> i32 {
    eprintln!("Usage: {usage}\nTry '{help_command} --help' for help.\n\nError: {message}");
    2
}

fn group_usage_error(message: &str) -> i32 {
    usage_error(
        &format!("{GROUP} [OPTIONS] COMMAND [ARGS]..."),
        GROUP,
        message,
    )
}

fn envelope(payload: Value) -> Value {
    let mut payload = payload;
    if let Value::Object(map) = &mut payload {
        // The payload's own `version` (a pack version) outranks the envelope's.
        map.entry("version").or_insert(json!(RESPONSE_VERSION));
    }
    payload
}

fn emit_json(value: &Value) {
    println!(
        "{}",
        crate::roles::cli::python_json(&crate::backlog::decisions_cli::sort_keys(value))
    );
}

struct Ctx {
    json: bool,
    root: PathBuf,
}

impl Ctx {
    /// Keep the advertised JSON interface on failure paths too.
    fn error(&self, message: &str) -> i32 {
        if self.json {
            emit_json(&envelope(json!({"error": message})));
        } else {
            eprintln!("{message}");
        }
        1
    }

    fn store(&self) -> Store {
        Store::at_root(&self.root)
    }
}

pub fn run(args: &[String]) -> i32 {
    let mut json_flag = false;
    let mut root: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "--json" | "-J" => json_flag = true,
            "--help" => {
                print!("{GROUP_HELP}");
                return 0;
            }
            "--root" => {
                i += 1;
                match args.get(i) {
                    Some(value) => root = Some(PathBuf::from(value)),
                    None => {
                        eprintln!("Error: Option '--root' requires an argument.");
                        return 2;
                    }
                }
            }
            a if a.starts_with("--root=") => {
                root = Some(PathBuf::from(&a["--root=".len()..]));
            }
            a if a.starts_with('-') => return group_usage_error(&format!("No such option: {a}")),
            _ => break,
        }
        i += 1;
    }
    let Some(name) = args.get(i) else {
        print!("{GROUP_HELP}");
        return 2;
    };
    let Some(command) = COMMANDS.iter().find(|c| c.name == name) else {
        return group_usage_error(&format!("No such command {}.", quoted(name)));
    };
    let mut json = json_flag;
    let mut positional: Vec<&str> = Vec::new();
    let mut options_done = false;
    for arg in &args[i + 1..] {
        let arg = arg.as_str();
        if options_done {
            positional.push(arg);
            continue;
        }
        match arg {
            "--" => options_done = true,
            "--json" | "-J" => json = true,
            "--help" => {
                print!("{}", command_help(command));
                return 0;
            }
            a if a.starts_with('-') && a.len() > 1 => {
                return usage_error(
                    &usage_line(command),
                    &format!("{GROUP} {}", command.name),
                    &format!("No such option: {a}"),
                )
            }
            a => positional.push(a),
        }
    }
    let wanted = usize::from(command.argument.is_some());
    if positional.len() < wanted {
        let (metavar, _) = command.argument.expect("wanted one argument");
        return usage_error(
            &usage_line(command),
            &format!("{GROUP} {}", command.name),
            &format!("Missing argument {}.", quoted(metavar)),
        );
    }
    if positional.len() > wanted {
        let extras = &positional[wanted..];
        let plural = if extras.len() > 1 { "s" } else { "" };
        return usage_error(
            &usage_line(command),
            &format!("{GROUP} {}", command.name),
            &format!(
                "Got unexpected extra argument{plural} ({})",
                extras.join(" ")
            ),
        );
    }
    let ctx = Ctx {
        json,
        root: root.unwrap_or_else(default_role_root),
    };
    let argument = positional.first().copied().unwrap_or_default();
    match command.name {
        "verify" => verify(&ctx, argument),
        "activate" => activate_command(&ctx, argument),
        "deactivate" => deactivate(&ctx, argument),
        "ls" => list(&ctx),
        "install" => super::install::run(ctx.json, &ctx.root, argument),
        "enable" => toggle(&ctx, argument, true),
        "disable" => toggle(&ctx, argument, false),
        "gallery" => super::gallery::run(ctx.json),
        "open" => super::open::run(&ctx.root, argument),
        _ => inspect(&ctx, argument),
    }
}

fn quoted(text: &str) -> String {
    format!("'{text}'")
}

fn verify(ctx: &Ctx, path: &str) -> i32 {
    let installed = match ctx.store().installed_index() {
        Ok(installed) => installed,
        Err(e) => {
            return ctx.error(&format!(
                "registry corrupt; cannot verify dependencies: {}",
                e.0
            ))
        }
    };
    let report = verify_pack(&crate::plans_path::expanduser(path), &installed);
    if ctx.json {
        emit_json(&envelope(report.to_json()));
    } else {
        println!("pack: {}", report.pack_path);
        for c in &report.conditions {
            let checked = if c.checked { "checked" } else { "unchecked" };
            println!(
                "  {:<13} {:<28} {:<9} {}",
                c.family, c.name, checked, c.result
            );
            if let Some(detail) = c.detail.as_deref().filter(|d| !d.is_empty()) {
                if c.result != "passed" {
                    println!("      {detail}");
                }
            }
        }
        println!("ok: {}", if report.ok() { "True" } else { "False" });
    }
    i32::from(!report.ok())
}

fn activate_command(ctx: &Ctx, path: &str) -> i32 {
    let configured: Vec<String> = match context::configured_artifacts(&context::repo_root()) {
        Ok(artifacts) => artifacts.into_iter().map(|a| a.identifier).collect(),
        Err(e) => return ctx.error(&format!("activation failed: {e}")),
    };
    let outcome = match activate::activate(
        &crate::plans_path::expanduser(path),
        &ctx.store(),
        &ctx.root,
        &configured,
    ) {
        Ok(outcome) => outcome,
        Err(Failure::Refused(refusal)) => {
            return ctx.error(&format!("refused ({}): {}", refusal.reason, refusal.detail))
        }
        Err(Failure::Error(e)) => return ctx.error(&format!("activation failed: {e}")),
    };
    let receipt = &outcome.receipt;
    if ctx.json {
        emit_json(&envelope(json!({
            "pack_id": receipt.pack_id,
            "pack_digest": receipt.pack_digest,
            "resolved_version": receipt.resolved_version,
            "written_paths": receipt.written_paths,
            "already_active": outcome.already_active,
            "unconfigured_artifacts": outcome.unconfigured.iter().map(|u| json!({
                "identifier": u.identifier,
                "example_source": u.example_source,
            })).collect::<Vec<_>>(),
        })));
        return 0;
    }
    let label = if outcome.already_active {
        "already active"
    } else {
        "activated"
    };
    println!(
        "{label} {} {} digest={}",
        receipt.pack_id,
        receipt.resolved_version,
        &receipt.pack_digest[..12]
    );
    for written in &receipt.written_paths {
        println!("  wrote {written}");
    }
    for item in &outcome.unconfigured {
        let example = item
            .example_source
            .as_ref()
            .map(|source| format!(" (pack example: {source})"))
            .unwrap_or_default();
        println!(
            "  unconfigured artifact '{}' - add it under [context.artifacts] in .fno/config.toml or resolution will block{example}",
            item.identifier
        );
    }
    0
}

fn deactivate(ctx: &Ctx, pack_id: &str) -> i32 {
    let outcome = match activate::deactivate(pack_id, &ctx.store(), &ctx.root) {
        Ok(outcome) => outcome,
        Err(message) => return ctx.error(&message),
    };
    if ctx.json {
        emit_json(&envelope(json!({
            "pack_id": pack_id,
            "removed": outcome.removed,
            "left_alone": outcome.left_alone,
        })));
    } else {
        println!(
            "deactivated {pack_id}: removed {} definition(s)",
            outcome.removed.len()
        );
        for left in &outcome.left_alone {
            println!("  left alone (receipt retained): {left}");
        }
    }
    // A partial deactivation leaves files behind under a residual receipt:
    // not a clean success.
    i32::from(!outcome.left_alone.is_empty())
}

fn list(ctx: &Ctx) -> i32 {
    let registry = match ctx.store().load() {
        Ok(registry) => registry,
        Err(e) => return ctx.error(&format!("registry corrupt; cannot list packs: {}", e.0)),
    };
    let active = |pack_id: &str| registry.receipt_for(pack_id).is_some();
    if ctx.json {
        let packs: Vec<Value> = registry
            .packs
            .iter()
            .map(|pack| {
                let mut row = json!({
                    "pack_id": pack.pack_id,
                    "version": pack.resolved_version,
                    "activated": active(&pack.pack_id),
                    "digest": &pack.pack_digest[..12],
                    "declared_effects": pack.declared_effects.iter().map(|e| e.to_json()).collect::<Vec<_>>(),
                });
                if let Value::Object(map) = &mut row {
                    map.extend(ui_rows(pack));
                }
                row
            })
            .collect();
        emit_json(&envelope(json!({"packs": packs})));
        return 0;
    }
    if registry.packs.is_empty() {
        println!("no installed packs");
        return 0;
    }
    for pack in &registry.packs {
        let state = if active(&pack.pack_id) {
            "active"
        } else {
            "installed"
        };
        let effects = pack
            .declared_effects
            .iter()
            .map(|e| format!("{}->{}", e.effect_class, e.destination))
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "{} {} {state} digest={} effects=[{}]",
            pack.pack_id,
            pack.resolved_version,
            &pack.pack_digest[..12],
            if effects.is_empty() {
                "none"
            } else {
                effects.as_str()
            }
        );
    }
    0
}

fn inspect(ctx: &Ctx, pack_id: &str) -> i32 {
    let registry = match ctx.store().load() {
        Ok(registry) => registry,
        Err(e) => return ctx.error(&format!("registry corrupt; cannot inspect: {}", e.0)),
    };
    let Some(pack) = registry.pack_by_id(pack_id) else {
        return ctx.error(&format!(
            "no installed pack {}",
            crate::client_commands::py_repr_str(pack_id)
        ));
    };
    let Ok(manifest) = load_manifest(Path::new(&pack.manifest_path)) else {
        return ctx.error(&format!("manifest for {pack_id} no longer readable"));
    };
    let roles: Vec<&str> = manifest.roles.iter().map(|r| r.role.id.as_str()).collect();
    let digest_matches = manifest.digest() == pack.pack_digest;
    if ctx.json {
        emit_json(&envelope(json!({
            "pack_id": manifest.id,
            "version": manifest.version,
            "manifest_path": pack.manifest_path,
            "installed_digest": pack.pack_digest,
            "digest_matches_install": digest_matches,
            "declared_roles": roles,
            "declared_permissions": manifest.permissions.iter().map(|e| e.to_json()).collect::<Vec<_>>(),
            "declared_adapters": manifest.adapters.iter().map(|a| json!({
                "id": a.id,
                "destination": a.destination,
                "conformance": super::manifest::conformance_json(&a.conformance),
            })).collect::<Vec<_>>(),
        })));
        return 0;
    }
    println!("{} {}", manifest.id, manifest.version);
    println!(
        "  declares roles: {}",
        if roles.is_empty() {
            "none".to_string()
        } else {
            roles.join(", ")
        }
    );
    if !digest_matches {
        eprintln!(
            "  WARNING: manifest digest no longer matches installed record {}",
            &pack.pack_digest[..12]
        );
    }
    for effect in &manifest.permissions {
        println!(
            "  declares effect ceiling: {} -> {}",
            effect.effect_class, effect.destination
        );
    }
    0
}

/// The UI keys `ls --json` adds per pack, all additive: `mods`, `layouts`,
/// `source`, and `manifest_error` when the manifest no longer loads.
fn ui_rows(pack: &super::registry::InstalledPack) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    out.insert("source".into(), json!(pack.source));
    let manifest_path = Path::new(&pack.manifest_path);
    let base = manifest_path.parent().unwrap_or(Path::new(""));
    let abs = |rel: &str| super::resolve_path(&base.join(rel)).to_string_lossy().into_owned();
    let (mods, layouts) = match load_manifest(manifest_path) {
        Ok(manifest) => (
            manifest
                .mods
                .iter()
                .map(|m| {
                    let kind = match m.render() {
                        Some(super::ui::Render::Run(_)) => "run",
                        Some(super::ui::Render::Feed(_)) => "feed",
                        Some(super::ui::Render::Native(_)) => "native",
                        None => "invalid",
                    };
                    json!({
                        "id": m.id,
                        "title": m.title,
                        "summary": m.summary,
                        "preview": abs(&m.preview),
                        "recipe": abs(&m.recipe),
                        "place": m.place,
                        "kind": kind,
                        "native": m.native,
                        "runs_code": m.runs_code(),
                        "enabled": pack.enabled(&m.id, m.runs_code()),
                    })
                })
                .collect::<Vec<_>>(),
            manifest
                .layouts
                .iter()
                .map(|l| {
                    json!({
                        "id": l.id,
                        "title": l.title,
                        "summary": l.summary,
                        "preview": abs(&l.preview),
                        "recipe": abs(&l.recipe),
                        "enabled": pack.enabled(&l.id, false),
                    })
                })
                .collect::<Vec<_>>(),
        ),
        Err(failure) => {
            out.insert("manifest_error".into(), json!(failure.detail));
            (Vec::new(), Vec::new())
        }
    };
    out.insert("mods".into(), json!(mods));
    out.insert("layouts".into(), json!(layouts));
    out
}

/// `enable`/`disable PACK[/ID]`. A bare pack id switches every component.
/// An unknown pack or component exits 2 and names what is installed.
fn toggle(ctx: &Ctx, target: &str, on: bool) -> i32 {
    let store = ctx.store();
    let _held = match store.lock() {
        Ok(held) => held,
        Err(e) => return ctx.error(&format!("registry lock failed: {e}")),
    };
    let mut registry = match store.load() {
        Ok(registry) => registry,
        Err(e) => return ctx.error(&format!("registry corrupt; cannot switch: {}", e.0)),
    };
    let (pack_id, component) = match target.split_once('/') {
        Some((pack, id)) => (pack, Some(id)),
        None => (target, None),
    };
    let known: Vec<String> = registry.packs.iter().map(|p| p.pack_id.clone()).collect();
    let Some(pack) = registry.pack_by_id_mut(pack_id) else {
        ctx.error(&format!(
            "no installed pack {}; installed: {}",
            quoted(pack_id),
            if known.is_empty() { "none".to_string() } else { known.join(", ") }
        ));
        return 2;
    };
    let ids: Vec<String> = match load_manifest(Path::new(&pack.manifest_path)) {
        Ok(manifest) => manifest
            .mods
            .iter()
            .map(|m| m.id.clone())
            .chain(manifest.layouts.iter().map(|l| l.id.clone()))
            .collect(),
        Err(_) => return ctx.error(&format!("manifest for {pack_id} no longer readable")),
    };
    let chosen: Vec<String> = match component {
        None => ids.clone(),
        Some(id) if ids.iter().any(|known| known == id) => vec![id.to_string()],
        Some(id) => {
            ctx.error(&format!(
                "pack {pack_id} declares no mod or layout {}; declared: {}",
                quoted(id),
                if ids.is_empty() { "none".to_string() } else { ids.join(", ") }
            ));
            return 2;
        }
    };
    for id in &chosen {
        pack.toggles.insert(id.clone(), on);
    }
    if let Err(e) = store.save(&registry) {
        return ctx.error(&format!("registry write failed: {e}"));
    }
    let state = if on { "on" } else { "off" };
    if ctx.json {
        emit_json(&envelope(json!({"pack_id": pack_id, "switched": chosen, "enabled": on})));
    } else {
        for id in &chosen {
            println!("{pack_id}/{id} {state}");
        }
    }
    0
}
