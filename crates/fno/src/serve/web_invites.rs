//! The command page's invite gate. A bridge that sits behind `tailscale
//! serve` binds loopback, and tailscale adds `Tailscale-User-Login` to each
//! proxied request. That header proves a login only on a loopback bind: on a
//! wide bind any client can send it, so a wide bind has no identity at all.
//!
//! `[[web.invites]]` rows (`email`, `scope`, `role`) say who may do what. They
//! come only from the trusted file ([`crate::trusted_config`]), read again per
//! request, so a revoke needs no restart and a served folder invites nobody.
//! The decision here is pure, so every refusal is a table test with no listener.

use crate::trusted_config::{Rows, TrustedFile, TrustedKey};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Rank order is the grant order: a higher role does everything a lower one does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Role {
    Viewer,
    /// Parsed so a commenter row is legal. No route needs it yet, so it grants
    /// nothing above viewer.
    Commenter,
    Editor,
    Owner,
}

impl Role {
    fn parse(s: &str) -> Option<Role> {
        match s.trim().to_ascii_lowercase().as_str() {
            "viewer" => Some(Role::Viewer),
            "commenter" => Some(Role::Commenter),
            "editor" => Some(Role::Editor),
            "owner" => Some(Role::Owner),
            _ => None,
        }
    }
}

/// One invite row. `scope` is `*` or a node id; a node id covers that node and
/// every node under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Invite {
    pub(crate) email: String,
    pub(crate) scope: String,
    pub(crate) role: Role,
}

/// Why a request was refused: the status the route returns and one line the
/// page shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub(crate) code: u16,
    pub(crate) text: String,
}

fn refuse(code: u16, text: &str) -> Refusal {
    Refusal {
        code,
        text: text.to_string(),
    }
}

/// One read of the invite list, held for one request. `None` is no list.
#[derive(Debug, Clone)]
pub(crate) struct Grants(Option<Arc<[Invite]>>);

impl Grants {
    fn nobody() -> Self {
        Self(Some(Vec::new().into()))
    }

    pub(crate) fn list(&self) -> Option<&[Invite]> {
        self.0.as_deref()
    }
}

/// Where one bridge reads its invites, and what it must not forget.
pub(crate) struct InviteSource {
    file: TrustedFile,
    /// A project list differed at bind. It admits nobody until a restart.
    held: bool,
    /// A list or a fault was seen. From then on no list means nobody, so
    /// removing the last row never reopens token-only reads.
    gated: AtomicBool,
}

impl InviteSource {
    /// The source for a bridge started in `startup`. Prints what it found. It
    /// never reads the cwd, so a bridge that changes folder keeps its hold.
    pub(crate) fn for_bridge(file: TrustedFile, startup: &Path) -> Self {
        let project = crate::digest_overlay::project_config_files(startup);
        let (source, notices) = Self::at_bind(file, &project);
        for notice in notices {
            eprintln!("fno mux serve --web: {notice}");
        }
        source
    }

    /// A project list used to win. It is compared once, here, and never
    /// grants: the same rows change nothing, different rows hold closed.
    pub(crate) fn at_bind(file: TrustedFile, project_files: &[PathBuf]) -> (Self, Vec<String>) {
        let key = TrustedKey::WebInvites;
        let mut notices = file.notes(key, &[]);
        let trusted = file.rows(key);
        let mut held = false;
        if let Some((project, rows)) = file.ignored_rows(key, project_files) {
            held = !matches!(&trusted, Rows::List(own) if same_rows(own, &rows));
            let project = project.display();
            let count = rows.len();
            notices.push(if held {
                let own = file.path().map_or_else(
                    || "the trusted config file".to_string(),
                    |path| path.display().to_string(),
                );
                format!(
                    "{project} holds {count} web.invites row(s) that {own} does not match. A project file never invites, so every proxied request is refused until a restart. Write every row you want: fno config set web.invites '[{{\"email\":\"<login>\",\"scope\":\"<node id or *>\",\"role\":\"<role>\"}}]'. Then run `fno config unset web.invites --local` in that folder and restart this bridge once."
                )
            } else {
                format!(
                    "{project} repeats the {count} trusted web.invites row(s). A project file never invites, so remove them there with `fno config unset web.invites --local`."
                )
            });
        }
        let gated = AtomicBool::new(held || trusted != Rows::Absent);
        (Self { file, held, gated }, notices)
    }

