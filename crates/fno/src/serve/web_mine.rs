//! The Mine section of `/hq`: personal views and a look per login, per
//! project. A login is the name `web_invites::decide` returns: a lowercased
//! invitee login, `"local"` at the owner's own machine, or `"token"` when no
//! invite list is set. `"token"` names nobody, so it owns no store.
//!
//! The store is `<state root>/mux/web-mine/<login key>/<project>.json`. The
//! key is a hash of the login, so no email lands in a path. A view's live
//! blocks fill from [`hosted_read`], which answers a closed table from rows
//! the bridge already holds and never starts a process.

use std::path::{Path, PathBuf};

use fno_proto::pack::View;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::node_row::NodeRow;
use crate::web_invites::Invite;

/// How many prior versions a store file keeps.
const HISTORY: usize = 3;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Theme {
    #[default]
    Auto,
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Density {
    #[default]
    Comfortable,
    Compact,
}

/// Tints only the Mine section's own frame: hue carries state everywhere else.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Accent {
    #[default]
    Ink,
    Blue,
    Green,
    Violet,
}

/// A look is a pick from closed sets, never raw CSS: a `url()` in CSS can
/// send data off the page.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Look {
    pub(crate) theme: Theme,
    pub(crate) density: Density,
    pub(crate) accent: Accent,
}

pub(crate) const LOOK_VALUES: &str =
    "theme auto|light|dark, density comfortable|compact, accent ink|blue|green|violet";

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Version {
    #[serde(default)]
    look: Look,
    #[serde(default)]
    views: Vec<View>,
}

/// One store file: the live look and views, and the prior versions, oldest first.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Doc {
    #[serde(default)]
    look: Look,
    #[serde(default)]
    views: Vec<View>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    history: Vec<Version>,
}

/// The store root under the mux sidecar root, so a pinned `FNO_CONFIG`
/// isolates it with the sockets.
pub(crate) fn store_root() -> PathBuf {
    #[cfg(not(test))]
    return crate::proto::mux_sidecar_root()
        .join("mux")
        .join("web-mine");
    #[cfg(test)]
    std::env::temp_dir().join(format!("fno-web-mine-test-{}", std::process::id()))
}

/// The folder name for a login: the first 16 hex digits of its SHA-256.
pub(crate) fn login_key(login: &str) -> String {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(login.trim().to_ascii_lowercase().as_bytes());
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// A project name is one safe path component.
fn safe_project(project: &str) -> bool {
    !project.is_empty()
        && project != "."
        && project != ".."
        && project.len() <= 128
        && project
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn login_dir(root: &Path, person: &str) -> Option<PathBuf> {
    (person != "token").then(|| root.join(login_key(person)))
}

/// The login has a store folder. A login with none needs no row read.
pub(crate) fn has_store(root: &Path, person: &str) -> bool {
    login_dir(root, person).is_some_and(|d| d.is_dir())
}

/// A missing file is an empty doc. A file that does not parse is an error
/// naming the serde reason, and nothing reads past it.
fn read_doc(path: &Path) -> std::io::Result<Doc> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(serde_json::from_str(&text)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Doc::default()),
        Err(e) => Err(e),
    }
}

/// The caller may read node `id`. `"local"` reads every row; a login reads a
/// row when one of its invites is `*` or names the row or an ancestor.
pub(crate) fn in_scope(
    rows: &[NodeRow],
    invites: Option<&[Invite]>,
    person: &str,
    id: &str,
) -> bool {
    if person == "local" {
        return true;
    }
    let Some(invites) = invites else {
        return false;
    };
    let mine: Vec<&Invite> = invites.iter().filter(|i| i.email == person).collect();
    if mine.iter().any(|i| i.scope == "*") {
        return true;
    }
    let chain = crate::web_answer::chain_of(rows, Some(id));
    mine.iter().any(|i| chain.contains(&i.scope))
}

/// Answer one live read from the rows in hand. The closed table:
/// `fno backlog get <id> [--json]` and `fno backlog list [--json]`, both held
/// to `project` and to `scope`. Any other argv answers `None`, so the block
/// keeps its declared rows and the read is named as refused.
pub(crate) fn hosted_read(
    rows: &[NodeRow],
    project: &str,
    scope: &dyn Fn(&str) -> bool,
    argv: &[String],
) -> Option<Value> {
    let ours =
        |r: &&NodeRow| r.project.as_deref() == Some(project) && r.id().is_some_and(|id| scope(id));
    let args: Vec<&str> = argv.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["fno", "backlog", "get", id] | ["fno", "backlog", "get", id, "--json"] => rows
            .iter()
            .find(|r| r.id() == Some(*id))
            .filter(ours)
            .map(NodeRow::to_value),
        ["fno", "backlog", "list"] | ["fno", "backlog", "list", "--json"] => {
            let found: Vec<Value> = rows.iter().filter(ours).map(NodeRow::to_value).collect();
            let n = found.len();
            Some(json!({ "rows": found, "total": n, "showing": n, "hidden": 0 }))
        }
        _ => None,
    }
}

