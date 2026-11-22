//! Thread placement: which flags ask for a portal, whether the mux would
//! take it, and the reach after the birth.
//!
//! A thread's split is a portal, and a placement flag is the gesture that
//! opens one. The check runs before a worker exists, so a placement the mux
//! refuses starts nothing. After the birth a failed placement never changes
//! the exit code: a nonzero there reads as a spawn failure to a retrying
//! caller, which starts a duplicate worker.

use serde_json::Value;
use std::path::PathBuf;

/// Seconds the mux gets to answer a check.
const CHECK_SECS: u64 = 10;
/// Seconds the mux gets to open a portal. A viewer can take a moment to start.
const REACH_SECS: u64 = 30;
/// The exit `fno mux thread --check` gives a placement the server refuses.
const REFUSED: i32 = 2;

/// Why a spawn's placement cannot be honored. Each is found before a birth.
#[derive(Debug, thiserror::Error)]
pub enum PlacementRefusal {
    #[error("--workspace/-s needs a nonblank workspace name")]
    BlankWorkspace,
    #[error(
        "--portal applies only to the thread lane; a pane hosts its own geometry and \
         headless hosts no session at all"
    )]
    PortalOffThread,
    #[error("--at needs --as pane; on the thread lane name the cell with --from")]
    AtOnThread,
    #[error("--bounded-placement needs --as pane; a portal takes no placement lease")]
    BoundedPortal,
    #[error(
        "--workspace/-s, --split/-x, --at, --tab and --from apply only to the pane and \
         thread lanes (headless and cloud have no pane geometry)"
    )]
    NoGeometry,
    #[error("--split/-x must be left, right, up, or down (got {0:?})")]
    Direction(String),
    #[error("--tab needs a nonblank selector or pane-group name")]
    BlankTab,
    #[error("--tab new opens one pane, so drop --tab new or --split")]
    NewTabSplit,
    #[error("fno agent spawn: no worker started: {0}")]
    Mux(String),
}

/// The placement block of the spawn params. A field of an unexpected type
/// reads as a spawn with no placement, as an absent key does.
#[derive(Debug, Default, serde::Deserialize)]
struct Placement {
    squad: Option<String>,
    split: Option<String>,
    at: Option<String>,
    tab: Option<String>,
    from: Option<String>,
    portal: Option<u8>,
    portal_new: Option<bool>,
    bounded_placement: Option<bool>,
    cwd: Option<String>,
    here: Option<bool>,
    spawned_by_session: Option<String>,
}

impl Placement {
    fn read(params: &Value) -> Self {
        serde_json::from_value(params.clone()).unwrap_or_default()
    }

    fn names_geometry(&self) -> bool {
        [&self.squad, &self.split, &self.at, &self.tab, &self.from]
            .iter()
            .any(|flag| flag.is_some())
    }

    /// The word for `--portal`: an index, or `new` for the server's pick.
    /// This caller never computes an index from a render.
    fn portal_word(&self) -> Option<String> {
        match self.portal {
            Some(index) => Some(index.to_string()),
            None => (self.portal_new == Some(true)).then(|| "new".to_string()),
        }
    }

    /// Answers whether the flags imply a new portal.
    fn admit(&self, substrate: &str) -> Result<bool, PlacementRefusal> {
        if self.squad.as_deref().is_some_and(|s| s.trim().is_empty()) {
            return Err(PlacementRefusal::BlankWorkspace);
        }
        let thread = substrate == "bg";
        let names_portal = self.portal_word().is_some();
        if names_portal && !thread {
            return Err(PlacementRefusal::PortalOffThread);
        }
        if thread && self.at.is_some() {
            return Err(PlacementRefusal::AtOnThread);
        }
        let geometry = self.names_geometry();
        if geometry && !thread && substrate != "pane" {
            return Err(PlacementRefusal::NoGeometry);
        }
        if let Some(dir) = self.split.as_deref() {
            if !matches!(dir, "left" | "right" | "up" | "down") {
                return Err(PlacementRefusal::Direction(dir.to_string()));
            }
        }
        if self.tab.as_deref().is_some_and(|t| t.trim().is_empty()) {
            return Err(PlacementRefusal::BlankTab);
        }
        if !thread {
            return Ok(false);
        }
        if self.tab.as_deref().map(str::trim) == Some("new") && self.split.is_some() {
            return Err(PlacementRefusal::NewTabSplit);
        }
        if (names_portal || geometry) && self.bounded_placement == Some(true) {
            return Err(PlacementRefusal::BoundedPortal);
        }
        Ok(geometry && !names_portal)
    }

