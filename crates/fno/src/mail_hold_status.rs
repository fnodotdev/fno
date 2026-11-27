//! `fno agent mail hold --status`, answered in process: the caller's own
//! do-not-disturb line, read from the hold store and the registry row. Every
//! other hold argv (arming, `--off`) writes records only fno-agent owns, so
//! it returns `None` and the caller execs the sibling binary. So does an
//! install whose state root comes from the global config, because the store
//! the line must read is then not the one this door can name.

use fno_mail::hold::{Hold, HoldClock, HoldStore, SetBy};
use fno_mail::session::SessionId;

use crate::self_identity::{resolve_self_identity, OwnedDisposition};

/// The registry `delivery_policy` value for a session that takes mail only.
const MAIL_ONLY: &str = "bus-only";

/// Run the leaf. `None` means the argv needs fno-agent.
pub fn run(args: &[String]) -> Option<i32> {
    if !matches!(args, [only] if only == "--status") || state_root_is_configured() {
        return None;
    }
    let registry = crate::ui::agents_view::registry_path();
    let ident = resolve_self_identity(&|name| std::env::var(name).ok(), None, None, &registry);
    if matches!(ident.disposition, OwnedDisposition::Ambiguous) {
        eprintln!("cannot decide which session is 'self': multiple harness markers present (inherited env?)");
        eprintln!("a hold on the wrong session holds another agent's mail");
        return Some(3);
    }
    let (Some(session_id), Some(_)) = (ident.session_id, ident.harness) else {
        eprintln!("no provable harness identity - there is no session to hold mail for");
        return Some(3);
    };
    let handle = canonical_handle(&session_id);
    let hold = SessionId::parse(&session_id).ok().and_then(|session| {
        HoldStore::new(crate::ui::agents_view::hold_dir()).live(&session, chrono::Utc::now())
    });
    let mail_only = mail_only_row(&session_id);
    println!(
        "{}",
        status_line(&handle, hold.as_ref(), mail_only, chrono::Utc::now())
    );
    Some(0)
}

/// A global config that names `paths.state_dir` moves the hold store away
/// from `$FNO_HOME` / `$HOME/.fno`, which is all this door resolves.
fn state_root_is_configured() -> bool {
    if std::env::var_os("FNO_HOME").is_some() {
        return false;
    }
    std::env::var_os("HOME")
        .map(|h| std::path::PathBuf::from(h).join(".fno").join("config.toml"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .is_some_and(|text| text.contains("state_dir"))
}

fn canonical_handle(session_id: &str) -> String {
    let head: String = session_id.chars().take(8).collect();
    if session_id.starts_with("ses_") {
        head
    } else {
        head.to_ascii_lowercase()
    }
}

fn identity_key(session_id: &str) -> String {
    if session_id.starts_with("ses_") {
        session_id.to_string()
    } else {
        session_id.to_lowercase()
    }
}

/// The registry rows, under `agents` or the older `entries` key.
#[derive(serde::Deserialize)]
struct RegistryDoc {
    #[serde(default, alias = "entries")]
    agents: Vec<RegistryRow>,
}

#[derive(serde::Deserialize)]
struct RegistryRow {
    #[serde(default)]
    harness_session_id: Option<String>,
    #[serde(default)]
    short_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    delivery_policy: Option<String>,
}

/// True when exactly one registry row answers to `session_id` and that row
/// carries the mail-only capability. A multi-match or unreadable registry
/// reads as no capability, as fno-agent's own read does.
fn mail_only_row(session_id: &str) -> bool {
    let Ok(text) = crate::ui::agents_view::registry_text() else {
        return false;
    };
    let Ok(doc) = serde_json::from_str::<RegistryDoc>(&text) else {
        return false;
    };
    let wanted = identity_key(session_id);
    let mut hits = doc.agents.iter().filter(|row| {
        let sid_hit = row.harness_session_id.as_deref().is_some_and(|sid| {
            let key = identity_key(sid);
            key == wanted || key.get(..8) == Some(wanted.as_str())
        });
        sid_hit
            || row.short_id.as_deref() == Some(session_id)
            || row.name.as_deref() == Some(session_id)
    });
    match (hits.next(), hits.next()) {
        (Some(only), None) => only.delivery_policy.as_deref() == Some(MAIL_ONLY),
        _ => false,
    }
}

fn clock_description(hold: &Hold) -> String {
    let stamp = |d: chrono::DateTime<chrono::Utc>| d.format("%H:%M:%S UTC").to_string();
    match hold.clock() {
        HoldClock::Wall => format!("wall clock, fixed deadline {}", stamp(hold.until())),
        HoldClock::Idle { ceiling, .. } => {
            format!("quiet minutes idle clock, ceiling {}", stamp(ceiling))
        }
    }
}

fn status_line(
    handle: &str,
    hold: Option<&Hold>,
    mail_only: bool,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    if let Some(hold) = hold {
        if hold.set_by() == SetBy::Conversation {
            return format!(
                "{handle}: holding mail, machine-armed while you talk ({}), lifts about 2 min after your answer",
                clock_description(hold)
            );
        }
        let ms = (hold.until() - now).num_milliseconds();
        let label = if ms < 60_000 {
            format!("~{}s", ms / 1000)
        } else {
            format!("~{}m", (ms + 59_999) / 60_000)
        };
        return format!(
            "{handle}: holding mail, {}, lifts in {label}",
            clock_description(hold)
        );
    }
    if mail_only {
        format!("{handle}: no hold - this session takes mail only, never typed text")
    } else {
        format!("{handle}: no hold - mail delivers normally")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_hold_line_names_the_lane() {
        let now = chrono::Utc::now();
        assert_eq!(
            status_line("abcd1234", None, false, now),
            "abcd1234: no hold - mail delivers normally"
        );
        assert_eq!(
            status_line("abcd1234", None, true, now),
            "abcd1234: no hold - this session takes mail only, never typed text"
        );
    }
}
