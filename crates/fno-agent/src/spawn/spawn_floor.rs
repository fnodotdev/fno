//! A per-command model floor: `agents.profiles.<command>.min_band` names the
//! lowest band a spawn of that command may run on. Compose calls `check` with
//! the model the spawn resolved, typed pins included, and a spawn below the
//! floor refuses before any worker launches. A profile with no key never
//! reaches this module's reads.

use serde_json::{Map, Value};

use crate::route_slot::{band_rank, row_value};

const BANDS: &str = "low|medium|high|max";

pub(crate) struct Ask<'a> {
    pub command: &'a str,
    pub floor: &'a toml::Value,
    pub route: Option<&'a str>,
    pub model: Option<&'a str>,
    pub vendor: Option<&'a str>,
    pub rows: &'a Value,
    pub tiers: &'a [(String, Vec<String>)],
    pub reach: &'a Map<String, Value>,
}

/// The `--model` a harness token after the `--` fence names, if any.
pub(crate) fn fenced_model(argv: &[String]) -> Option<String> {
    let tail = argv.get(argv.iter().position(|t| t == "--")? + 1..)?;
    tail.iter().enumerate().find_map(|(i, t)| {
        if let Some(v) = t.strip_prefix("--model=") {
            return Some(v.to_string());
        }
        (t == "--model" || t == "-m")
            .then(|| tail.get(i + 1).cloned())
            .flatten()
    })
}

/// Permit, or the one refusal line.
pub(crate) fn check(ask: &Ask) -> Option<String> {
    let key = format!("agents.profiles.{}.min_band", ask.command);
    let refuse = |why: String| format!("fno agent spawn: {why}; refusing; no worker launched");
    let floor = ask.floor.as_str().map(str::trim).unwrap_or("");
    if band_rank(floor) < 0 {
        return Some(refuse(format!(
            "{key} must be one of {BANDS}, got {}",
            ask.floor
        )));
    }
    let (subject, found) = match (ask.route, ask.model, ask.vendor) {
        (Some(route), model, _) => {
            let rows = rows_where(ask.rows, |r| row_value(r, "route") == route);
            let tail = route.split_once('/').map(|(_, m)| m).or(model);
            (route, band_of(ask, rows, tail))
        }
        (None, Some(model), _) => {
            let rows = rows_where(ask.rows, |r| row_value(r, "model") == model);
            let by_name = rows_where(ask.rows, |r| row_value(r, "name") == model);
            (model, band_of(ask, [rows, by_name].concat(), Some(model)))
        }
        (None, None, Some(vendor)) => {
            let prefix = format!("{vendor}/");
            let rows = rows_where(ask.rows, |r| row_value(r, "route").starts_with(&prefix));
            (vendor, band_of(ask, rows, None))
        }
        (None, None, None) => {
            return Some(refuse(format!(
                "{key} is {floor} but this spawn resolves no model, and a harness \
                 default has no band; name one with --route, a lane or a profile model"
            )))
        }
    };
    match found {
        Some((band, _)) if band_rank(&band) >= band_rank(floor) => None,
        Some((band, who)) => Some(refuse(format!(
            "{} may not run on {subject} ({who}, band {band}): {key} is {floor}",
            ask.command
        ))),
        None => Some(refuse(format!(
            "{} may not run on {subject}: it matches no routing.models row or \
             built-in tier, so its band is unknown and {key} is {floor}; declare \
             a routing.models row with a band",
            ask.command
        ))),
    }
}

fn rows_where(rows: &Value, keep: impl Fn(&Value) -> bool) -> Vec<(String, String)> {
    rows.as_object()
        .map(|all| {
            all.iter()
                .filter(|(_, row)| keep(row))
                .map(|(name, row)| (name.clone(), row_value(row, "band")))
                .collect()
        })
        .unwrap_or_default()
}

/// The weakest band among the matched rows, else among the built-in tiers
/// naming the model. An unbanded row ranks below every band.
fn band_of(
    ask: &Ask,
    rows: Vec<(String, String)>,
    model: Option<&str>,
) -> Option<(String, String)> {
    let weakest = |pairs: Vec<(String, String)>| {
        pairs
            .into_iter()
            .min_by_key(|(_, band)| band_rank(band))
            .map(|(who, band)| {
                (
                    if band.is_empty() {
                        "unbanded".into()
                    } else {
                        band
                    },
                    who,
                )
            })
    };
    if !rows.is_empty() {
        return weakest(rows);
    }
    let model = model?;
    let tiered = ask
        .tiers
        .iter()
        .flat_map(|(band, names)| names.iter().map(move |n| (band, n)))
        .filter(|(_, name)| {
            model == name.as_str()
                || model.starts_with(name.as_str())
                || name.contains(model)
                || ask
                    .reach
                    .get(name.as_str())
                    .and_then(|pair| pair.get(1))
                    .and_then(Value::as_str)
                    == Some(model)
        })
        .map(|(band, name)| (format!("tier {name}"), band.clone()))
        .collect();
    weakest(tiered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rows() -> Value {
        json!({
            "claude-opus-5-5": {"model": "claude-opus-5-5", "band": "high"},
            "zai-flash": {"model": "glm-5.3-flash[1m]", "route": "zai/glm-5.3-flash[1m]", "band": "medium"},
        })
    }

    fn verdict(floor: &str, route: Option<&str>, model: Option<&str>) -> Option<String> {
        let floor = toml::Value::String(floor.to_string());
        let (tiers, reach) = crate::route_gather::load_model_tiers();
        check(&Ask {
            command: "blueprint",
            floor: &floor,
            route,
            model,
            vendor: None,
            rows: &rows(),
            tiers: &tiers,
            reach: &reach,
        })
    }

    #[test]
    fn a_typed_route_below_the_floor_refuses_with_the_named_line() {
        let line = verdict("high", Some("zai/glm-5.3-flash[1m]"), None).unwrap();
        assert!(line.contains("blueprint may not run on zai/glm-5.3-flash[1m]"));
        assert!(line.contains("zai-flash, band medium"));
        assert!(line.contains("agents.profiles.blueprint.min_band is high"));
        assert!(line.ends_with("no worker launched"));
    }

    #[test]
    fn a_model_at_or_above_the_floor_permits() {
        assert!(verdict("high", None, Some("claude-opus-5-5")).is_none());
        assert!(verdict("medium", Some("zai/glm-5.3-flash[1m]"), None).is_none());
        assert!(verdict("high", None, Some("claude-opus-5-9")).is_none());
        assert!(verdict("high", None, Some("opus")).is_none());
        assert!(verdict("high", None, Some("haiku")).is_some());
    }

    #[test]
    fn an_unbandable_model_and_a_bad_key_both_refuse() {
        let unknown = verdict("low", None, Some("mystery-1")).unwrap();
        assert!(unknown.contains("declare a routing.models row with a band"));
        let bad = verdict("huge", None, Some("claude-opus-5-5")).unwrap();
        assert!(bad.contains("low|medium|high|max"));
    }
}
