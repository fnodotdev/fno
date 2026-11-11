//! `fno config plugins gallery`: published packs, read from GitHub topics.
//!
//! One `gh api search/repositories` read per topic, merged by repo name. It
//! never clones and never installs: each row carries the install command and
//! reads "not reviewed by fno". A failed read exits nonzero with gh's own
//! error line, never an empty list.

use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::process::Command;

/// The topics an author tags a repo with, one per component kind.
pub const TOPICS: [&str; 3] = ["fno-mod", "fno-layout", "fno-pack"];

#[derive(Debug, Deserialize)]
struct Search {
    items: Vec<Repo>,
}

#[derive(Debug, Deserialize)]
struct Owner {
    login: String,
}

#[derive(Debug, Deserialize)]
struct Repo {
    full_name: String,
    owner: Owner,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    stargazers_count: u64,
    #[serde(default)]
    pushed_at: Option<String>,
    clone_url: String,
}

#[derive(Debug, PartialEq)]
pub struct Row {
    pub name: String,
    pub owner: String,
    pub description: String,
    pub stars: u64,
    pub pushed_at: String,
    pub topic: &'static str,
    pub install: String,
}

#[derive(Debug)]
pub enum GalleryError {
    /// gh exited nonzero or could not start; carries its error line.
    Gh(String),
    /// gh answered with JSON this read cannot parse.
    Parse(String),
}

impl std::fmt::Display for GalleryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GalleryError::Gh(m) => write!(f, "gh read failed: {m}"),
            GalleryError::Parse(m) => write!(f, "gh answer did not parse: {m}"),
        }
    }
}

/// The gallery rows, read through the `gh` program named.
pub fn read(gh: &str) -> Result<Vec<Row>, GalleryError> {
    let mut rows: BTreeMap<String, Row> = BTreeMap::new();
    for topic in TOPICS {
        let output = Command::new(gh)
            .args([
                "api",
                "-X",
                "GET",
                "search/repositories",
                "-f",
                &format!("q=topic:{topic}"),
                "-f",
                "per_page=100",
            ])
            .output()
            .map_err(|e| GalleryError::Gh(format!("{gh}: {e}")))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let line = stderr
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("gh exited nonzero");
            return Err(GalleryError::Gh(line.trim().to_string()));
        }
        let search: Search = serde_json::from_slice(&output.stdout)
            .map_err(|e| GalleryError::Parse(e.to_string()))?;
        for repo in search.items {
            rows.entry(repo.full_name.clone()).or_insert(Row {
                install: format!("fno config plugins install {}", repo.clone_url),
                name: repo.full_name,
                owner: repo.owner.login,
                description: repo.description.unwrap_or_default(),
                stars: repo.stargazers_count,
                pushed_at: repo.pushed_at.unwrap_or_default(),
                topic,
            });
        }
    }
    let mut rows: Vec<Row> = rows.into_values().collect();
    rows.sort_by(|a, b| b.stars.cmp(&a.stars).then_with(|| a.name.cmp(&b.name)));
    Ok(rows)
}

pub fn run(as_json: bool) -> i32 {
    let rows = match read("gh") {
        Ok(rows) => rows,
        Err(e) => {
            if as_json {
                println!("{}", json!({"error": e.to_string(), "version": 1}));
            } else {
                eprintln!("{e}");
            }
            return 1;
        }
    };
    if as_json {
        let items: Vec<_> = rows
            .iter()
            .map(|r| {
                json!({
                    "name": r.name,
                    "owner": r.owner,
                    "description": r.description,
                    "stars": r.stars,
                    "pushed_at": r.pushed_at,
                    "topic": r.topic,
                    "install": r.install,
                    "reviewed": false,
                })
            })
            .collect();
        println!("{}", json!({"repos": items, "version": 1}));
        return 0;
    }
    if rows.is_empty() {
        println!("no published packs under topics {}", TOPICS.join(", "));
        return 0;
    }
    for r in &rows {
        println!(
            "{}  {} stars  {}  (not reviewed by fno)",
            r.name, r.stars, r.topic
        );
        if !r.description.is_empty() {
            println!("  {}", r.description);
        }
        println!("  {}", r.install);
    }
    0
}