    /// `mux thread` words for this placement. `--from current` rides as
    /// typed: the child inherits FNO_PANE and resolves it.
    fn thread_args(&self, name: &str, portal: &str) -> Vec<String> {
        let mut args = vec!["mux".to_string(), "thread".to_string()];
        if !name.is_empty() {
            args.push(name.to_string());
        }
        args.extend(["--portal".to_string(), portal.to_string()]);
        for (flag, value) in [
            ("--workspace", &self.squad),
            ("--tab", &self.tab),
            ("--split", &self.split),
            ("--from", &self.from),
        ] {
            if let Some(value) = value.as_deref().filter(|v| !v.is_empty()) {
                args.extend([flag.to_string(), value.to_string()]);
            }
        }
        args
    }

    /// Who asks, for a thread with no row here: the folder it runs in (the
    /// caller's for a worker on another machine) and the spawning session.
    fn origin_args(&self, params: &Value, remote: bool) -> Vec<String> {
        let mut args = Vec::new();
        if let Some(cwd) = self.origin_cwd(params, remote) {
            args.extend(["--cwd".to_string(), cwd.to_string_lossy().into_owned()]);
        }
        if let Some(parent) = self.spawned_by_session.as_deref().filter(|p| !p.is_empty()) {
            args.extend(["--parent".to_string(), parent.to_string()]);
        }
        args
    }

    fn origin_cwd(&self, params: &Value, remote: bool) -> Option<PathBuf> {
        let caller = std::env::current_dir().ok()?;
        if remote {
            return Some(caller);
        }
        if let Some(explicit) = self.cwd.as_deref() {
            return Some(caller.join(explicit));
        }
        if self.here == Some(true) {
            return Some(caller);
        }
        Some(match crate::node_seed::spawn_node_cwd(params, &caller) {
            crate::node_seed::SpawnNodeCwd::Project(project) => project,
            _ => crate::paths::canonical_repo_root(&caller).unwrap_or(caller),
        })
    }
}

/// Validate the placement flags of a spawn. On the thread lane a placement
/// flag with no `--portal` asks for a new portal.
pub fn admit(params: &mut Value, substrate: &str) -> Result<(), PlacementRefusal> {
    if Placement::read(params).admit(substrate)? {
        if let Some(map) = params.as_object_mut() {
            map.insert("portal_new".into(), Value::Bool(true));
        }
    }
    Ok(())
}

/// Ask the mux whether the placement would land, before a worker starts.
/// `host` is the machine a relay picked, pinned or least-loaded, and `None`
/// is this one. Only a refusal from the mux stops the spawn: with no server,
/// an older one or no answer, the spawn goes on and the reach after the birth
/// reports.
pub fn check_before_birth(
    params: &Value,
    name: &str,
    host: Option<&str>,
) -> Result<(), PlacementRefusal> {
    let placement = Placement::read(params);
    let Some(portal) = placement.portal_word() else {
        return Ok(());
    };
    let address = host.map_or_else(|| name.to_string(), |host| address_on(name, host));
    let mut args = placement.thread_args(&address, &portal);
    args.insert(2, "--check".to_string());
    args.extend(placement.origin_args(params, host.is_some()));
    let Ok(out) = run_fno(&args, CHECK_SECS) else {
        return Ok(());
    };
    match refusal_of(out.status.code(), &String::from_utf8_lossy(&out.stderr)) {
        Some(reason) => Err(PlacementRefusal::Mux(reason)),
        None => Ok(()),
    }
}

/// The refusal in a check's answer. An fno that does not know `--check`
/// exits 2 as well and names the flag, and that is no answer.
fn refusal_of(code: Option<i32>, stderr: &str) -> Option<String> {
    (code == Some(REFUSED) && !stderr.contains("--check")).then(|| one_line(stderr))
}

