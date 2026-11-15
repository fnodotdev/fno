//! The one-release env fallback for the `fno-agents` -> `fno-agent` rename.
//! Every binary calls [`adopt`] first in `main`, before it starts a thread:
//! each `FNO_AGENTS_<X>` the caller set becomes `FNO_AGENT_<X>` unless the new
//! name is already set. Every later read, and every child process, then sees
//! the new name. A worker spawned before the upgrade keeps its old env and
//! still resolves the same home and binaries.

const OLD: &str = "FNO_AGENTS_";
const NEW: &str = "FNO_AGENT_";

/// The (new name, value) pairs to set: one per old-prefixed var whose new
/// name is absent from `vars`.
pub fn renames(vars: &[(String, String)]) -> Vec<(String, String)> {
    vars.iter()
        .filter_map(|(key, value)| {
            let new = format!("{NEW}{}", key.strip_prefix(OLD)?);
            (!vars.iter().any(|(k, _)| *k == new)).then(|| (new, value.clone()))
        })
        .collect()
}

/// Copy each old-prefixed var to its new name in this process's env.
pub fn adopt() {
    let vars: Vec<(String, String)> = std::env::vars().collect();
    for (key, value) in renames(&vars) {
        std::env::set_var(key, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
        items
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn old_prefix_fills_only_an_absent_new_name() {
        let got = renames(&pairs(&[
            ("FNO_AGENTS_HOME", "/old"),
            ("FNO_AGENTS_BIN", "/old/bin"),
            ("FNO_AGENT_BIN", "/new/bin"),
            ("FNO_AGENT_SELF", "w"),
        ]));
        assert_eq!(got, pairs(&[("FNO_AGENT_HOME", "/old")]));
    }
}
