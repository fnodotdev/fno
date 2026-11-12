//! Graft slot binding and spawn, with the mod slot.
//!
//! Moved out of `layout_graft` under the file-budget gate, with the code the
//! mod slot touched. A `LayoutBinding::Mod` slot carries only a `<pack>/<mod>`
//! target. The server builds the one fixed argv `fno config plugins open
//! <target>`, so no spec can name another command (Locked Decision 9). The
//! open command re-checks the switch and the digest at launch, so a stale
//! read here cannot run a switched-off mod.
//!
//! The switch read is a subprocess. It runs on the async control arm BEFORE
//! the core turn, never inside it: the core is one serialized loop and a
//! blocking read there would stall every client.

use super::*;
use crate::proto::{AnchoredLayoutSpec, GraftOutcome, GraftSlotResult, LayoutBinding};
use serde::Deserialize;

/// The reason a switched-off mod's slot fills with a shell.
pub(super) const MOD_OFF: &str = "mod off";

/// What the switch read said about one `<pack>/<mod>` target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ModVerdict {
    On,
    /// Fill the slot with a shell and report this reason.
    Off(String),
}

pub(crate) type ModVerdicts = HashMap<String, ModVerdict>;

#[derive(Debug, Deserialize)]
struct Listing {
    packs: Vec<PackRow>,
}

#[derive(Debug, Deserialize)]
struct PackRow {
    pack_id: String,
    #[serde(default)]
    mods: Vec<ModRow>,
}

#[derive(Debug, Deserialize)]
struct ModRow {
    id: String,
    #[serde(default)]
    enabled: bool,
}

/// The one argv a mod slot runs.
pub(super) fn open_argv(target: &str) -> Vec<String> {
    vec![
        fno_bin().to_string_lossy().into_owned(),
        "config".into(),
        "plugins".into(),
        "open".into(),
        target.into(),
    ]
}