/// Open the portal of a worker that is live, through the reach a manual
/// `fno mux thread` would type. Answers the last receipt line: where the
/// worker shows, or why it does not and the command that shows it. `None`
/// means the spawn asked for no portal.
pub fn place_after_birth(params: &Value, name: &str, host: Option<&str>) -> Option<String> {
    let placement = Placement::read(params);
    let portal = placement.portal_word()?;
    let address = host.map_or_else(|| name.to_string(), |host| address_on(name, host));
    let words = placement.thread_args(&address, &portal);
    let mut args = words.clone();
    if host.is_some() {
        args.extend(placement.origin_args(params, true));
    }
    let reason = match run_fno(&args, REACH_SECS) {
        Ok(out) if out.status.success() => {
            return Some(one_line(&String::from_utf8_lossy(&out.stdout)));
        }
        Ok(out) if out.status.code().is_none() => {
            format!("the mux did not answer in {REACH_SECS}s")
        }
        Ok(out) => {
            let said = if out.stderr.is_empty() {
                &out.stdout
            } else {
                &out.stderr
            };
            let said = one_line(&String::from_utf8_lossy(said));
            if said.is_empty() {
                format!("fno mux thread ended with {}", out.status)
            } else {
                said
            }
        }
        Err(e) => format!("fno mux thread did not run: {e}"),
    };
    Some(format!(
        "launched, not shown: {reason}; show it with: fno {}",
        crate::plugins::ui::shell_words(&words)
    ))
}

fn address_on(name: &str, host: &str) -> String {
    if name.is_empty() || name.contains('@') {
        name.to_string()
    } else {
        format!("{name}@{host}")
    }
}

fn run_fno(args: &[String], secs: u64) -> std::io::Result<std::process::Output> {
    let mut cmd = std::process::Command::new(crate::scrape::fno_bin());
    cmd.args(args);
    crate::bounded_cmd::output_with_timeout_result(cmd, secs)
}

fn one_line(text: &str) -> String {
    text.lines()
        .map(|line| line.trim().trim_start_matches("fno mux thread: "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn spawn_portal_placement_table() {
        // (params, lane, the portal word after admission or the refusal)
        let cases = [
            (json!({}), "bg", Ok(None)),
            (json!({"portal_new": true}), "bg", Ok(Some("new"))),
            (json!({"split": "down"}), "bg", Ok(Some("new"))),
            (json!({"from": "current"}), "bg", Ok(Some("new"))),
            (json!({"portal": 2, "tab": "1"}), "bg", Ok(Some("2"))),
            (json!({"split": "left"}), "pane", Ok(None)),
            (json!({"portal_new": true}), "pane", Err("thread lane")),
            (json!({"tab": "1"}), "headless", Err("no pane geometry")),
            (json!({"at": "4"}), "bg", Err("--from")),
            (
                json!({"tab": "new", "split": "down"}),
                "bg",
                Err("drop --tab new"),
            ),
            (
                json!({"portal": 1, "bounded_placement": true}),
                "bg",
                Err("--as pane"),
            ),
        ];
        for (mut params, lane, want) in cases {
            let case = format!("{params} on {lane}");
            match (admit(&mut params, lane), want) {
                (Ok(()), Ok(word)) => {
                    assert_eq!(
                        Placement::read(&params).portal_word().as_deref(),
                        word,
                        "{case}"
                    )
                }
                (Err(refusal), Err(text)) => {
                    assert!(refusal.to_string().contains(text), "{case}: {refusal}")
                }
                (got, _) => panic!("{case}: {got:?}"),
            }
        }
        let line = Placement::read(&json!({"tab": "1", "split": "down", "from": "portal 0"}));
        assert_eq!(
            crate::plugins::ui::shell_words(&line.thread_args("kite@demobox", "2")),
            "mux thread kite@demobox --portal 2 --tab 1 --split down --from 'portal 0'"
        );
        // Only a refusal stops a birth. An fno without the check does not.
        assert_eq!(
            refusal_of(Some(2), "fno mux thread: no such tab").as_deref(),
            Some("no such tab")
        );
        assert_eq!(refusal_of(Some(2), "unexpected argument '--check'"), None);
        assert_eq!(refusal_of(Some(24), "no live mux server"), None);
        assert_eq!(refusal_of(None, ""), None);
    }
}
