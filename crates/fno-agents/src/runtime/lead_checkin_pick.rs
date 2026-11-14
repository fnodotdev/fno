//! The check-in's pick: the one action the beat offers the lead to start,
//! and the journal keys that say whether the next beat found it taken.
//! The beat never decides; it names a command. A beat that offered a pick
//! and finds it still offered, with no `--pick` answer, reads `reactive`.
use serde_json::{json, Map, Value};

fn ids(bp: &Value, key: &str) -> Vec<String> {
    bp.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

fn target(id: &str) -> String {
    format!("fno agents spawn \"/fno:target {id}\"")
}

/// Every node id the beat still names as startable, in offer order, with
/// the command that starts it: a blueprint start, then target-ready, then
/// plan-ready, then a batch's contain command. An idle child offers nothing:
/// its owner is usually the lead's own epic.
fn offers(bp: &Value) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for id in ids(bp, "starts") {
        out.push((id.clone(), format!("start /fno:blueprint subagent {id}")));
    }
    for key in ["target_ready", "plan_ready"] {
        for id in ids(bp, key) {
            out.push((id.clone(), target(&id)));
        }
    }
    for g in bp
        .get("batch")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let (Some(owner), Some(command)) = (
            g.get("owner").and_then(Value::as_str),
            g.get("command").and_then(Value::as_str),
        ) {
            out.push((owner.to_string(), command.to_string()));
        }
    }
    out
}

/// Add the pick keys to the beat's data: `pick_offered` (the command or
/// null), `pick_node`, `pick` (the lead's `--pick` text or null), `pick_line`
/// (the rendered line), and `reactive` (only when the previous beat offered
/// a pick: true when it is still offered and no `--pick` answered it). With
/// no blueprint reading the beat measured nothing, so no key claims it.
pub(crate) fn annotate(
    data: &mut Map<String, Value>,
    bp: Option<&Value>,
    previous: Option<&Value>,
    pick: Option<&str>,
) {
    let Some(bp) = bp else {
        return;
    };
    let now = offers(bp);
    let answer = pick.map(str::trim).filter(|t| !t.is_empty());
    let last = previous.and_then(|p| {
        let node = p.get("pick_node").and_then(Value::as_str)?;
        let command = p.get("pick_offered").and_then(Value::as_str)?;
        Some((node.to_string(), command.to_string()))
    });
    let reactive = last
        .as_ref()
        .map(|(node, _)| answer.is_none() && now.iter().any(|(id, _)| id == node));
    let first = now.first();
    let mut line = match first {
        Some((_, command)) => {
            format!("pick: {command} (answer with --pick \"<what you started>\")")
        }
        None => {
            let why = bp
                .get("skips")
                .and_then(Value::as_array)
                .and_then(|s| s.first())
                .and_then(|s| s.get("reason").and_then(Value::as_str))
                .unwrap_or("nothing startable in scope");
            format!("pick: none offered ({why})")
        }
    };
    if let (Some(true), Some((_, command))) = (reactive, last.as_ref()) {
        line = format!("pick: the last offer ({command}) went untaken. {line}");
    }
    data.insert("pick_offered".into(), json!(first.map(|(_, c)| c)));
    data.insert("pick_node".into(), json!(first.map(|(n, _)| n)));
    data.insert("pick".into(), json!(answer));
    data.insert("pick_line".into(), json!(line));
    if let Some(reactive) = reactive {
        data.insert("reactive".into(), json!(reactive));
    }
}

/// The beat's `pick:` line, empty when the beat measured no blueprint reading.
pub(crate) fn lines(data: &Map<String, Value>) -> Vec<String> {
    data.get("pick_line")
        .and_then(Value::as_str)
        .map(|l| vec![l.to_string()])
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bp(target_ready: &[&str]) -> Value {
        json!({"starts": [], "target_ready": target_ready, "plan_ready": [], "skips": []})
    }

    #[test]
    fn an_offer_is_reactive_until_taken_or_answered() {
        let mut data = Map::new();
        annotate(&mut data, Some(&bp(&["x-aaaa"])), None, None);
        assert_eq!(
            data["pick_offered"],
            "fno agents spawn \"/fno:target x-aaaa\""
        );
        assert!(!data.contains_key("reactive"));

        let prior = json!({"pick_offered": "c", "pick_node": "x-aaaa"});
        let mut open = Map::new();
        annotate(&mut open, Some(&bp(&["x-aaaa"])), Some(&prior), None);
        assert_eq!(open["reactive"], true);
        assert!(lines(&open)[0].contains("went untaken"));

        let mut gone = Map::new();
        annotate(&mut gone, Some(&bp(&["x-bbbb"])), Some(&prior), None);
        assert_eq!(gone["reactive"], false);
        let mut answered = Map::new();
        annotate(
            &mut answered,
            Some(&bp(&["x-aaaa"])),
            Some(&prior),
            Some("planned it"),
        );
        assert_eq!(answered["reactive"], false);
        assert_eq!(answered["pick"], "planned it");
    }

    #[test]
    fn no_offer_names_the_reason_and_an_unread_reading_names_nothing() {
        let mut data = Map::new();
        let empty = json!({"starts": [], "target_ready": [], "plan_ready": [],
            "skips": [{"id": null, "reason": "no unplanned node in scope"}]});
        annotate(&mut data, Some(&empty), None, None);
        assert_eq!(data["pick_offered"], Value::Null);
        assert_eq!(
            lines(&data)[0],
            "pick: none offered (no unplanned node in scope)"
        );
        let mut unread = Map::new();
        annotate(&mut unread, None, None, None);
        assert!(unread.is_empty());
    }
}
