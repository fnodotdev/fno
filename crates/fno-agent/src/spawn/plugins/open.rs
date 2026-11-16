//! `fno config plugins open PACK/MOD`: show one switched-on mod here.
//!
//! A feed mod draws its rows as a text table. This is the one renderer for a
//! data-only mod: the mux opens it in a pane and the GPUI app draws that
//! pane's cells, so neither host carries a second widget renderer. On a
//! terminal it redraws every few seconds until interrupted; piped, it prints
//! once. A missing or failing feed prints one line naming the command and
//! exits nonzero, so the pane never hangs blank.
//!
//! A command mod runs its argv from the pack directory, after the launch
//! check proves the tree still hashes to the digest consent recorded. A
//! native mod names a view the mux paints itself, so it opens there. A view
//! mod's blocks print as text, its live tables and lists read fresh on each
//! redraw, so the mux can open it in a pane like a feed.

use super::registry::Store;
use super::ui::{Feed, Render};
use super::verify::load_manifest;
use serde_json::Value;
use std::path::Path;
use std::process::Command;

const REFRESH_SECONDS: u64 = 5;
/// A feed read past this bound is killed and the panel names it.
const FEED_SECONDS: u64 = 30;

#[derive(Debug)]
pub enum OpenError {
    /// The target names no installed, switched-on mod this host can show.
    Target(String),
    /// The feed command failed to run or exited nonzero.
    Feed(String),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::Target(m) | OpenError::Feed(m) => f.write_str(m),
        }
    }
}

