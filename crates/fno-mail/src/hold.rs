//! The hold record: one per session, always with an end. A hold keeps live
//! delivery off a session until `until`; mail still lands on the bus and the
//! release drains it. There is no hold without an expiry: [`Hold::new`] is the
//! only constructor, and it caps the requested length at the caller's
//! ceiling, so a hand stamp with no clock cannot be built.
//!
//! [`HoldStore`] is the one writer. Each record is one JSON file named by the
//! session id under the store directory.

use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::session::SessionId;

/// Who set a hold. The conversation rules arm and shorten their own holds
/// and never touch one the user set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetBy {
    /// The session's own hold command, or a tool acting for its user.
    User,
    /// The machine, while the user talks to the session.
    Conversation,
}

/// How the end moves. A wall hold ends at a fixed time. An idle hold
/// restarts its window on activity, but never past its ceiling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HoldClock {
    Wall,
    Idle {
        window_s: i64,
        ceiling: DateTime<Utc>,
    },
}

/// One hold. The fields are private: every record comes from [`Hold::new`]
/// or from a file the store wrote, and `until` is always present.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hold {
    session: SessionId,
    until: DateTime<Utc>,
    set_by: SetBy,
    reason: String,
    clock: HoldClock,
}

impl Hold {
    /// A hold of `length` from `now`, capped at `ceiling`. An idle hold's
    /// re-arm ceiling is twice its window, under the same cap. A length
    /// under one second is one second.
    pub fn new(
        session: SessionId,
        length: Duration,
        idle: bool,
        now: DateTime<Utc>,
        ceiling: Duration,
        set_by: SetBy,
        reason: &str,
    ) -> Hold {
        let one = Duration::seconds(1);
        let cap = ceiling.max(one);
        let length = length.max(one).min(cap);
        let clock = if idle {
            let reach = (length + length).min(cap);
            HoldClock::Idle {
                window_s: length.num_seconds(),
                ceiling: now + reach,
            }
        } else {
            HoldClock::Wall
        };
        Hold {
            session,
            until: now + length,
            set_by,
            reason: reason.to_string(),
            clock,
        }
    }

    pub fn session(&self) -> &SessionId {
        &self.session
    }

    pub fn until(&self) -> DateTime<Utc> {
        self.until
    }

    pub fn set_by(&self) -> SetBy {
        self.set_by
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }

    pub fn clock(&self) -> HoldClock {
        self.clock
    }

    pub fn live(&self, now: DateTime<Utc>) -> bool {
        self.until > now
    }

    /// The idle re-arm: a live idle hold moves its end to one window from
    /// `now`, never past its ceiling. A wall hold, or one that has ended,
    /// comes back unchanged.
    pub fn extended(&self, now: DateTime<Utc>) -> Hold {
        let HoldClock::Idle { window_s, ceiling } = self.clock else {
            return self.clone();
        };
        if !self.live(now) {
            return self.clone();
        }
        let mut next = self.clone();
        next.until = (now + Duration::seconds(window_s))
            .min(ceiling)
            .max(self.until);
        next
    }

    /// The same hold ending no later than `until`. A shorten never extends.
    pub fn shortened_to(&self, until: DateTime<Utc>) -> Hold {
        let mut next = self.clone();
        next.until = self.until.min(until);
        next
    }
}

/// The hold store: one file per session under `dir`.
#[derive(Clone, Debug)]
pub struct HoldStore {
    dir: PathBuf,
}

