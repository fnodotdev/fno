//! Which bytes does a pane's `[Image #N]` stand for?
//!
//! The harness already kept them: Claude in a per-session store, and all
//! three harnesses in the transcript. The mux reads those and never the
//! clipboard. A number can be reused, so the turn is picked first and the
//! bytes come from that turn. With no single turn, nothing opens.
//!
//! Everything here blocks: call it off the core loop.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::chat_events::{ChatReadError, ChatRoots, PastedTurn};
use crate::harness::Harness;
use crate::proto::chat_event::{decode_base64, SessionKey};
use crate::server::image_refs::{placeholder_number, ImageError, Placeholder};

/// The kept transcript images, over all sessions, before the oldest go.
const CACHE_CAP: u64 = 256 * 1024 * 1024;
/// Claude writes the stored file again at submit, just around the row's stamp.
const SUBMIT_SLACK_MS: i64 = 10_000;

/// The trusted answer to whose pane this is.
#[derive(Debug, Clone)]
pub(crate) struct PaneSession {
    pub(crate) harness: Harness,
    /// The harness's own session id, which keys its store and transcript.
    pub(crate) id: String,
}

/// Where pasted images live.
#[derive(Debug, Clone)]
pub(crate) struct Stores {
    pub(crate) chat: ChatRoots,
    pub(crate) claude_tmp: Vec<PathBuf>,
    pub(crate) cache: PathBuf,
}

impl Stores {
    pub(crate) fn host() -> Self {
        // SAFETY: getuid reads the process credential and cannot fail.
        let uid = unsafe { libc::getuid() };
        let leaf = format!("claude-{uid}");
        let set = std::env::var_os("CLAUDE_CODE_TMPDIR").filter(|dir| !dir.is_empty());
        let mut claude_tmp = Vec::new();
        for base in set
            .map(PathBuf::from)
            .into_iter()
            .chain([PathBuf::from("/private/tmp"), PathBuf::from("/tmp")])
        {
            // /tmp is shared, so only this user's own directory is trusted.
            if let Some(dir) = own_dir(&base.join(&leaf), uid) {
                if !claude_tmp.contains(&dir) {
                    claude_tmp.push(dir);
                }
            }
        }
        Self {
            chat: ChatRoots::host(),
            claude_tmp,
            cache: cache_dir(),
        }
    }
}

fn own_dir(dir: &Path, uid: u32) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    // An absent store is the normal answer on a machine without Claude.
    let meta = std::fs::symlink_metadata(dir).ok()?;
    if !meta.is_dir() || meta.uid() != uid {
        return None;
    }
    std::fs::canonicalize(dir).ok()
}

fn cache_dir() -> PathBuf {
    crate::proto::mux_dir().join("images")
}

/// A session id that is safe as one path component.
fn safe_component(id: &str) -> Option<&str> {
    let safe = !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    safe.then_some(id)
}

pub(crate) fn resolve(
    paste: &Placeholder,
    session: &PaneSession,
    stores: &Stores,
) -> Result<PathBuf, ImageError> {
    let number = paste.number;
    let claude = session.harness == Harness::Claude;
    let stored = if claude {
        stored_file(paste.number, &session.id, &stores.claude_tmp)
    } else {
        None
    };
    if paste.composing {
        // The message being typed has no turn, so no older turn may answer.
        return stored.map(|(path, _)| path).ok_or(if claude {
            ImageError::NoLongerStored { number }
        } else {
            ImageError::NotSent { number }
        });
    }
    let key = SessionKey {
        harness: session.harness.as_str().to_string(),
        session_id: session.id.clone(),
        subagent: None,
    };
    let turns = match crate::chat_events::pasted_turns(&key, &stores.chat) {
        Ok(turns) => turns,
        Err(ChatReadError::NotFound { .. }) => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    let candidates: Vec<&PastedTurn> = turns
        .iter()
        .filter(|turn| turn.images.iter().any(|image| image.number == number))
        .collect();
    let Some(newest) = candidates.last() else {
        // Only the draft takes the store with no turn to vouch for it: a
        // placeholder elsewhere can be quoted from another session.
        return Err(if stored.is_some() {
            ImageError::NoMatch { number }
        } else {
            ImageError::NoLongerStored { number }
        });
    };
    let turn = only_match(&candidates, &paste.row_text).ok_or(ImageError::NoMatch { number })?;
    if let Some((path, written_ms)) = stored {
        // The store holds the newest use only, at full size.
        let untouched = turn
            .at_ms
            .is_some_and(|at| written_ms <= at.saturating_add(SUBMIT_SLACK_MS));
        if std::ptr::eq(turn, *newest) && untouched {
            return Ok(path);
        }
    }
    let bytes = turn
        .images
        .iter()
        .find(|image| image.number == number)
        .and_then(|image| image.base64.as_deref())
        .and_then(decode_base64)
        .ok_or(ImageError::NoLongerStored { number })?;
    keep(&bytes, &session.id, &stores.cache, number)
}

/// The one turn whose text holds the row's text. A row with nothing but
/// placeholders names a turn only when one turn has that number.
fn only_match<'a>(candidates: &[&'a PastedTurn], row: &str) -> Option<&'a PastedTurn> {
    let key = squash(
        row.trim_start_matches(|c: char| !(c.is_alphanumeric() || c == '['))
            .trim_end()
            .trim_end_matches('…'),
    );
    let bare = only_placeholders(&key);
    let mut hits = candidates
        .iter()
        .filter(|turn| bare || squash(&turn.text).contains(&key));
    let hit = hits.next()?;
    hits.next().is_none().then_some(*hit)
}

fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn only_placeholders(text: &str) -> bool {
    let mut rest = text.trim();
    while !rest.is_empty() {
        let Some(end) = rest.find(']') else {
            return false;
        };
        let (marker, tail) = rest.split_at(end + 1);
        if placeholder_number(marker).is_none() {
            return false;
        }
        rest = tail.trim_start();
    }
    true
}

/// The newest `<tmp>/<any slug>/<session>/images/<N>.<ext>` and its mtime.
/// The slug follows the cwd, and a reused number can change the extension.
fn stored_file(number: u64, session: &str, roots: &[PathBuf]) -> Option<(PathBuf, i64)> {
    let session = safe_component(session)?;
    let stem = number.to_string();
    let mut newest: Option<(PathBuf, i64)> = None;
    for root in roots {
        // An unreadable level holds no image to show.
        for slug in std::fs::read_dir(root).into_iter().flatten().flatten() {
            let dir = slug.path().join(session).join("images");
            for file in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let path = file.path();
                if path.file_stem().and_then(|s| s.to_str()) != Some(stem.as_str()) {
                    continue;
                }
                let Some(real) = crate::server::image_refs::probe(&path) else {
                    continue;
                };
                // A store link out of the store is not this session's image.
                if !real.starts_with(root) {
                    continue;
                }
                let Some(at) = modified_ms(&real) else {
                    continue;
                };
                if newest.as_ref().is_none_or(|(_, newest)| at > *newest) {
                    newest = Some((real, at));
                }
            }
        }
    }
    newest
}

fn modified_ms(path: &Path) -> Option<i64> {
    // A file gone since the probe is simply not stored.
    let modified = std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()?;
    let since = modified.duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(since.as_millis()).ok()
}

/// A transcript image as a private, content-addressed file.
fn keep(bytes: &[u8], session: &str, cache: &Path, number: u64) -> Result<PathBuf, ImageError> {
    use image::ImageFormat as F;
    use sha2::{Digest, Sha256};
    let gone = || ImageError::NoLongerStored { number };
    let ext = match image::guess_format(bytes) {
        Ok(F::Png) => "png",
        Ok(F::Jpeg) => "jpg",
        Ok(F::Gif) => "gif",
        Ok(F::WebP) => "webp",
        _ => return Err(gone()),
    };
    let dir = cache.join(safe_component(session).ok_or_else(gone)?);
    crate::proto::ensure_private_dir(cache)?;
    crate::proto::ensure_private_dir(&dir)?;
    let hash: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let path = dir.join(format!("{hash}.{ext}"));
    if !path.is_file() {
        // A named temp file is created 0600, and the rename never shows half an image.
        let mut file = tempfile::NamedTempFile::new_in(&dir)?;
        file.write_all(bytes)?;
        file.persist(&path).map_err(|error| error.error)?;
        trim(cache, &path, CACHE_CAP);
    }
    // The viewer's URI must be absolute even under a relative mux dir.
    Ok(std::fs::canonicalize(&path)?)
}

/// Oldest first until the cache is back under `cap`. Never the file just kept.
fn trim(cache: &Path, just_kept: &Path, cap: u64) {
    let mut files = Vec::new();
    let mut total = 0u64;
    for session in std::fs::read_dir(cache).into_iter().flatten().flatten() {
        for file in std::fs::read_dir(session.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            let Ok(meta) = file.metadata() else { continue };
            total += meta.len();
            if file.path() != just_kept {
                files.push((
                    meta.modified().unwrap_or(UNIX_EPOCH),
                    meta.len(),
                    file.path(),
                ));
            }
        }
    }
    files.sort();
    for (_, len, path) in files {
        if total <= cap {
            break;
        }
        if std::fs::remove_file(path).is_ok() {
            total = total.saturating_sub(len);
        }
    }
}

/// Drop one closed pane's images, unless another pane shows the session.
/// They come back from the transcript on a click.
pub(super) fn forget(session: &PaneSession, shown_elsewhere: impl FnOnce() -> bool) {
    let Some(id) = safe_component(&session.id) else {
        return;
    };
    let dir = cache_dir().join(id);
    if dir.is_dir() && !shown_elsewhere() {
        std::thread::spawn(move || {
            if let Err(error) = std::fs::remove_dir_all(&dir) {
                crate::mux_log!(error; "fno mux: cannot remove {}: {error}", dir.display());
            }
        });
    }
}

/// At server start, drop the images of sessions with no live row. An
/// unreadable registry proves nothing dead, so it removes nothing.
pub(super) fn sweep_dead_sessions() {
    let cache = cache_dir();
    if !cache.is_dir() {
        return;
    }
    let Some(rows) = crate::restore_gate::restore_registry_rows() else {
        return;
    };
    for entry in std::fs::read_dir(&cache).into_iter().flatten().flatten() {
        let name = entry.file_name();
        let Some(id) = name.to_str() else { continue };
        let live = rows.iter().any(|row| !row.exited && row.keyed_by(id));
        if !live {
            if let Err(error) = std::fs::remove_dir_all(entry.path()) {
                crate::mux_log!(error; "fno mux: cannot sweep image cache {id}: {error}");
            }
        }
    }
}