/// Every project file of the caller, each view filled through
/// [`hosted_read`]. The top-level `look` is the first readable file's, and
/// `look_project` names it so the page posts a change to the same file.
pub(crate) fn mine_json(
    root: &Path,
    rows: &[NodeRow],
    invites: Option<&[Invite]>,
    person: &str,
) -> Value {
    let kind = match person {
        "local" => "local",
        "token" => "none",
        _ => "login",
    };
    let mut out = json!({
        "person": kind,
        "look": Look::default(),
        "look_project": Value::Null,
        "projects": [],
    });
    let Some(dir) = login_dir(root, person) else {
        return out;
    };
    let mut files: Vec<(String, PathBuf)> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let project = path
                .file_name()?
                .to_str()?
                .strip_suffix(".json")?
                .to_string();
            safe_project(&project).then_some((project, path))
        })
        .collect();
    files.sort();
    let scope = |id: &str| in_scope(rows, invites, person, id);
    let mut projects = Vec::new();
    for (project, path) in files {
        let doc = match read_doc(&path) {
            Ok(doc) => doc,
            Err(e) => {
                projects
                    .push(json!({ "project": project, "views": [], "refused": [], "broken": e.to_string() }));
                continue;
            }
        };
        if out["look_project"].is_null() {
            out["look"] = json!(doc.look);
            out["look_project"] = json!(&project);
        }
        let mut refused = Vec::new();
        let views: Vec<View> = doc
            .views
            .into_iter()
            .map(|mut view| {
                refused.extend(view.fill_live(&|argv| hosted_read(rows, &project, &scope, argv)));
                view
            })
            .collect();
        projects.push(json!({
            "project": project,
            "look": doc.look,
            "views": views,
            "refused": refused,
        }));
    }
    out["projects"] = json!(projects);
    out
}

/// A refused look write: the HTTP status and one line the page shows.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Refused(pub(crate) u16, pub(crate) String);

/// Store `look` for the caller's `project`. The prior version goes onto the
/// history, which keeps the newest [`HISTORY`]. A file that does not parse
/// stays as it is, so a hand edit is never lost.
pub(crate) fn set_look(
    root: &Path,
    person: &str,
    project: &str,
    look: Look,
) -> Result<Look, Refused> {
    let Some(dir) = login_dir(root, person) else {
        return Err(Refused(
            403,
            "no login on this request: personal look needs an invite".into(),
        ));
    };
    if !safe_project(project) {
        return Err(Refused(
            400,
            "the project is one name of letters, digits, '-', '_' or '.'".into(),
        ));
    }
    let path = dir.join(format!("{project}.json"));
    let existed = path.exists();
    let mut doc = read_doc(&path).map_err(|e| {
        Refused(
            409,
            format!("the store file {project}.json does not parse, so it was left as it is: {e}"),
        )
    })?;
    if existed {
        doc.history.push(Version {
            look: doc.look,
            views: doc.views.clone(),
        });
        let over = doc.history.len().saturating_sub(HISTORY);
        doc.history.drain(..over);
    }
    doc.look = look;
    write_doc(&dir, &path, &doc)
        .map_err(|e| Refused(500, format!("cannot write the store: {e}")))?;
    Ok(look)
}