    /// The grants as of now.
    pub(crate) fn read(&self) -> Grants {
        if self.held {
            return Grants::nobody();
        }
        match self.file.rows(TrustedKey::WebInvites) {
            Rows::List(rows) => {
                self.gated.store(true, Ordering::SeqCst);
                Grants(Some(parse_invites(&rows).into()))
            }
            Rows::Invalid(_) => {
                self.gated.store(true, Ordering::SeqCst);
                Grants::nobody()
            }
            Rows::Absent if self.gated.load(Ordering::SeqCst) => Grants::nobody(),
            Rows::Absent => Grants(None),
        }
    }
}

/// The same rows in any order.
fn same_rows(a: &[toml::Value], b: &[toml::Value]) -> bool {
    let mut rest = b.to_vec();
    let all_found = a.iter().all(|row| {
        let found = rest.iter().position(|other| other == row);
        found.map(|at| rest.swap_remove(at)).is_some()
    });
    all_found && rest.is_empty()
}

#[derive(serde::Deserialize)]
struct InviteRow {
    email: String,
    #[serde(default = "whole_board")]
    scope: String,
    role: String,
}

fn whole_board() -> String {
    "*".to_string()
}

/// A row with a missing or wrong-typed field or an unknown role grants
/// nothing. A scope that is present is never read as a missing one.
fn parse_invites(rows: &[toml::Value]) -> Vec<Invite> {
    rows.iter()
        // A derived struct also reads an array as its fields in order.
        .filter(|row| row.is_table())
        .filter_map(|row| {
            // A row that does not parse is dropped, and the gate stays on.
            let row: InviteRow = row.clone().try_into().ok()?;
            let email = row.email.trim().to_ascii_lowercase();
            let scope = row.scope.trim().to_string();
            let role = Role::parse(&row.role)?;
            (!email.is_empty() && !scope.is_empty()).then_some(Invite { email, scope, role })
        })
        .collect()
}

/// What a request carries: the facts the decision reads.
pub(crate) struct Caller<'a> {
    /// `Tailscale-User-Login`, as sent.
    pub(crate) login: Option<&'a str>,
    /// The bridge is bound to loopback.
    pub(crate) loopback_bind: bool,
    /// The request's Host names loopback and it carries no forwarding header,
    /// so it did not come through a proxy.
    pub(crate) loopback_host: bool,
}

impl Caller<'_> {
    /// The user at their own machine. A login header of any value, even an
    /// empty one, means a proxy sent the request.
    pub(crate) fn is_direct_local(&self) -> bool {
        self.loopback_bind && self.loopback_host && self.login.is_none()
    }

    /// The login the rows are matched against.
    pub(crate) fn identity(&self) -> Option<String> {
        self.login
            .map(|login| login.trim().to_ascii_lowercase())
            .filter(|login| !login.is_empty())
    }
}

/// Allow with the name to stamp on the work, or refuse with the reason.
/// `chain` is the node and its ancestors; `None` means the route has no node,
/// so any scope counts.
pub(crate) fn decide(
    caller: &Caller<'_>,
    invites: Option<&[Invite]>,
    chain: Option<&[String]>,
    needed: Role,
) -> Result<String, Refusal> {
    // The user at their own machine: a direct loopback request carries no
    // proxy login. They own every scope.
    if caller.is_direct_local() {
        return Ok("local".to_string());
    }
    let Some(invites) = invites else {
        // No invite list: reads keep today's token-only rule, and nothing
        // else is open to a request that came through the proxy.
        return if needed == Role::Viewer {
            Ok("token".to_string())
        } else {
            Err(refuse(
                403,
                "answering from here needs web.invites in config: no invite list is set",
            ))
        };
    };
    if !caller.loopback_bind {
        return Err(refuse(
            403,
            "web.invites is set, so the bridge must bind loopback behind tailscale serve",
        ));
    }
    let Some(login) = caller.identity() else {
        return Err(refuse(
            403,
            "no login on the request: it did not come through tailscale serve",
        ));
    };
    let mine: Vec<&Invite> = invites.iter().filter(|i| i.email == login).collect();
    if mine.is_empty() {
        return Err(refuse(403, "this login is not on the invite list"));
    }
    let covers =
        |inv: &Invite| inv.scope == "*" || chain.is_none_or(|c| c.iter().any(|n| *n == inv.scope));
    let scoped: Vec<&&Invite> = mine.iter().filter(|i| covers(i)).collect();
    if scoped.is_empty() {
        return Err(refuse(403, "this invite does not cover that node"));
    }
    if scoped.iter().any(|i| i.role >= needed) {
        Ok(login)
    } else {
        Err(refuse(403, "this invite's role does not allow that"))
    }
}