impl HoldStore {
    pub fn new(dir: impl Into<PathBuf>) -> HoldStore {
        HoldStore { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn path(&self, session: &SessionId) -> PathBuf {
        self.dir.join(format!("{}.json", session.as_str()))
    }

    /// Write the record atomically (temp file, then rename).
    pub fn put(&self, hold: &Hold) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join(format!(
            ".{}.{}.tmp",
            hold.session.as_str(),
            std::process::id()
        ));
        {
            let mut f = std::fs::File::create(&tmp)?;
            let mut text = serde_json::to_string(hold).map_err(std::io::Error::other)?;
            text.push('\n');
            f.write_all(text.as_bytes())?;
        }
        std::fs::rename(&tmp, self.path(&hold.session))
    }

    /// The stored record, live or not. A missing file, or one that is not a
    /// hold record, reads as none.
    pub fn get(&self, session: &SessionId) -> Option<Hold> {
        let text = std::fs::read_to_string(self.path(session)).ok()?;
        let hold: Hold = serde_json::from_str(&text).ok()?;
        (hold.session == *session).then_some(hold)
    }

    /// The live record. A record whose end has passed is removed here, so
    /// the first reader after the end finds no hold and no file.
    pub fn live(&self, session: &SessionId, now: DateTime<Utc>) -> Option<Hold> {
        let hold = self.get(session)?;
        if hold.live(now) {
            return Some(hold);
        }
        let _ = std::fs::remove_file(self.path(session));
        None
    }

    /// Remove the record. True when a file was there.
    pub fn clear(&self, session: &SessionId) -> std::io::Result<bool> {
        match std::fs::remove_file(self.path(session)) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Every readable record in the store, in file-name order.
    pub fn list(&self) -> Vec<Hold> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        paths.sort();
        paths
            .iter()
            .filter_map(|p| std::fs::read_to_string(p).ok())
            .filter_map(|t| serde_json::from_str::<Hold>(&t).ok())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid() -> SessionId {
        SessionId::parse("d9daf3e8-1111-2222-3333-444455556666").unwrap()
    }

    /// A hold asked for longer than the ceiling ends at the ceiling, and a
    /// file with no `until` is not a hold.
    #[test]
    fn hold_without_minutes_gets_the_ceiling() {
        let now = Utc::now();
        let ceiling = Duration::minutes(480);
        let hold = Hold::new(
            sid(),
            Duration::MAX,
            false,
            now,
            ceiling,
            SetBy::User,
            "dnd",
        );
        assert_eq!(hold.until(), now + ceiling);
        let idle = Hold::new(
            sid(),
            Duration::minutes(300),
            true,
            now,
            ceiling,
            SetBy::User,
            "",
        );
        assert_eq!(
            idle.clock(),
            HoldClock::Idle {
                window_s: 300 * 60,
                ceiling: now + ceiling
            }
        );
        let legacy =
            r#"{"until": null, "window_s": 60, "clock_kind": "permanent", "ceiling": null}"#;
        assert!(serde_json::from_str::<Hold>(legacy).is_err());
    }

    /// AC7: a hold whose end has passed reads as none, and the record is gone.
    #[test]
    fn an_ended_hold_reads_as_none_and_its_record_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let store = HoldStore::new(dir.path());
        let now = Utc::now();
        let hold = Hold::new(
            sid(),
            Duration::minutes(5),
            false,
            now,
            Duration::hours(8),
            SetBy::User,
            "",
        );
        store.put(&hold).unwrap();
        assert_eq!(store.live(&sid(), now), Some(hold.clone()));
        assert_eq!(store.list(), vec![hold]);
        assert_eq!(store.live(&sid(), now + Duration::minutes(6)), None);
        assert!(!store.path(&sid()).exists());
    }

    /// The idle re-arm moves the end one window out, never past its
    /// ceiling; a shorten never extends.
    #[test]
    fn an_idle_hold_re_arms_up_to_its_ceiling_and_a_shorten_never_extends() {
        let now = Utc::now();
        let hold = Hold::new(
            sid(),
            Duration::minutes(10),
            true,
            now,
            Duration::hours(8),
            SetBy::User,
            "",
        );
        let later = now + Duration::minutes(8);
        assert_eq!(hold.extended(later).until(), later + Duration::minutes(10));
        let late = now + Duration::minutes(18);
        assert_eq!(
            hold.extended(now + Duration::minutes(9))
                .extended(late)
                .until(),
            now + Duration::minutes(20)
        );
        assert_eq!(
            hold.shortened_to(now + Duration::hours(1)).until(),
            hold.until()
        );
        let wall = Hold::new(
            sid(),
            Duration::minutes(10),
            false,
            now,
            Duration::hours(8),
            SetBy::User,
            "",
        );
        assert_eq!(wall.extended(later), wall);
    }
}
