//! The will-run line: the literal `fno agents spawn` command Enter runs,
//! rendered from the same request builder the launch uses, and its edit-back.
//! An edit parses into the chips and pills; nothing else writes an axis.

use super::*;
use crate::dispatch_launch::COMPOSER_FLAGS;

/// The dim note after a worktree launch: the server picks the path after
/// Enter, so the line cannot show it.
const WORKTREE_NOTE: &str = " \u{b7} in a new worktree";

/// The line while it holds the keyboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WillEdit {
    pub text: String,
    pub orig: String,
    pub error: Option<String>,
}

/// The command the card would run, or the reason it would refuse. Both ride
/// `prepare`, so the line and Enter can never disagree.
pub(crate) fn line(view: &View) -> Result<String, String> {
    let request = prepare_for(view, 0, false)?;
    let argv = crate::dispatch_launch::launch_spawn_argv("fno", &request, &view.session);
    let at_default = view
        .launcher
        .as_ref()
        .is_some_and(|l| l.draft.project_idx == 0);
    let argv = drop_workspace_cwd(argv, at_default);
    let mut text = shlex::try_join(argv.iter().map(String::as_str)).map_err(|e| e.to_string())?;
    if let Some(home) = std::env::var("HOME").ok().filter(|h| h.len() > 1) {
        text = text.replace(&format!(" {home}"), " ~");
    }
    if request.worktree && request.host.is_none() {
        text.push_str(WORKTREE_NOTE);
    }
    Ok(text)
}

/// The workspace's own folder is where the card opens, so the line leaves the
/// `--cwd` pair out and the clip keeps the flags that differ.
fn drop_workspace_cwd(mut argv: Vec<String>, at_default: bool) -> Vec<String> {
    if at_default {
        if let Some(at) = argv.iter().position(|a| a == "--cwd") {
            argv.drain(at..(at + 2).min(argv.len()));
        }
    }
    argv
}

/// Start editing: the text begins as the rendered line, or empty when the
/// card would refuse.
pub(crate) fn begin(view: &mut View) {
    let text = line(view).unwrap_or_default();
    if let Some(l) = view.launcher.as_mut() {
        l.will_edit = Some(WillEdit {
            orig: text.clone(),
            text,
            error: None,
        });
    }
}

fn want(args: &mut std::slice::Iter<'_, String>, flag: &str) -> Result<String, String> {
    args.next()
        .cloned()
        .ok_or_else(|| format!("{flag} needs a value"))
}

/// Apply an edited line. An unchanged line parses nothing. A bad value
/// refuses and leaves every chip as it was.
pub(crate) fn apply(
    l: &mut Launcher,
    catalog: &Option<CatalogOutcome>,
    typed: &str,
) -> Result<(), String> {
    let text = typed.strip_suffix(WORKTREE_NOTE).unwrap_or(typed);
    let argv = shlex::split(text).ok_or_else(|| "unbalanced quote in the command".to_string())?;
    if argv.get(1).map(String::as_str) != Some("agents")
        || argv.get(2).map(String::as_str) != Some("spawn")
    {
        return Err("the line must start with: fno agents spawn".to_string());
    }
    let mut d = l.draft.clone();
    d.model.clear();
    d.model_row = None;
    d.route.clear();
    d.provider.clear();
    d.effort.clear();
    d.permission.clear();
    d.pills.clear();
    d.placement = Placement::Thread;
    let rows = match catalog {
        Some(CatalogOutcome::Ok(rows, _, _)) => rows.as_slice(),
        _ => &[],
    };
    let mut args = argv.get(3..).unwrap_or_default().iter();
    while let Some(flag) = args.next() {
        let (name, inline) = match flag.split_once('=') {
            Some((n, v)) if flag.starts_with("--") => (n, Some(v.to_string())),
            _ => (flag.as_str(), None),
        };
        let mut value = |f: &str| -> Result<String, String> {
            match inline.clone() {
                Some(v) => Ok(v),
                None => want(&mut args, f),
            }
        };
        match name {
            "--no-wait" => {}
            // A remote launch renders its host as `--name @host`; the host
            // chip already owns it, so it must not come back as a pill.
            "--name" if d.host.is_some() => {
                let v = value(name)?;
                if !v.starts_with('@') {
                    d.pills.push((name.to_string(), Some(v)));
                }
            }
            "--mux-session" | "--prompt-file" | "--node" | "--portal" => {
                value(name)?;
            }
            "--harness" | "-H" => {
                let v = value(name)?;
                let at = d
                    .harnesses
                    .iter()
                    .position(|h| *h == v)
                    .ok_or_else(|| format!("harness {v} is not installed"))?;
                d.harness_idx = at;
            }
            "--model" | "-m" | "--route" => {
                let v = value(name)?;
                let harness = d.harness();
                let hit = rows.iter().find(|r| r.name == harness).and_then(|r| {
                    r.models
                        .iter()
                        .chain(r.more.iter())
                        .find(|m| m.model == v || m.route == v || m.name == v)
                });
                let m = hit.ok_or_else(|| format!("model {v} is not in the {harness} list"))?;
                d.model = m.model.clone();
                d.model_row = Some(m.name.clone());
                d.route = m.route.clone();
                d.provider = m.provider.clone().unwrap_or_default();
            }
            "--effort" => {
                let v = value(name)?;
                let harness = d.harness();
                let offered = rows
                    .iter()
                    .find(|r| r.name == harness)
                    .and_then(|r| r.efforts.as_ref());
                match offered {
                    Some(list) if list.is_empty() || list.contains(&v) => d.effort = v,
                    _ => return Err(format!("effort {v} is not offered by {harness}")),
                }
            }
            "--permission-mode" => {
                let v = value(name)?;
                let harness = d.harness();
                let offered = rows
                    .iter()
                    .find(|r| r.name == harness)
                    .and_then(|r| r.permission_modes.as_ref());
                match offered {
                    Some(list) if list.is_empty() || list.contains(&v) => d.permission = v,
                    _ => return Err(format!("mode {v} is not offered by {harness}")),
                }
            }
            "--cwd" => {
                let mut v = value(name)?;
                if let (Some(rest), Ok(home)) = (v.strip_prefix('~'), std::env::var("HOME")) {
                    v = format!("{home}{rest}");
                }
                d.project_idx =
                    d.projects.iter().position(|p| *p == v).ok_or_else(|| {
                        format!("folder {v} is not one of this workspace's folders")
                    })?;
            }
            "--as" => {
                if value(name)? == "pane" {
                    d.placement = Placement::PaneActiveTab;
                }
            }
            "--tab" => {
                if value(name)? == "new" {
                    d.placement = Placement::ThreadNewTab;
                }
            }
            "--split" => {
                value(name)?;
                d.placement = Placement::ThreadSplitBeside;
            }
            other if COMPOSER_FLAGS.contains(&other) => {
                return Err(format!("{other} cannot be changed on this line"));
            }
            other => {
                let takes = args.as_slice().first().is_some_and(|n| !n.starts_with('-'));
                let v = if inline.is_some() {
                    inline
                } else if takes {
                    args.next().cloned()
                } else {
                    None
                };
                d.pills.push((other.to_string(), v));
            }
        }
    }
    d.bump();
    l.draft = d;
    Ok(())
}