/// `<safe>/<safe>`: one slash, ids made of word characters, `.` and `-`.
pub(crate) fn target_ok(target: &str) -> bool {
    let safe = |s: &str| {
        !s.is_empty()
            && !s.starts_with('-')
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    matches!(target.split_once('/'), Some((p, m)) if safe(p) && safe(m))
}

fn mod_targets(spec: &AnchoredLayoutSpec) -> Vec<&str> {
    spec.slots
        .iter()
        .filter_map(|s| match &s.binding {
            LayoutBinding::Mod(t) => Some(t.as_str()),
            _ => None,
        })
        .collect()
}

/// Read the installed mods' switches once, when the spec binds any mod.
/// Blocking subprocess: call from `spawn_blocking`.
fn read_switches(targets: &[&str]) -> ModVerdicts {
    let mut cmd = crate::process_admission::std_command(fno_bin());
    cmd.args(["config", "plugins", "-J", "ls"])
        .stdin(std::process::Stdio::null());
    let failed = |why: String| -> ModVerdicts {
        targets
            .iter()
            .map(|t| {
                (
                    t.to_string(),
                    ModVerdict::Off(format!("plugins read failed: {why}")),
                )
            })
            .collect()
    };
    let out = match crate::process_admission::std_output(&mut cmd) {
        Ok(o) => o,
        Err(e) => return failed(first_line_or(&e.to_string(), "did not start")),
    };
    if !out.status.success() {
        return failed(first_line_or(
            &String::from_utf8_lossy(&out.stderr),
            "nonzero exit",
        ));
    }
    let listing: Listing = match serde_json::from_slice(&out.stdout) {
        Ok(l) => l,
        Err(e) => return failed(first_line_or(&e.to_string(), "unreadable listing")),
    };
    targets
        .iter()
        .map(|t| {
            let on = t.split_once('/').and_then(|(pack, id)| {
                listing
                    .packs
                    .iter()
                    .find(|p| p.pack_id == pack)
                    .and_then(|p| p.mods.iter().find(|m| m.id == id))
                    .map(|m| m.enabled)
            });
            let verdict = match on {
                Some(true) => ModVerdict::On,
                Some(false) => ModVerdict::Off(MOD_OFF.into()),
                None => ModVerdict::Off(format!("no installed mod {t}")),
            };
            (t.to_string(), verdict)
        })
        .collect()
}

/// Resolve every mod slot's verdict off the core loop.
pub(crate) async fn resolve(spec: &AnchoredLayoutSpec) -> ModVerdicts {
    let targets: Vec<String> = mod_targets(spec).into_iter().map(String::from).collect();
    if targets.is_empty() {
        return ModVerdicts::new();
    }
    tokio::task::spawn_blocking(move || {
        let refs: Vec<&str> = targets.iter().map(String::as_str).collect();
        read_switches(&refs)
    })
    .await
    .unwrap_or_default()
}

/// Slots the binding pass left to spawn, with the receipts already settled.
pub(super) struct Bound {
    pub resolved: HashMap<String, u64>,
    /// `(slot, reason)`: a shell, with the reason a mod slot fell back to one.
    pub shell_slots: Vec<(String, Option<String>)>,
    /// `(slot, target)`: a pane running the fixed open argv.
    pub mod_slots: Vec<(String, String)>,
    pub results: Vec<GraftSlotResult>,
}

impl Bound {
    /// Slot names still to spawn, for the pre-spawn fit probe.
    pub fn pending(&self) -> impl Iterator<Item = &str> {
        self.shell_slots
            .iter()
            .map(|(n, _)| n.as_str())
            .chain(self.mod_slots.iter().map(|(n, _)| n.as_str()))
    }
}

type Refusal = (u32, String);

impl Core {
    /// Resolve Anchor + Fno bindings to live pane ids; collect the slots to
    /// spawn. An unavailable Fno is REFUSED, never degraded to a shell.
    /// Duplicates (two bindings -> one pane, or Fno -> the anchor) are refused.
    /// A mod slot with a malformed target is refused before any spawn; a
    /// switched-off mod becomes a shell slot that carries its reason.
    pub(super) fn bind_graft_slots(
        &self,
        anchor: u64,
        spec: &AnchoredLayoutSpec,
        mods: &ModVerdicts,
    ) -> Result<Bound, Refusal> {
        let mut bound = Bound {
            resolved: HashMap::new(),
            shell_slots: Vec::new(),
            mod_slots: Vec::new(),
            results: Vec::new(),
        };
        for slot in &spec.slots {
            let (pid, outcome) = match &slot.binding {
                LayoutBinding::Anchor => (anchor, GraftOutcome::Anchor),
                LayoutBinding::Fno(id) => {
                    let p = self.resolve_local_pane(id).ok_or_else(|| {
                        (
                            err_code::BAD_REQUEST,
                            format!("fno binding {:?} has no live pane", slot.name),
                        )
                    })?;
                    (p, GraftOutcome::Fno)
                }
                LayoutBinding::Shell => {
                    bound.shell_slots.push((slot.name.clone(), None));
                    continue;
                }
                LayoutBinding::Mod(target) => {
                    if !target_ok(target) {
                        return Err((
                            err_code::BAD_REQUEST,
                            format!(
                                "mod binding {:?} wants <pack>/<mod>, got {target:?}",
                                slot.name
                            ),
                        ));
                    }
                    match mods.get(target) {
                        Some(ModVerdict::On) => {
                            bound.mod_slots.push((slot.name.clone(), target.clone()))
                        }
                        Some(ModVerdict::Off(why)) => bound
                            .shell_slots
                            .push((slot.name.clone(), Some(why.clone()))),
                        None => bound.shell_slots.push((
                            slot.name.clone(),
                            Some(format!("plugins read failed: no verdict for {target}")),
                        )),
                    }
                    continue;
                }
            };
            if bound.resolved.values().any(|&v| v == pid) {
                return Err((
                    err_code::BAD_REQUEST,
                    format!("two graft slots resolve to the same pane {pid}"),
                ));
            }
            bound.resolved.insert(slot.name.clone(), pid);
            bound.results.push(GraftSlotResult {
                slot: slot.name.clone(),
                pane_id: pid,
                outcome,
                reason: None,
            });
        }
        Ok(bound)
    }

    /// Spawn the shell and mod slots AFTER pure validation. All-or-nothing: a
    /// failed spawn reaps every pane spawned so far (no detach happened yet,
    /// so rollback is spawn-only). Returns the spawned pane ids for the
    /// caller's later rollbacks.
    pub(super) fn spawn_graft_slots(
        &mut self,
        bound: &mut Bound,
        cwd: &str,
    ) -> Result<Vec<u64>, Refusal> {
        let mut spawned: Vec<u64> = Vec::new();
        let shells = std::mem::take(&mut bound.shell_slots);
        let mods = std::mem::take(&mut bound.mod_slots);
        let shell_jobs = shells.into_iter().map(|(n, why)| (n, None, why));
        let mod_jobs = mods.into_iter().map(|(n, t)| (n, Some(t), None));
        for (name, target, reason) in shell_jobs.chain(mod_jobs) {
            let spawn = match &target {
                Some(t) => {
                    self.spawn_pane_cmd(&open_argv(t), vt::DEFAULT_ROWS, vt::DEFAULT_COLS, cwd)
                }
                None => self.spawn_pane(vt::DEFAULT_ROWS, vt::DEFAULT_COLS, cwd),
            };
            match spawn {
                Ok(p) => {
                    spawned.push(p);
                    bound.resolved.insert(name.clone(), p);
                    bound.results.push(GraftSlotResult {
                        slot: name,
                        pane_id: p,
                        outcome: if target.is_some() {
                            GraftOutcome::Mod
                        } else {
                            GraftOutcome::Shell
                        },
                        reason,
                    });
                }
                Err(_) => {
                    for p in &spawned {
                        self.reap_pane(*p);
                    }
                    return Err((
                        err_code::SPAWN_FAILED,
                        format!("graft slot {name:?} failed to spawn; rolled back"),
                    ));
                }
            }
        }
        Ok(spawned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_shape_is_one_safe_pair() {
        assert!(target_ok("fno-panels/review-ledger"));
        assert!(!target_ok("fno-panels"));
        assert!(!target_ok("a/b/c"));
        assert!(!target_ok("/b"));
        assert!(!target_ok("a/--flag"));
        assert!(!target_ok("a/b c"));
    }

    #[test]
    fn open_argv_is_the_one_fixed_command() {
        let argv = open_argv("p/m");
        let (_, rest) = argv.split_first().unwrap();
        assert_eq!(rest, ["config", "plugins", "open", "p/m"]);
    }
}
