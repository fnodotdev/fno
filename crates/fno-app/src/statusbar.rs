//! The status bar at the window foot: one line. Left, each provider's
//! used percent and when its window resets. Right, the live agent count,
//! the focused agent's branch and this machine. The usage read is the
//! `fno config accounts usage` cache, polled once a minute on a background
//! task; a failed read shows no providers, never an error.

use std::process::{Command, Stdio};

use gpui::{div, prelude::*, px, rgb, Div};
use serde_json::Value;

use super::link::Layout;
use super::start;
use super::ui::{self, Role};
use super::window::Root;

/// The bar's height. Fixed, like the title bar's.
pub const BAR_H: f32 = 24.0;

/// The latest reading the poll stored. A static, so the window state
/// carries no field for it.
static USAGE: std::sync::Mutex<Vec<ProviderUse>> = std::sync::Mutex::new(Vec::new());

pub fn set_usage(rows: Vec<ProviderUse>) {
    if let Ok(mut held) = USAGE.lock() {
        *held = rows;
    }
}

fn usage_rows() -> Vec<ProviderUse> {
    USAGE.lock().map(|held| held.clone()).unwrap_or_default()
}

/// One provider's tightest window: the one closest to its cap.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderUse {
    pub name: String,
    pub used_pct: f64,
    pub resets_at: Option<f64>,
}

/// The providers in `fno config accounts usage --json` that carry a
/// reading. An `unknown` entry has no windows and is skipped.
pub fn parse_usage(json: &str) -> Vec<ProviderUse> {
    #[derive(serde::Deserialize)]
    struct Window {
        used_pct: Option<f64>,
        resets_at: Option<f64>,
    }
    #[derive(serde::Deserialize)]
    struct Entry {
        #[serde(default)]
        windows: Vec<Window>,
    }
    let Ok(map) = serde_json::from_str::<std::collections::BTreeMap<String, Value>>(json) else {
        return Vec::new();
    };
    let mut out: Vec<ProviderUse> = map
        .into_iter()
        .filter_map(|(name, v)| {
            let entry: Entry = serde_json::from_value(v).ok()?;
            let (used_pct, resets_at) = entry
                .windows
                .iter()
                .filter_map(|w| Some((w.used_pct?, w.resets_at)))
                .max_by(|a, b| a.0.total_cmp(&b.0))?;
            Some(ProviderUse {
                name,
                used_pct,
                resets_at,
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The `fno` to run: the installer's link first, because an app launched
/// from the Dock carries a minimal PATH.
fn fno_bin() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(|h| std::path::Path::new(&h).join(".local/bin/fno"))
        .filter(|p| p.exists())
        .unwrap_or_else(|| "fno".into())
}

/// Read the cached usage within ten seconds. `None` is a failed or timed
/// out read, so the caller keeps the last good reading.
pub fn read_usage() -> Option<Vec<ProviderUse>> {
    use std::io::Read;
    let mut child = Command::new(fno_bin())
        .args(["config", "accounts", "usage", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Drained on its own thread so a full pipe never stalls the child.
    let mut out = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        out.read_to_string(&mut text).ok().map(|_| text)
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            _ => {
                if child.kill().is_ok() {
                    let _reaped = child.wait();
                }
                return None;
            }
        }
    };
    let text = reader.join().ok()??;
    // Output that is not JSON is a failed read, not an empty one.
    serde_json::from_str::<Value>(&text).ok()?;
    status.success().then(|| parse_usage(&text))
}

/// Time left to a reset, in the two largest units that matter.
pub fn resets_in(resets_at: f64, now: f64) -> String {
    let secs = (resets_at - now).max(0.0) as u64;
    match secs {
        0 => "now".into(),
        1..=3599 => format!("{}m", secs.div_ceil(60)),
        3600..=86_399 => format!("{}h{:02}m", secs / 3600, secs % 3600 / 60),
        _ => format!("{}d{}h", secs / 86_400, secs % 86_400 / 3600),
    }
}

fn usage_text(u: &ProviderUse, now: f64) -> String {
    let pct = format!("{} {:.0}%", u.name, u.used_pct);
    match u.resets_at {
        Some(at) => format!("{pct} · resets {}", resets_in(at, now)),
        None => pct,
    }
}

/// Agents that are alive: a row whose process exited does not count.
pub fn live_agents(layout: &Layout) -> usize {
    layout.agents.iter().filter(|a| !a.exited).count()
}

/// The branch of the agent the focused pane hosts, when its subline
/// carries one.
pub fn focused_branch(layout: &Layout) -> Option<String> {
    let a = layout
        .agents
        .iter()
        .find(|a| a.pane_id == Some(layout.focus))?;
    let base = a.cwd_base.as_deref()?;
    start::branch_of(a, base).map(str::to_string)
}

/// The right-hand facts, in order, each one only when it is known.
fn facts(root: &Root) -> Vec<String> {
    let n = live_agents(&root.layout);
    let mut out = vec![format!("{n} agent{}", if n == 1 { "" } else { "s" })];
    out.extend(focused_branch(&root.layout));
    out.extend(start::local_machine(root));
    out
}

pub(super) fn render(root: &Root) -> Div {
    let p = root.palette;
    let now = fno::app_api::digest_overlay::now_secs() as f64;
    let mut left = div().flex().flex_row().items_center().gap_4().min_w_0();
    for u in &usage_rows() {
        left = left.child(ui::text(usage_text(u, now), Role::Fact, &p).whitespace_nowrap());
    }
    // pr_2 lines the last fact up with the title bar's last label.
    let mut right = div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap_2()
        .pr_2();
    for (i, f) in facts(root).into_iter().enumerate() {
        if i > 0 {
            right = right.child(ui::text("·", Role::Hint, &p));
        }
        right = right.child(ui::text(f, Role::Fact, &p).whitespace_nowrap());
    }
    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap_4()
        .h(px(BAR_H))
        .px_3()
        .border_t_1()
        .border_color(rgb(p.border))
        .font_family(ui::MONO)
        .child(left)
        .child(div().flex_1())
        .child(right)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_keeps_each_providers_tightest_window_and_skips_unknown() {
        let json = r#"{
            "makers": {"windows": [
                {"label": "weekly", "used_pct": 35.0, "resets_at": 1000.0},
                {"label": "5h", "used_pct": 80.0, "resets_at": 500.0}]},
            "readyrule": {"state": "unknown", "reason": "not-probed"}
        }"#;
        assert_eq!(
            parse_usage(json),
            [ProviderUse {
                name: "makers".into(),
                used_pct: 80.0,
                resets_at: Some(500.0)
            }]
        );
        assert!(parse_usage("not json").is_empty());
    }

    #[test]
    fn a_reset_reads_in_its_two_largest_units() {
        assert_eq!(resets_in(100.0, 200.0), "now");
        assert_eq!(resets_in(1000.0, 0.0), "17m");
        assert_eq!(resets_in(2.0 * 3600.0 + 14.0 * 60.0, 0.0), "2h14m");
        assert_eq!(resets_in(3.0 * 86_400.0 + 5.0 * 3600.0, 0.0), "3d5h");
    }
}
