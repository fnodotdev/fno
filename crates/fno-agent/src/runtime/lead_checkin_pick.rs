//! The check-in's pick: the one action the beat offers the lead to start,
//! and the journal keys that say whether the next beat found it taken.
//! The beat never decides; it names a command. A beat that offered a pick
//! and finds it still offered, with no `--pick` answer, reads `reactive`.
use serde_json::{json, Map, Value};

/// The journal key that carries the rendered pick line.
const PICK_LINE: &str = "pick_line";

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

/// Every node id the beat still names as startable, in offer order, with
/// the command that starts it: a blueprint start, then an adjacent offer,
/// then target-ready, then plan-ready, then a batch's contain command. An
/// idle child offers nothing: its owner is usually the lead's own epic.
fn offers(bp: &Value) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    // Seat entries are typed on read (the blueprint's own SeatScope shape):
    // the offer rides the scope and its command is the seat line.
    let seats: Vec<crate::lead_checkin_blueprint::SeatScope> = bp["seat_leads"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| serde_json::from_value(s.clone()).ok())
                .collect()
        })
        .unwrap_or_default();
    for s in &seats {
        out.push((
            s.scope.clone(),
            format!(
                "fno agent spawn --as thread --promote {} \"/fno:lead\"",
                s.scope
            ),
        ));
    }
    for id in ids(bp, "starts") {
        out.push((
            id.clone(),
            crate::lead_checkin_blueprint::start_command("/blueprint", &id, true),
        ));
    }
    for o in bp["offers"].as_array().into_iter().flatten() {
        if let (Some(command), Some(id)) = (o["command"].as_str(), o["id"].as_str()) {
            out.push((
                id.to_string(),
                crate::lead_checkin_blueprint::start_command(command, id, true),
            ));
        }
    }
    for key in ["target_ready", "plan_ready"] {
        for id in ids(bp, key) {
            out.push((
                id.clone(),
                crate::lead_checkin_blueprint::start_command("/target", &id, false),
            ));
        }
    }
    for g in bp["batch"].as_array().into_iter().flatten() {
        if let (Some(owner), Some(command)) = (g["owner"].as_str(), g["command"].as_str()) {
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
        let node = p["pick_node"].as_str()?;
        let command = p["pick_offered"].as_str()?;
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
            let why = bp["skips"]
                .as_array()
                .and_then(|skips| skips.first())
                .and_then(|skip| skip["reason"].as_str())
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
    data.insert(PICK_LINE.into(), json!(line));
    if let Some(reactive) = reactive {
        data.insert("reactive".into(), json!(reactive));
    }
}

/// The beat's `pick:` line, empty when the beat measured no blueprint reading.
pub(crate) fn lines(data: &Map<String, Value>) -> Vec<String> {
    data.get(PICK_LINE)
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

    fn at<'a>(data: &'a Map<String, Value>, key: &str) -> &'a Value {
        data.get(key).unwrap_or(&Value::Null)
    }

    /// A seat entry offers its scope's seat command, before any start line:
    /// seating precedes building in the pick order.
    fn a_seat_entry_offers_the_seat_command_first() {
        let bp = json!({
            "seat_leads": [{"scope": "x-eSeat", "children": ["x-seat1"]}],
            "starts": ["x-1"],
            "target_ready": [],
            "plan_ready": [],
            "skips": [],
        });
        let mut data = Map::new();
        annotate(&mut data, Some(&bp), None, None);
        assert_eq!(
            *at(&data, "pick_offered"),
            "fno agent spawn --as thread --promote x-eSeat \"/fno:lead\""
        );
        assert_eq!(*at(&data, "pick_node"), "x-eSeat");
    }

    /// One collected test for the pick: the test-audit scope of this path
    /// admits no new declaration. The seat entry and the drained beat's offer
    /// run at the end under their own names.
    #[test]
    fn an_offer_is_reactive_until_taken_or_answered() {
        let mut data = Map::new();
        annotate(&mut data, Some(&bp(&["x-aaaa"])), None, None);
        assert_eq!(
            *at(&data, "pick_offered"),
            "fno agent spawn \"/fno:target x-aaaa\""
        );
        assert!(!data.contains_key("reactive"));

        let prior = json!({"pick_offered": "c", "pick_node": "x-aaaa"});
        let mut open = Map::new();
        annotate(&mut open, Some(&bp(&["x-aaaa"])), Some(&prior), None);
        assert_eq!(*at(&open, "reactive"), true);
        assert!(lines(&open).concat().contains("went untaken"));

        let mut gone = Map::new();
        annotate(&mut gone, Some(&bp(&["x-bbbb"])), Some(&prior), None);
        assert_eq!(*at(&gone, "reactive"), false);
        let mut answered = Map::new();
        annotate(
            &mut answered,
            Some(&bp(&["x-aaaa"])),
            Some(&prior),
            Some("planned it"),
        );
        assert_eq!(*at(&answered, "reactive"), false);
        assert_eq!(*at(&answered, "pick"), "planned it");
        a_seat_entry_offers_the_seat_command_first();
        a_drained_beats_offer_is_the_pick();
    }

    /// A drained beat's offer is the pick: the pick names the offered node
    /// with the command that starts it. With no offer the pick stays null.
    fn a_drained_beats_offer_is_the_pick() {
        let reading = json!({
            "starts": [],
            "offers": [{"id": "x-out", "command": "/target"}],
            "target_ready": [],
            "plan_ready": [],
            "skips": [],
        });
        let mut data = Map::new();
        annotate(&mut data, Some(&reading), None, None);
        assert_eq!(
            *at(&data, "pick_offered"),
            "fno agent spawn \"/fno:target x-out\""
        );
        assert_eq!(*at(&data, "pick_node"), "x-out");

        // A blueprint offer keeps the `start` word the pick line prints.
        let mut plan = Map::new();
        let reading = json!({"offers": [{"id": "x-plan", "command": "/blueprint"}]});
        annotate(&mut plan, Some(&reading), None, None);
        assert_eq!(
            *at(&plan, "pick_offered"),
            "start /fno:blueprint subagent x-plan"
        );

        let mut none = Map::new();
        annotate(&mut none, Some(&bp(&[])), None, None);
        assert!(at(&none, "pick_offered").is_null());
        assert_eq!(
            lines(&none),
            vec!["pick: none offered (nothing startable in scope)".to_string()]
        );
    }
}
