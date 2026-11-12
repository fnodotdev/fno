//! The `fno config plugins` feeds both hosts read: the mux settings tab and
//! the desktop app's Extensions view. One parser, one door to the `fno`
//! front door, one pane launcher, so the two hosts show one list and the
//! consent prompt stays the CLI's.

use serde::Deserialize;

pub const LS: [&str; 4] = ["config", "plugins", "ls", "--json"];
pub const GALLERY: [&str; 4] = ["config", "plugins", "gallery", "--json"];

/// One mod or layout row of `ls --json`, with its pack's install commit.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Component {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub preview: String,
    #[serde(default)]
    pub recipe: String,
    pub enabled: bool,
    #[serde(default)]
    pub runs_code: bool,
    /// `run`, `feed` or `native`; a layout carries none.
    #[serde(default)]
    pub kind: String,
    /// A command mod's argv.
    #[serde(default)]
    pub run: Option<Vec<String>>,
    /// The commit a URL install recorded; empty for a local pack.
    #[serde(skip)]
    pub commit: String,
}

#[derive(Debug, Clone, Deserialize)]
struct SourceRow {
    commit: String,
}

#[derive(Debug, Clone, Deserialize)]
struct PackRow {
    pack_id: String,
    #[serde(default)]
    source: Option<SourceRow>,
    #[serde(default)]
    mods: Vec<Component>,
    #[serde(default)]
    layouts: Vec<Component>,
}

