//! The name-to-session question: one trait, one answer shape. A caller hands
//! a token and gets one session, a list of equally-good sessions, or nothing
//! plus the live sessions nearest the token's spelling.

use crate::session::{SessionId, ShortForm};

/// What a token resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Exactly one stored session answers the token.
    One(SessionId),
    /// Two or more stored sessions answer it equally well; the caller
    /// refuses rather than pick.
    Ambiguous(Vec<SessionId>),
    /// Nothing answered. `near` lists the live sessions whose names share a
    /// spelling with the token, best first, so a refusal can point at them.
    None { near: Vec<(ShortForm, String)> },
}

impl Resolution {
    /// The merge rule every source list shares: one hit is One, two or more
    /// are Ambiguous, zero is None with no near rows (the caller that knows
    /// the neighborhood fills those in).
    pub fn from_hits(mut hits: Vec<SessionId>) -> Resolution {
        match hits.len() {
            0 => Resolution::None { near: Vec::new() },
            1 => Resolution::One(hits.swap_remove(0)),
            _ => {
                hits.sort();
                hits.dedup();
                Resolution::Ambiguous(hits)
            }
        }
    }
}

/// The one question a sender asks before it addresses mail.
pub trait Directory {
    fn resolve(&self, token: &str) -> Resolution;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(hex_tail: &str) -> SessionId {
        SessionId::parse(&format!("a1b2c3d4-1111-2222-3333-{hex_tail}")).unwrap()
    }

    /// Two sessions whose names both equal the token resolve Ambiguous with
    /// both ids; the caller refuses before any row (AC3-ERR).
    #[test]
    fn mail_addr_directory_ambiguous_lists_both() {
        let both = vec![id("444455556666"), id("999988887777")];
        let resolved = Resolution::from_hits(both);
        let Resolution::Ambiguous(ids) = resolved else {
            panic!("two equal hits must read Ambiguous");
        };
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&id("444455556666")));
        assert!(ids.contains(&id("999988887777")));
        // One hit reads One; zero reads None.
        assert_eq!(
            Resolution::from_hits(vec![id("444455556666")]),
            Resolution::One(id("444455556666"))
        );
        assert_eq!(
            Resolution::from_hits(Vec::new()),
            Resolution::None { near: Vec::new() }
        );
    }
}