#[cfg(test)]
mod web_invites_tests {
    use super::*;

    fn inv(email: &str, scope: &str, role: Role) -> Invite {
        Invite {
            email: email.into(),
            scope: scope.into(),
            role,
        }
    }

    fn tailnet(login: &str) -> Caller<'_> {
        Caller {
            login: Some(login),
            loopback_bind: true,
            loopback_host: false,
        }
    }

    fn chain(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    fn web_invites_parse_drops_bad_rows_and_lowercases() {
        #[derive(serde::Deserialize)]
        struct Doc {
            rows: Vec<toml::Value>,
        }
        let doc: Doc = toml::from_str(
            r#"rows = [
                {email = "Ann@Example.com", scope = "x-1", role = "Editor"},
                {email = "bob@example.com", role = "boss"},
                {scope = "*", role = "viewer"},
                {email = "dee@example.com", scope = ["x-1"], role = "owner"},
                {email = "eve@example.com", scope = " ", role = "owner"},
                ["fay@example.com", "*", "owner"],
                {email = "cy@example.com", role = "viewer"},
            ]"#,
        )
        .unwrap();
        let rows = parse_invites(&doc.rows);
        assert_eq!(
            rows,
            vec![
                inv("ann@example.com", "x-1", Role::Editor),
                inv("cy@example.com", "*", Role::Viewer),
            ]
        );
    }

    fn web_invites_a_scoped_editor_answers_inside_the_scope_only() {
        let list = [inv("ann@example.com", "x-1", Role::Editor)];
        let ok = decide(
            &tailnet("ann@example.com"),
            Some(&list),
            Some(&chain(&["x-9", "x-1"])),
            Role::Editor,
        );
        assert_eq!(ok, Ok("ann@example.com".to_string()));
        let out = decide(
            &tailnet("ann@example.com"),
            Some(&list),
            Some(&chain(&["x-2", "x-3"])),
            Role::Editor,
        );
        assert_eq!(out.unwrap_err().code, 403);
    }

    fn web_invites_a_low_role_an_unlisted_login_and_a_wide_bind_all_refuse() {
        let list = [
            inv("view@example.com", "*", Role::Viewer),
            inv("talk@example.com", "*", Role::Commenter),
        ];
        let c = chain(&["x-1"]);
        for who in ["view@example.com", "talk@example.com"] {
            let r = decide(&tailnet(who), Some(&list), Some(&c), Role::Editor);
            assert_eq!(r.unwrap_err().code, 403, "{who}");
        }
        let r = decide(&tailnet("eve@example.com"), Some(&list), None, Role::Viewer);
        assert_eq!(r.unwrap_err().code, 403);
        let wide = Caller {
            login: Some("view@example.com"),
            loopback_bind: false,
            loopback_host: false,
        };
        assert_eq!(
            decide(&wide, Some(&list), None, Role::Viewer)
                .unwrap_err()
                .code,
            403
        );
        // A viewer reads: no node, any scope.
        assert!(decide(
            &tailnet("view@example.com"),
            Some(&list),
            None,
            Role::Viewer
        )
        .is_ok());
    }

    #[test]
    fn web_invites_decide_table() {
        web_invites_parse_drops_bad_rows_and_lowercases();
        web_invites_a_scoped_editor_answers_inside_the_scope_only();
        web_invites_a_low_role_an_unlisted_login_and_a_wide_bind_all_refuse();
    }
}