#[derive(Debug, Deserialize)]
struct Listing {
    packs: Vec<PackRow>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Published {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub stars: u64,
    pub install: String,
    #[serde(default)]
    pub reviewed: bool,
}

#[derive(Debug, Deserialize)]
struct GalleryRead {
    repos: Vec<Published>,
}

/// A failed `fno config plugins` read, as the one line a page paints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadFailed(pub String);

impl std::fmt::Display for ReadFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One argv shown so a reader can paste it: plain words bare, anything
/// else single-quoted.
pub fn shell_words(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            let plain = !a.is_empty()
                && a.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_./=:@,+".contains(c));
            if plain {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', r"'\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The one door to the plugins owner: every read and pane runs the fno front
/// door, which hands `config plugins` to fno-agents.
fn fno_door() -> std::path::PathBuf {
    door_for(crate::server::fno_bin())
}

/// The desktop app is its own executable, which does not speak `config
/// plugins`: take the `fno` beside it, else the one on PATH.
fn door_for(exe: std::path::PathBuf) -> std::path::PathBuf {
    if exe.file_stem().is_some_and(|s| s == "fno-app") {
        let sibling = exe.with_file_name("fno");
        return if sibling.exists() {
            sibling
        } else {
            "fno".into()
        };
    }
    exe
}

/// The argv of `fno config plugins <args>` through the one door, for a host
/// that execs it as a pane's own command.
pub fn door_argv(args: &[&str]) -> Vec<String> {
    let mut argv = vec![fno_door().to_string_lossy().into_owned()];
    argv.extend(
        ["config", "plugins"]
            .iter()
            .chain(args)
            .map(|a| a.to_string()),
    );
    argv
}

pub fn fno_json(args: &[&str]) -> Result<Vec<u8>, ReadFailed> {
    let shown = format!("fno {}", args.join(" "));
    let mut cmd = crate::process_admission::std_command(fno_door());
    cmd.args(args).stdin(std::process::Stdio::null());
    let out = crate::process_admission::std_output(&mut cmd)
        .map_err(|e| ReadFailed(format!("{shown} did not start: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let first = stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        return Err(ReadFailed(format!(
            "{shown} exited {}: {}",
            out.status.code().unwrap_or(-1),
            first.trim()
        )));
    }
    Ok(out.stdout)
}

/// Parse `ls --json` into `(pack id, component)` rows, mods before layouts
/// per pack.
pub fn parse_installed(
    read: Result<Vec<u8>, ReadFailed>,
) -> Result<Vec<(String, Component)>, ReadFailed> {
    let bytes = read?;
    let listing: Listing = serde_json::from_slice(&bytes)
        .map_err(|e| ReadFailed(format!("fno {} printed no listing: {e}", LS.join(" "))))?;
    Ok(listing
        .packs
        .into_iter()
        .flat_map(|p| {
            let id = p.pack_id;
            let commit = p.source.map(|s| s.commit).unwrap_or_default();
            p.mods.into_iter().chain(p.layouts).map(move |mut c| {
                c.commit = commit.clone();
                (id.clone(), c)
            })
        })
        .collect())
}

/// Read and parse `gallery --json`.
pub fn read_gallery() -> Result<Vec<Published>, ReadFailed> {
    fno_json(&GALLERY).and_then(|bytes| parse_gallery(&bytes))
}

pub fn parse_gallery(bytes: &[u8]) -> Result<Vec<Published>, ReadFailed> {
    serde_json::from_slice::<GalleryRead>(bytes)
        .map(|g| g.repos)
        .map_err(|e| ReadFailed(format!("fno {} printed no gallery: {e}", GALLERY.join(" "))))
}

/// Turn `pack/id` on or off through `config plugins enable|disable`.
pub fn switch(target: &str, on: bool) -> Result<(), ReadFailed> {
    let command = if on { "enable" } else { "disable" };
    fno_json(&["config", "plugins", command, target]).map(|_| ())
}

/// The `mux pane run` argv that shows `fno config plugins <args>` in a pane
/// split to the right of `anchor`, through `sh`, which keeps the pane open on
/// the result until Enter. The argv rides as positional parameters, so no
/// word of it is shell-parsed. A split needs an anchor: neither host is a
/// pane, so FNO_PANE is unset and the focused pane rides as `--at`. `server`
/// names the mux server the pane opens on.
pub fn pane_argv(fno_text: &str, args: &[&str], anchor: u64, server: Option<&str>) -> Vec<String> {
    let script = r#"fno="$1"; shift; "$fno" config plugins "$@"; printf '\n[exit %s] press enter to close\n' "$?"; read -r _"#;
    let mut argv: Vec<String> = ["mux", "pane", "run"].map(String::from).into();
    if let Some(server) = server {
        argv.extend(["--server".to_string(), server.to_string()]);
    }
    argv.extend(["--split", "right", "--at"].map(String::from));
    argv.push(anchor.to_string());
    argv.extend(["--", "sh", "-c"].map(String::from));
    argv.extend([script, "sh", fno_text].map(String::from));
    argv.extend(args.iter().map(|a| a.to_string()));
    argv
}

/// Run `fno config plugins <args>` in a new pane beside `anchor` and wait for
/// `pane run` to answer, so the caller reports the real result. Blocks: run it
/// off the UI loop.
pub fn run_in_pane(
    args: &[&str],
    what: &str,
    anchor: u64,
    server: Option<&str>,
) -> Result<(), ReadFailed> {
    if anchor == 0 {
        return Err(ReadFailed("No pane to split yet".into()));
    }
    let fno = fno_door();
    let argv = pane_argv(&fno.to_string_lossy(), args, anchor, server);
    let mut cmd = crate::process_admission::std_command(&fno);
    cmd.args(&argv).stdin(std::process::Stdio::null());
    let out = crate::process_admission::std_output(&mut cmd)
        .map_err(|e| ReadFailed(format!("{what} did not open a pane: {e}")))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let first = stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    Err(ReadFailed(format!(
        "{what} did not open a pane: exit {}: {}",
        out.status.code().unwrap_or(-1),
        first.trim()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_json_names_the_command_and_does_not_panic() {
        let err = parse_installed(Ok(b"Traceback: boom".to_vec())).unwrap_err();
        assert!(
            err.0
                .starts_with("fno config plugins ls --json printed no listing:"),
            "{err}"
        );
    }

    #[test]
    fn the_pane_argv_anchors_the_split_on_the_focused_pane() {
        let argv = pane_argv("/bin/fno", &["install", "https://x/y"], 7, Some("work"));
        let cut = argv.iter().position(|a| a == "--").unwrap();
        let (head, _) = argv.split_at(cut);
        assert_eq!(
            head,
            ["mux", "pane", "run", "--server", "work", "--split", "right", "--at", "7"]
        );
        let tail: Vec<&str> = argv
            .iter()
            .rev()
            .take(3)
            .rev()
            .map(String::as_str)
            .collect();
        assert_eq!(tail, ["/bin/fno", "install", "https://x/y"]);
        assert_eq!(
            door_for("/opt/x/fno".into()),
            std::path::PathBuf::from("/opt/x/fno")
        );
        assert_eq!(
            door_for("/nonexistent/fno-app".into()),
            std::path::PathBuf::from("fno"),
            "the app's own executable is never the door"
        );
        let none = run_in_pane(&["open", "p/m"], "p/m", 0, None).unwrap_err();
        assert_eq!(none.0, "No pane to split yet");
    }
}