/// The feed rows as aligned text, one header line then one line per row.
pub fn table(feed: &Feed, output: &Value) -> Vec<String> {
    let rows: &[Value] = if feed.rows.is_empty() {
        output.as_array().map(Vec::as_slice).unwrap_or(&[])
    } else {
        output
            .pointer(&feed.rows)
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    };
    let cell = |row: &Value, key: &str| match row.pointer(key) {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    };
    let grid: Vec<Vec<String>> = rows
        .iter()
        .map(|row| feed.columns.iter().map(|c| cell(row, &c.key)).collect())
        .collect();
    let widths: Vec<usize> = feed
        .columns
        .iter()
        .enumerate()
        .map(|(i, c)| {
            grid.iter()
                .map(|r| r.get(i).map_or(0, |c| c.chars().count()))
                .chain([c.title.chars().count()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let line = |cells: Vec<&str>| {
        cells
            .iter()
            .zip(&widths)
            .map(|(text, width)| format!("{text:<width$}"))
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_string()
    };
    let mut out = vec![line(
        feed.columns.iter().map(|c| c.title.as_str()).collect(),
    )];
    if grid.is_empty() {
        out.push("(no rows)".to_string());
    }
    for row in &grid {
        out.push(line(row.iter().map(String::as_str).collect()));
    }
    out
}

/// A declared view's blocks as text lines: a heading, prose, bullets, an
/// aligned table, and `key: value` pairs. Live blocks read first; a read
/// outside the known reads never runs.
pub fn view_lines(view: &fno_proto::pack::View) -> Vec<String> {
    use fno_proto::pack::Block;
    let mut view = view.clone();
    let failed = view.fill_live(&|argv| {
        if !super::ui::feed_is_read(argv) {
            return None;
        }
        let feed = Feed {
            argv: argv.to_vec(),
            rows: String::new(),
            columns: Vec::new(),
        };
        feed_output(&feed).ok()
    });
    let mut out = Vec::new();
    if let Some(title) = &view.title {
        out.push(title.clone());
        out.push(String::new());
    }
    for block in &view.blocks {
        match block {
            Block::Heading { text } => out.push(format!("== {text}")),
            Block::Text { text } => out.push(text.clone()),
            Block::List { items, .. } => out.extend(items.iter().map(|i| format!("  • {i}"))),
            Block::Table { columns, rows, .. } => {
                // A declared row is keyed by the pointer's name; escape it
                // back into a one-segment pointer for the feed table.
                let escaped = |key: &str| {
                    format!(
                        "/{}",
                        key.trim_start_matches('/')
                            .replace('~', "~0")
                            .replace('/', "~1")
                    )
                };
                let feed = Feed {
                    argv: Vec::new(),
                    rows: String::new(),
                    columns: columns
                        .iter()
                        .map(|c| super::ui::Column {
                            key: escaped(&c.key),
                            title: c.title.clone(),
                        })
                        .collect(),
                };
                out.extend(table(&feed, &serde_json::json!(rows)));
            }
            Block::KeyValue { entries } => {
                out.extend(entries.iter().map(|e| format!("{}: {}", e.key, e.value)))
            }
        }
        out.push(String::new());
    }
    for argv in failed {
        out.push(format!("live read `{argv}` failed; the declared rows show"));
    }
    out
}

fn feed_output(feed: &Feed) -> Result<Value, OpenError> {
    let shown = feed.argv.join(" ");
    let (program, args) = feed
        .argv
        .split_first()
        .ok_or_else(|| OpenError::Feed("the feed declares no command".into()))?;
    let mut cmd = Command::new(program);
    cmd.args(args);
    let output = crate::bounded_cmd::output_with_timeout_result(cmd, FEED_SECONDS)
        .map_err(|e| OpenError::Feed(format!("feed `{shown}` did not start: {e}")))?;
    if !output.status.success() {
        return Err(OpenError::Feed(format!(
            "feed `{shown}` exited {}",
            output.status.code().unwrap_or(-1)
        )));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|e| OpenError::Feed(format!("feed `{shown}` printed no JSON: {e}")))
}

/// What `open` does with a resolved mod.
enum Opened {
    Feed(Feed),
    View(std::path::PathBuf),
    Run {
        argv: Vec<String>,
        dir: std::path::PathBuf,
    },
}

/// An installed, switched-on mod.
fn resolve(root: &Path, target: &str) -> Result<Opened, OpenError> {
    let (pack_id, mod_id) = target
        .split_once('/')
        .ok_or_else(|| OpenError::Target(format!("name a mod as PACK_ID/MOD_ID, got {target}")))?;
    let registry = Store::at_root(root)
        .load()
        .map_err(|e| OpenError::Target(e.0))?;
    let pack = registry
        .pack_by_id(pack_id)
        .ok_or_else(|| OpenError::Target(format!("no installed pack {pack_id}")))?;
    let manifest = load_manifest(Path::new(&pack.manifest_path))
        .map_err(|_| OpenError::Target(format!("manifest for {pack_id} no longer readable")))?;
    let found = manifest
        .mods
        .iter()
        .find(|m| m.id == mod_id)
        .ok_or_else(|| OpenError::Target(format!("pack {pack_id} declares no mod {mod_id}")))?;
    if !pack.enabled(&found.id, found.runs_code()) {
        return Err(OpenError::Target(format!(
            "{target} is off: fno config plugins enable {target}"
        )));
    }
    match found.render() {
        Some(Render::Feed(feed)) => Ok(Opened::Feed(feed.clone())),
        Some(Render::Run(argv)) => {
            super::install::launch_check(root, pack_id)
                .map_err(|e| OpenError::Target(e.to_string()))?;
            let dir = Path::new(&pack.manifest_path)
                .parent()
                .unwrap_or(Path::new("."))
                .to_path_buf();
            Ok(Opened::Run {
                argv: argv.to_vec(),
                dir,
            })
        }
        Some(Render::Native(view)) => Err(OpenError::Target(format!(
            "{target} is the mux's own {view} view; open it from the mux"
        ))),
        Some(Render::View(path)) => Ok(Opened::View(
            Path::new(&pack.manifest_path)
                .parent()
                .unwrap_or(Path::new("."))
                .join(path),
        )),
        None => Err(OpenError::Target(format!(
            "{target} declares no render path"
        ))),
    }
}

fn run_command(argv: &[String], dir: &Path) -> i32 {
    let Some((program, args)) = argv.split_first() else {
        return 2;
    };
    match Command::new(program).args(args).current_dir(dir).status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            println!("mod command `{}` did not start: {e}", argv.join(" "));
            1
        }
    }
}

pub fn run(root: &Path, target: &str) -> i32 {
    match resolve(root, target) {
        Ok(Opened::Feed(feed)) => redraw(|| feed_output(&feed).map(|out| table(&feed, &out))),
        Ok(Opened::View(path)) => match read_view(&path) {
            Ok(view) => redraw(|| Ok(view_lines(&view))),
            Err(e) => {
                eprintln!("{e}");
                1
            }
        },
        Ok(Opened::Run { argv, dir }) => run_command(&argv, &dir),
        Err(e) => {
            eprintln!("{e}");
            2
        }
    }
}

fn read_view(path: &Path) -> Result<fno_proto::pack::View, OpenError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| OpenError::Target(format!("cannot read view {}: {e}", path.display())))?;
    serde_json::from_str(&text)
        .map_err(|e| OpenError::Target(format!("view {} does not parse: {e}", path.display())))
}

/// Print `draw`'s lines once when piped; on a terminal, clear and redraw
/// every few seconds until interrupted. A failed draw prints its line and,
/// piped, exits 1.
fn redraw(draw: impl Fn() -> Result<Vec<String>, OpenError>) -> i32 {
    let live = unsafe { libc::isatty(1) == 1 };
    loop {
        let drawn = draw();
        if live {
            print!("\x1b[H\x1b[2J");
        }
        match drawn {
            Ok(lines) => {
                for line in lines {
                    println!("{line}");
                }
            }
            Err(e) => {
                println!("{e}");
                if !live {
                    return 1;
                }
            }
        }
        if !live {
            return 0;
        }
        std::thread::sleep(std::time::Duration::from_secs(REFRESH_SECONDS));
    }
}
