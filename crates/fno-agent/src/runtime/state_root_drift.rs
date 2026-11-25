//! The state-root drift reading: top-level entries of the fno state root that
//! no keep row of the layout table (`docs/state-root-layout.tsv`) carries. The
//! table is compiled in, so the reading needs no staged doc and reads the same
//! set the write gate and `fno doctor lint state-roots` read. It moves
//! nothing: the daily reclaim lane and the lead check-in print it.

use std::path::Path;

/// `*` is the only wildcard a table row can carry.
pub(crate) fn glob_match(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let Some(mut rest) = name.strip_prefix(parts[0]) else {
        return false;
    };
    let last = parts[parts.len() - 1];
    for mid in &parts[1..parts.len() - 1] {
        let Some(at) = rest.find(mid) else {
            return false;
        };
        rest = &rest[at + mid.len()..];
    }
    rest.ends_with(last)
}

pub struct DriftReport {
    pub count: usize,
    /// The names no keep row carries: first the ones no row carries at all
    /// (fno never declared them), then the ones a row still owes a move or a
    /// park. Each group is sorted.
    pub entries: Vec<String>,
}

/// The live reading. A failure is a REFUSAL, never a silent zero: an
/// unreadable root makes the caller print a failed reading.
pub fn drift_report(state_root: &Path) -> Result<DriftReport, String> {
    let names: Vec<String> = std::fs::read_dir(state_root)
        .map_err(|e| format!("read {}: {e}", state_root.display()))?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    Ok(report_of(names))
}

fn report_of(names: Vec<String>) -> DriftReport {
    let (mut unknown, mut owed): (Vec<String>, Vec<String>) = names
        .into_iter()
        .filter(|n| !crate::state_layout::is_kept(n))
        .partition(|n| !crate::state_layout::is_carried(n));
    unknown.sort();
    owed.sort();
    unknown.extend(owed);
    DriftReport {
        count: unknown.len(),
        entries: unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reading_names_what_no_keep_row_carries_unknown_first() {
        let names = ["db", "config.toml.bak-1", "bus", "stray.out", "agents"]
            .map(String::from)
            .to_vec();
        let rep = report_of(names);
        assert_eq!(
            rep.entries,
            vec!["stray.out".to_string(), "bus".to_string()]
        );
        assert_eq!(rep.count, 2);
    }

    #[test]
    fn a_failed_root_read_is_a_refusal_not_a_zero() {
        let file = std::env::temp_dir().join(format!("fno-drift-file-{}", std::process::id()));
        let _ = std::fs::remove_file(&file);
        std::fs::write(&file, b"x").unwrap();
        // A root under a regular FILE is unreadable: read_dir refuses, so the
        // reading is a refusal, never a silent zero.
        let report = drift_report(&file.join("root"));
        let _ = std::fs::remove_file(&file);
        assert!(report.is_err());
    }
}