/// Write through a temp file and a rename in the same folder.
fn write_doc(dir: &Path, path: &Path, doc: &Doc) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let body = serde_json::to_vec_pretty(doc).map_err(std::io::Error::other)?;
    // One bridge process can run two writes at once, so the pid alone does
    // not make the temp name unique.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("json.tmp-{}-{seq}", std::process::id()));
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web_invites::Role;

    fn rows() -> Vec<NodeRow> {
        serde_json::from_str(
            r#"[{"id":"x-1","project":"fno","title":"one"},
                {"id":"x-2","project":"fno","parent":"x-1","title":"two"},
                {"id":"x-9","project":"fno","title":"nine"},
                {"id":"y-1","project":"other","title":"elsewhere"}]"#,
        )
        .unwrap()
    }

    fn invites() -> Vec<Invite> {
        vec![Invite {
            email: "a@example.com".into(),
            scope: "x-1".into(),
            role: Role::Viewer,
        }]
    }

    fn argv(s: &str) -> Vec<String> {
        s.split(' ').map(str::to_string).collect()
    }

    fn temp_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("fno-web-mine-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    fn table_view(read: &str, pointer: &str) -> View {
        serde_json::from_value(json!({
            "title": "mine",
            "blocks": [{"kind": "table", "columns": [{"key": "/title", "title": "Title"}],
                        "rows": [{"title": "declared"}],
                        "live": {"argv": argv(read), "rows": pointer}}]
        }))
        .unwrap()
    }

    // AC2: the hosted reader answers only its table, only inside the chain,
    // and a refused read keeps the declared rows and is named.
    #[test]
    fn hosted_read_holds_to_the_invite_chain() {
        let rows = rows();
        let inv = invites();
        let scope = |id: &str| in_scope(&rows, Some(&inv), "a@example.com", id);
        assert_eq!(
            hosted_read(&rows, "fno", &scope, &argv("fno backlog get x-9 --json")),
            None
        );
        assert_eq!(
            hosted_read(&rows, "fno", &scope, &argv("fno backlog board")),
            None
        );
        assert_eq!(
            hosted_read(&rows, "fno", &scope, &argv("fno backlog get x-2 --fix")),
            None
        );
        let list = hosted_read(&rows, "fno", &scope, &argv("fno backlog list --json")).unwrap();
        let ids: Vec<&str> = list["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["id"].as_str())
            .collect();
        assert_eq!(ids, ["x-1", "x-2"]);
        assert_eq!(list["total"], 2);
        let got = hosted_read(&rows, "fno", &scope, &argv("fno backlog get x-2")).unwrap();
        assert_eq!(got["title"], "two");
        // Another project's row never answers, even for the owner.
        let all = |_: &str| true;
        assert_eq!(
            hosted_read(&rows, "fno", &all, &argv("fno backlog get y-1")),
            None
        );

        let mut view = table_view("fno backlog get x-9 --json", "");
        let failed = view.fill_live(&|a| hosted_read(&rows, "fno", &scope, a));
        assert_eq!(failed, ["fno backlog get x-9 --json"]);
        assert_eq!(
            view,
            table_view("fno backlog get x-9 --json", ""),
            "declared rows stay"
        );
        assert!(in_scope(&rows, None, "local", "x-9"));
        assert!(!in_scope(&rows, None, "token", "x-1"));
    }

    // AC1: one login's view draws for that login only; a login with no
    // store gets no projects.
    #[test]
    fn mine_json_draws_each_login_its_own_views() {
        let root = temp_root("ac1");
        let rows = rows();
        let inv = invites();
        let dir = root.join(login_key("A@example.com "));
        std::fs::create_dir_all(&dir).unwrap();
        let mut view = table_view("fno backlog list --json", "/rows");
        view.title = Some("my two".into());
        let doc = json!({"look": {"theme": "dark"}, "views": [view]});
        std::fs::write(dir.join("fno.json"), doc.to_string()).unwrap();

        let a = mine_json(&root, &rows, Some(&inv), "a@example.com");
        assert_eq!(a["person"], "login");
        assert_eq!(a["look"]["theme"], "dark");
        assert_eq!(a["look_project"], "fno");
        let projects = a["projects"].as_array().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0]["project"], "fno");
        assert_eq!(projects[0]["views"][0]["title"], "my two");
        assert_eq!(
            projects[0]["views"][0]["blocks"][0]["rows"][1]["title"],
            "two"
        );
        assert!(
            !a.to_string().contains("example.com"),
            "no login in the answer"
        );

        let b = mine_json(&root, &rows, Some(&inv), "b@example.com");
        assert_eq!(b["projects"], json!([]));
        assert!(!has_store(&root, "b@example.com"));
        assert_eq!(
            mine_json(&root, &rows, None, "token")["projects"],
            json!([])
        );

        // A hand edit that does not parse draws nothing and names the reason.
        std::fs::write(dir.join("fno.json"), r#"{"views": [], "colour": 1}"#).unwrap();
        let broken = mine_json(&root, &rows, Some(&inv), "a@example.com");
        assert!(broken["projects"][0]["broken"]
            .as_str()
            .unwrap()
            .contains("colour"));
        let _ = std::fs::remove_dir_all(&root);
    }

    // AC3 and AC4: four writes keep three prior versions; a look outside the
    // closed set or a broken file changes nothing.
    #[test]
    fn set_look_keeps_three_versions_and_refuses_the_rest() {
        let root = temp_root("ac3");
        let looks = [
            Look::default(),
            Look {
                theme: Theme::Light,
                ..Look::default()
            },
            Look {
                density: Density::Compact,
                ..Look::default()
            },
            Look {
                theme: Theme::Dark,
                density: Density::Compact,
                accent: Accent::Blue,
            },
        ];
        for look in looks {
            assert_eq!(set_look(&root, "a@example.com", "fno", look), Ok(look));
        }
        let path = root.join(login_key("a@example.com")).join("fno.json");
        let doc = read_doc(&path).unwrap();
        assert_eq!(doc.look, looks[3]);
        assert_eq!(doc.history.len(), 3);
        assert_eq!(doc.history[2].look, looks[2]);

        for bad in [
            json!({"accent": "url(https://x)"}),
            json!({"theme": "dark", "css": "body{}"}),
        ] {
            assert!(serde_json::from_value::<Look>(bad).is_err());
        }
        assert_eq!(
            set_look(&root, "token", "fno", Look::default())
                .unwrap_err()
                .0,
            403
        );
        assert_eq!(
            set_look(&root, "a@example.com", "../x", Look::default())
                .unwrap_err()
                .0,
            400
        );
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(
            set_look(&root, "a@example.com", "fno", Look::default())
                .unwrap_err()
                .0,
            409
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{not json");
        let _ = std::fs::remove_dir_all(&root);
    }
}
