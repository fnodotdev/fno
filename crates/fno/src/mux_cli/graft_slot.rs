//! The graft `--slot` value parser, moved out of `mux_cli` under the
//! file-budget gate with the `mod:` form it grew.

use crate::proto::LayoutBinding;

/// A graft `--slot` binding: `anchor` -> Anchor, `fno:<id>` -> Fno, `-`/`shell`
/// -> Shell, `mod:<pack>/<mod>` -> Mod.
pub(super) fn parse_graft_slot(s: &str) -> Result<LayoutBinding, String> {
    const FORMS: &str = "anchor|fno:<id>|mod:<pack>/<mod>|- (shell)";
    match s {
        "anchor" | "a" => Ok(LayoutBinding::Anchor),
        "-" | "shell" => Ok(LayoutBinding::Shell),
        other if other.starts_with("fno:") => Ok(LayoutBinding::Fno(other[4..].to_string())),
        other => match other.strip_prefix("mod:") {
            Some(t) if crate::server::mod_target_ok(t) => Ok(LayoutBinding::Mod(t.to_string())),
            Some(t) => Err(format!(
                "graft --slot mod: wants <pack>/<mod>, got {t:?}; accepted: {FORMS}"
            )),
            None => Err(format!("graft --slot must be {FORMS}, got {other:?}")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graft_slot_takes_a_mod_target() {
        assert_eq!(
            parse_graft_slot("mod:fno-panels/review-ledger"),
            Ok(LayoutBinding::Mod("fno-panels/review-ledger".into()))
        );
    }

    #[test]
    fn graft_slot_refuses_an_empty_mod_target_naming_the_forms() {
        let err = parse_graft_slot("mod:").unwrap_err();
        assert!(
            err.contains("anchor|fno:<id>|mod:<pack>/<mod>|- (shell)"),
            "{err}"
        );
    }
}
