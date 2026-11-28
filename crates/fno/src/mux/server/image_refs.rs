//! Which image, if any, does a pane cell name?
//!
//! The one resolver behind a click and a hover. [`find`] reads only the text
//! around the cell ([`LinkWindow`]) and an injected probe, so the core loop
//! does a stat and never a read. [`resolve`] reads bytes and runs off the loop.
//!
//! Pane output chooses these paths, so it is a trust boundary: a reference
//! opens only when both the resolved file's extension and its first bytes
//! name an image, and nothing here ever reaches the platform opener.

use std::path::{Path, PathBuf};

use crate::server::pasted_images::{self, PaneSession, Stores};
use crate::vt::{LinkWindow, TextLine};

const EXTS: [&str; 5] = ["png", "jpg", "jpeg", "gif", "webp"];
/// How many words a path with spaces may hold.
const MAX_WORDS: usize = 8;
/// Rows a hard-wrapped path may continue onto.
const MAX_JOINED: usize = 2;
/// Filesystem probes one lookup may spend.
const MAX_PROBES: usize = 96;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ImageSource {
    /// An existing regular file with an image extension, canonical.
    File(PathBuf),
    /// A harness's `[Image #N]` (OpenCode writes `[Image N]`).
    Pasted(Placeholder),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Placeholder {
    pub(crate) number: u64,
    /// The text around the placeholder on its line: the key to its turn.
    pub(crate) row_text: String,
    /// The line sits at the cursor, so it is the message being typed.
    pub(crate) composing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImageRef {
    pub(crate) source: ImageSource,
    /// Every visible pane-local cell of the reference.
    pub(crate) cells: Vec<(u16, u16)>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ImageError {
    #[error("not a readable image: {name}")]
    NotAnImage { name: String },
    #[error("image #{number} is no longer stored")]
    NoLongerStored { number: u64 },
    #[error("image #{number} opens after you send the message")]
    NotSent { number: u64 },
    #[error("image #{number} matches no one message, so nothing opens")]
    NoMatch { number: u64 },
    #[error("cannot read the transcript: {0}")]
    Transcript(#[from] crate::chat_events::ChatReadError),
    #[error("cannot keep the pasted image: {0}")]
    Cache(#[from] std::io::Error),
}

/// Whether `path` carries one of the extensions the viewer decodes.
pub(crate) fn has_image_ext(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| EXTS.iter().any(|known| ext.eq_ignore_ascii_case(known)))
}

/// The canonical path of an existing regular image-named file. One stat and
/// no open, so a FIFO or a device cannot park the core loop.
pub(crate) fn probe(path: &Path) -> Option<PathBuf> {
    // A miss is the common answer: most candidates are prose.
    let real = std::fs::canonicalize(path).ok()?;
    let regular = std::fs::metadata(&real).is_ok_and(|meta| meta.is_file());
    let linkable = crate::link::is_file_uri(&crate::link::file_uri(&real, None));
    // The resolved name is judged, so `a.png -> x.command` never links.
    (regular && linkable && has_image_ext(&real)).then_some(real)
}

/// [`find`] against this machine's home, host name and filesystem.
pub(crate) fn find_here(window: &LinkWindow, cwd: &str) -> Option<ImageRef> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let host = crate::agents_view::hostname();
    find(window, Path::new(cwd), home.as_deref(), &host, probe)
}

/// The image reference under the window's cell. A placeholder outranks the
/// link around it: Claude links `[Image #N]` to a file a later paste replaces.
/// A hyperlink outranks the text it covers, so a non-image link never falls
/// through to the path its label happens to spell.
pub(crate) fn find(
    window: &LinkWindow,
    cwd: &Path,
    home: Option<&Path>,
    host: &str,
    probe: impl Fn(&Path) -> Option<PathBuf>,
) -> Option<ImageRef> {
    let line = window.lines.get(window.line)?;
    if let Some(found) = find_placeholder(line, window.at, window.composing) {
        return Some(found);
    }
    if let Some(link) = &window.hyperlink {
        let path = probe(&file_url_path(&link.uri, host)?)?;
        return Some(ImageRef {
            source: ImageSource::File(path),
            cells: link.cells.clone(),
        });
    }
    find_path(window, cwd, home, &probe)
}

/// The local path a `file:` URL names, percent-decoded.
fn file_url_path(uri: &str, host: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let (named, path) = rest.split_at(rest.find('/')?);
    let short = host.split('.').next().unwrap_or(host);
    let local = named.is_empty()
        || named.eq_ignore_ascii_case("localhost")
        || (!host.is_empty()
            && (named.eq_ignore_ascii_case(host) || named.eq_ignore_ascii_case(short)));
    if !local {
        return None;
    }
    let path = path.split(['#', '?']).next()?;
    let path = crate::theme_import::percent_decode_component(path)?;
    (!path.chars().any(char::is_control)).then(|| PathBuf::from(path))
}

/// The `[Image #N]` or `[Image N]` covering char `at`.
fn find_placeholder(line: &TextLine, at: usize, composing: bool) -> Option<ImageRef> {
    let open = line.chars.get(..=at)?.iter().rposition(|c| *c == '[')?;
    let len = line.chars.get(open..)?.iter().position(|c| *c == ']')? + 1;
    let end = open + len;
    if at >= end {
        return None;
    }
    let marker: String = line.chars.get(open..end)?.iter().collect();
    Some(ImageRef {
        source: ImageSource::Pasted(Placeholder {
            number: placeholder_number(&marker)?,
            row_text: segment(&line.chars, open, end),
            composing,
        }),
        cells: line
            .cells
            .get(open..end)?
            .iter()
            .copied()
            .flatten()
            .collect(),
    })
}

/// The number a whole `[Image #N]` or `[Image N]` carries.
pub(crate) fn placeholder_number(marker: &str) -> Option<u64> {
    let inner = marker.strip_prefix("[Image ")?.strip_suffix(']')?;
    let digits = inner.strip_prefix('#').unwrap_or(inner);
    let plain = (1..=6).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit());
    // The shape check leaves nothing for the parse to refuse.
    plain.then(|| digits.parse().ok()).flatten()
}

/// The run around `start..end` bounded by borders and by three-space gaps,
/// so a side column or a timestamp does not join the turn key.
fn segment(chars: &[char], start: usize, end: usize) -> String {
    let breaks = |i: usize| {
        chars.get(i) == Some(&'│')
            || chars
                .get(i..i + 3)
                .is_some_and(|gap| gap.iter().all(|c| *c == ' '))
    };
    let from = (0..start).rev().find(|&i| breaks(i)).map_or(0, |i| i + 1);
    let to = (end..chars.len())
        .find(|&i| breaks(i))
        .unwrap_or(chars.len());
    chars.get(from..to).unwrap_or_default().iter().collect()
}

/// A run of text joined from the window's lines, and where each char came
/// from as `(line, char)`.
#[derive(Default)]
struct Joined {
    chars: Vec<char>,
    from: Vec<Option<(usize, usize)>>,
}

impl Joined {
    /// Lines `first..=last`, each continuation without its indent or border.
    fn new(lines: &[TextLine], first: usize, last: usize, gap: Option<char>) -> Self {
        let mut out = Self::default();
        for (n, line) in lines.iter().enumerate().take(last + 1).skip(first) {
            let margin = |c: &char| c.is_whitespace() || *c == '│';
            let mut start = 0;
            if n > first {
                start = line.chars.iter().take_while(|c| margin(c)).count();
                if let Some(gap) = gap {
                    out.chars.push(gap);
                    out.from.push(None);
                }
            }
            let tail = line.chars.iter().rev().take_while(|c| margin(c)).count();
            let end = line.chars.len() - tail;
            for (i, c) in line.chars.iter().enumerate().take(end).skip(start) {
                out.chars.push(*c);
                out.from.push(Some((n, i)));
            }
        }
        out
    }

    fn line_of(&self, i: usize) -> Option<usize> {
        self.from.get(i).copied().flatten().map(|(line, _)| line)
    }

    /// Ends of every `.<image extension>` that closes a word.
    fn ext_ends(&self) -> Vec<usize> {
        let mut out = Vec::new();
        for dot in (0..self.chars.len()).filter(|&i| self.chars.get(i) == Some(&'.')) {
            for ext in EXTS {
                let end = dot + 1 + ext.len();
                let Some(word) = self.chars.get(dot + 1..end) else {
                    continue;
                };
                let named = word
                    .iter()
                    .zip(ext.chars())
                    .all(|(c, e)| c.eq_ignore_ascii_case(&e));
                let closes = match (self.chars.get(end), self.chars.get(end + 1)) {
                    (Some('.'), Some(next)) => !next.is_alphanumeric(),
                    (Some(next), _) => !(next.is_alphanumeric() || matches!(next, '_' | '-' | '/')),
                    (None, _) => true,
                };
                if named && closes {
                    out.push(end);
                }
            }
        }
        out
    }

    /// Where a path ending at `end` can start, nearest first.
    fn starts(&self, end: usize) -> Vec<usize> {
        let wraps = |c: char| c.is_whitespace() || "<>()[]{}\"'`*|,;:=@│".contains(c);
        let mut out = Vec::new();
        let mut words = 1;
        for i in (0..end).rev() {
            let Some(&c) = self.chars.get(i) else { break };
            let before = i.checked_sub(1).and_then(|p| self.chars.get(p)).copied();
            if c.is_whitespace() {
                continue;
            }
            if before.is_none_or(wraps) {
                out.push(i);
                if before.is_some_and(char::is_whitespace) {
                    words += 1;
                    if words > MAX_WORDS {
                        break;
                    }
                }
            }
        }
        out
    }
}

/// The longest existing image path that covers the window's cell, over the
/// soft-wrapped line and the hard wraps that can touch it.
fn find_path(
    window: &LinkWindow,
    cwd: &Path,
    home: Option<&Path>,
    probe: &impl Fn(&Path) -> Option<PathBuf>,
) -> Option<ImageRef> {
    let lines = &window.lines;
    let here = Some((window.line, window.at));
    let mut probes = 0;
    let mut best: Option<(usize, PathBuf, Joined, usize, usize)> = None;
    // Narrowest first, so the joins never spend the probes a plain line needs.
    for extra in 0..=MAX_JOINED {
        for back in 0..=extra {
            let Some(first) = window.line.checked_sub(back) else {
                continue;
            };
            let last = first + extra;
            if last >= lines.len() {
                continue;
            }
            let wraps = lines.get(first..last)?.iter().all(|line| line.reaches_edge);
            if !wraps {
                continue;
            }
            let gaps: &[Option<char>] = if first == last {
                &[None]
            } else {
                &[None, Some(' ')]
            };
            for &gap in gaps {
                let text = Joined::new(lines, first, last, gap);
                // The pointer is on an indent this join dropped.
                let Some(pointed) = text.from.iter().position(|from| *from == here) else {
                    continue;
                };
                let mut found: Option<(usize, PathBuf, usize, usize)> = None;
                for end in text.ext_ends() {
                    if end <= pointed || text.line_of(end - 1) != Some(last) {
                        continue;
                    }
                    for start in text.starts(end) {
                        let covers = start <= pointed && text.line_of(start) == Some(first);
                        if !covers || probes >= MAX_PROBES {
                            continue;
                        }
                        let raw: String = text.chars.get(start..end)?.iter().collect();
                        let Some(path) = expand(&raw, cwd, home) else {
                            continue;
                        };
                        probes += 1;
                        if found.as_ref().is_none_or(|f| end - start > f.0) {
                            if let Some(real) = probe(&path) {
                                found = Some((end - start, real, start, end));
                            }
                        }
                    }
                }
                if let Some((len, path, start, end)) = found {
                    if best.as_ref().is_none_or(|b| len > b.0) {
                        best = Some((len, path, text, start, end));
                    }
                }
            }
        }
    }
    let (_, path, text, start, end) = best?;
    let cells = text
        .from
        .get(start..end)?
        .iter()
        .flatten()
        .filter_map(|&(line, at)| *lines.get(line)?.cells.get(at)?)
        .collect();
    Some(ImageRef {
        source: ImageSource::File(path),
        cells,
    })
}

fn expand(raw: &str, cwd: &Path, home: Option<&Path>) -> Option<PathBuf> {
    if raw.len() > crate::link::MAX_URL_LEN || raw.chars().any(char::is_control) {
        return None;
    }
    if raw.starts_with("~/") {
        let home = home.map(Path::as_os_str);
        return Some(PathBuf::from(crate::connections_view::expand_tilde(
            raw, home,
        )));
    }
    Some(cwd.join(raw))
}

/// The file a click opens. Blocking: call it off the core loop.
pub(crate) fn resolve(
    source: &ImageSource,
    session: Option<&PaneSession>,
    stores: &Stores,
) -> Result<PathBuf, ImageError> {
    let path = match (source, session) {
        (ImageSource::File(path), _) => path.clone(),
        (ImageSource::Pasted(paste), Some(session)) => {
            pasted_images::resolve(paste, session, stores)?
        }
        (ImageSource::Pasted(paste), None) => {
            return Err(ImageError::NoMatch {
                number: paste.number,
            })
        }
    };
    verify(&path)?;
    Ok(path)
}

/// Extension and magic bytes must both name an image.
pub(crate) fn verify(path: &Path) -> Result<(), ImageError> {
    if has_image_ext(path) && has_image_magic(path) {
        return Ok(());
    }
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    Err(ImageError::NotAnImage {
        name: name.chars().filter(|c| !c.is_control()).collect(),
    })
}

fn has_image_magic(path: &Path) -> bool {
    use image::ImageFormat as F;
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    // Nonblocking, so a FIFO swapped in after the probe cannot park the read.
    let Ok(mut file) = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
    else {
        return false;
    };
    if !file.metadata().is_ok_and(|meta| meta.is_file()) {
        return false;
    }
    let mut head = [0u8; 16];
    let Ok(n) = file.read(&mut head) else {
        return false;
    };
    matches!(
        image::guess_format(head.get(..n).unwrap_or_default()),
        Ok(F::Png | F::Jpeg | F::Gif | F::WebP)
    )
}

#[cfg(test)]
mod tests;
