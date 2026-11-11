//! `/hq/model.json`: the org view's fleet fold as JSON. The bridge keeps
//! the last good read and refreshes it off the request path.

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::backlog_model::Inputs;
use crate::client::org_seats::{self, Fleet};
use crate::digest_overlay::ObsidianCfg;
use crate::link::{plan_link, PlanLink};
use crate::needs_overlay::{QuestionItem, QuestionsFold};

pub(crate) const REFRESH_AFTER: Duration = Duration::from_secs(60);

#[derive(Default)]
pub(crate) struct HqCache {
    state: tokio::sync::Mutex<Board>,
    /// The first read: every request before it waits on the same one.
    pub(crate) first: tokio::sync::OnceCell<()>,
}

/// One open question as the board shows it: the fold's item plus the live
/// pane the asker runs in, when one joins.
#[derive(Debug, Clone)]
pub(crate) struct OpenQuestion {
    pub(crate) item: QuestionItem,
    pub(crate) watch_pane: Option<u64>,
    node_origin: Option<crate::backlog_model::OriginSession>,
}

/// The command center board: the fleet fold, the open questions and the open
/// PRs. The bridge's `/hq/model.json` and the desktop app both build it here,
/// so the two never show different seats or questions.
#[derive(Default, Clone)]
pub(crate) struct Board {
    /// The fold, its machines as JSON, and when it was read.
    good: Option<(Fleet, Value, Instant)>,
    failure: Option<(String, Instant)>,
    /// The last good question read. A failed read keeps it and sets the error.
    questions: Vec<OpenQuestion>,
    questions_error: Option<String>,
    questions_read: bool,
    prs: Vec<Value>,
    /// The last backlog read failed; `prs` is the last good list.
    prs_error: Option<String>,
    read_at: Option<Instant>,
}

impl HqCache {
    /// True when the caller should start a refresh. Claiming stamps the
    /// attempt, so a burst of reads starts one and a lost refresh retries.
    pub(crate) async fn claim_refresh(&self) -> bool {
        let mut st = self.state.lock().await;
        if st.read_at.is_some_and(|at| at.elapsed() < REFRESH_AFTER) {
            return false;
        }
        st.read_at = Some(Instant::now());
        true
    }

    /// The board, locked. A refresh records into it.
    pub(crate) async fn board(&self) -> tokio::sync::MutexGuard<'_, Board> {
        self.state.lock().await
    }

    /// The node of a question the page already shows, from the last read. `None`
    /// means the question is not in the cache; `Some(None)` means it has no node.
    pub(crate) async fn question_node(&self, id: &str) -> Option<Option<String>> {
        let st = self.state.lock().await;
        if st.questions_error().is_some() {
            return None;
        }
        let q = st.questions.iter().find(|q| q.item.id == id)?;
        Some(q.item.node.clone())
    }

    pub(crate) async fn model(&self) -> Value {
        self.state.lock().await.model()
    }
}

impl Board {
    /// Store one refresh. A failed fleet read keeps the last good fold, and a
    /// failed question read keeps the last good questions. The local roster
    /// joins the asker's session to a pane, so a question card can link to
    /// the live session.
    pub(crate) fn record(
        &mut self,
        fleet: Result<Value, String>,
        questions: Result<QuestionsFold, String>,
        backlog: &Inputs,
        measured_at: u64,
        agents: &[crate::proto::AgentRow],
    ) {
        let now = Instant::now();
        match fleet {
            Ok(map) => {
                let fleet = org_seats::fold(&map, backlog, &[], measured_at);
                let cwd = std::env::current_dir().unwrap_or_default();
                let machines = machines_json(&fleet, &ObsidianCfg::read(&cwd));
                self.good = Some((fleet, machines, now));
                self.failure = None;
            }
            Err(reason) => self.failure = Some((reason, now)),
        }
        self.questions_read = true;
        match questions {
            Ok(fold) => {
                self.questions = open_questions(fold, backlog, agents);
                self.questions_error = None;
            }
            Err(reason) => self.questions_error = Some(reason),
        }
        match &backlog.rows_error {
            Some(reason) => self.prs_error = Some(reason.clone()),
            None => (self.prs, self.prs_error) = (open_prs(backlog), None),
        }
        self.read_at = Some(now);
    }

    /// The last good fleet fold, if any read has succeeded.
    pub(crate) fn fleet(&self) -> Option<&Fleet> {
        self.good.as_ref().map(|(f, _, _)| f)
    }

    /// Why the newest fleet read failed, or that none has run.
    #[cfg(feature = "desktop")]
    pub(crate) fn fleet_error(&self) -> Option<&str> {
        match (&self.failure, &self.good) {
            (Some((reason, _)), _) => Some(reason),
            (None, None) => Some("fleet not read yet"),
            (None, Some(_)) => None,
        }
    }

    /// False until the first refresh lands: the errors then name "not read
    /// yet", which a view shows as waiting, never as a failure.
    #[cfg(feature = "desktop")]
    pub(crate) fn read_yet(&self) -> bool {
        self.questions_read
    }

    /// The last good open questions, in fold order.
    #[cfg(feature = "desktop")]
    pub(crate) fn questions(&self) -> &[OpenQuestion] {
        &self.questions
    }

    /// The same questions newest first, the order the desktop views show.
    #[cfg(feature = "desktop")]
    pub(crate) fn questions_newest_first(&self) -> Vec<&OpenQuestion> {
        let mut out: Vec<&OpenQuestion> = self.questions.iter().collect();
        out.sort_by(|a, b| b.item.created_at.cmp(&a.item.created_at));
        out
    }

    /// Why the newest question read failed, or that none has run.
    pub(crate) fn questions_error(&self) -> Option<&str> {
        match (&self.questions_error, self.questions_read) {
            (Some(reason), _) => Some(reason),
            (None, false) => Some("not read yet"),
            (None, true) => None,
        }
    }

    pub(crate) fn model(&self) -> Value {
        let mut waiting = serde_json::Map::new();
        waiting.insert("prs".into(), json!(self.prs));
        waiting.insert("prs_error".into(), json!(self.prs_error));
        match self.questions_error() {
            Some(reason) => waiting.insert("questions_error".into(), json!(reason)),
            None => {
                let items: Vec<Value> = self.questions.iter().map(question_json).collect();
                waiting.insert("questions".into(), json!(items))
            }
        };
        let fleet = self.fleet();
        let fleet_error = match (&self.failure, &self.good) {
            (Some((reason, at)), _) => {
                json!({"reason": reason, "failed_age_s": at.elapsed().as_secs()})
            }
            (None, None) => json!({ "reason": "fleet not read yet", "failed_age_s": null }),
            (None, Some(_)) => Value::Null,
        };
        json!({
            "schema": 1,
            "measured_at": fleet.map(|f| f.measured_at),
            "age_s": self.good.as_ref().map(|(_, _, at)| at.elapsed().as_secs()),
            "notice": fleet.and_then(|f| f.notice.clone()),
            "seat_error": fleet.and_then(|f| f.seat_error.clone()),
            "fleet_error": fleet_error,
            "machines": self.good.as_ref().map_or_else(|| json!([]), |(_, m, _)| m.clone()),
            "waiting": waiting,
        })
    }
}

/// The fold's open, unsettled questions, each joined to the live pane its
/// asker runs in.
fn open_questions(
    fold: QuestionsFold,
    backlog: &Inputs,
    agents: &[crate::proto::AgentRow],
) -> Vec<OpenQuestion> {
    fold.items
        .into_iter()
        .filter(|q| q.state == "open" && !q.settled)
        .map(|item| {
            let watch_pane = item
                .origin
                .as_ref()
                .and_then(|o| o.session_id.as_deref())
                .and_then(|sid| {
                    agents
                        .iter()
                        .filter(|a| !a.exited)
                        .find(|a| a.harness_session_id.as_deref() == Some(sid))
                        .and_then(|a| a.pane_id)
                });
            let node_origin = item
                .node
                .as_deref()
                .and_then(|n| crate::backlog_model::origin_session_for(backlog, n));
            OpenQuestion {
                item,
                watch_pane,
                node_origin,
            }
        })
        .collect()
}

fn question_json(open: &OpenQuestion) -> Value {
    let q = &open.item;
    json!({
        "id": q.id,
        "number": q.number,
        "kind": q.kind,
        "title": q.title,
        "node": q.node,
        "host": q.host,
        "created_at": q.created_at,
        "priority": q.priority,
        "class": q.class,
        "tried": q.tried,
        "evidence": q.evidence,
        "on_silence": q.on_silence,
        "deadline": q.deadline,
        "options": q.options.iter().map(|o| json!({"n": o.n, "text": o.text})).collect::<Vec<_>>(),
        "recommendation": q.recommendation.as_ref().map(|r| json!({"option": r.option, "why": r.why})),
        "origin": q.origin,
        "rung": q.rung,
        "via": q.via,
        "node_title": q.node_title,
        "node_stage": q.node_stage,
        "watch_pane": open.watch_pane,
        "node_origin": open.node_origin,
    })
}

/// The machines with each seat's plan link, so the page builds none. With obsidian on, a plan in the vault gets its
/// `obsidian://` link; any other plan gets a plain `file://` link.
fn machines_json(fleet: &Fleet, obsidian: &ObsidianCfg) -> Value {
    let mut machines = json!(fleet.machines);
    let rows = machines
        .as_array_mut()
        .into_iter()
        .flatten()
        .flat_map(|m| m["leads"].as_array_mut().into_iter().flatten())
        .flat_map(|l| l["seats"].as_array_mut().into_iter().flatten());
    for (row, seat) in rows.zip(fleet.seats()) {
        let plan = seat.work.as_ref().and_then(|w| w.plan.as_deref());
        match (plan_link(plan.map(Path::new), obsidian), plan) {
            (PlanLink::Obsidian { uri }, _) => row["plan_uri"] = json!(uri),
            (_, Some(plan)) if Path::new(plan).is_absolute() => {
                row["plan_file"] = json!(format!(
                    "file://{}",
                    crate::link::percent_encode_query(plan)
                ))
            }
            _ => {}
        }
    }
    machines
}

/// PRs on in-review nodes, the primary binding and `additional_prs`, that the
/// store has not seen merged or closed. The store records no review or
/// conflict word, so `merge_status` is its raw word or null.
fn open_prs(backlog: &Inputs) -> Vec<Value> {
    let text = |v: &Option<String>| v.clone().filter(|s| !s.is_empty());
    backlog
        .rows
        .iter()
        .filter(|row| row.status.as_ref() == Some(&crate::node_row::Status::InReview))
        .flat_map(|row| {
            let primary = row
                .pr_number
                .map(|n| (n, text(&row.pr_url), text(&row.merge_status)));
            let extra = row
                .additional_prs()
                .iter()
                .filter_map(|p| Some((p.number?, text(&p.url), text(&p.merge_status))));
            primary
                .into_iter()
                .chain(extra)
                .filter(|(_, _, m)| !matches!(m.as_deref(), Some("merged" | "closed")))
                .map(move |(number, url, merge_status)| {
                    json!({"node": text(&row.id), "title": text(&row.title), "number": number,
                    "url": url, "merge_status": merge_status})
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_machines() -> Value {
        json!({"machines": [
            {"host": "u", "local": true, "fresh": true, "sessions": [
                {"name": "kara", "session_id": "976b7b64", "harness": "claude", "role": "lead"},
                {"name": "t-one", "session_id": "aaaa1111", "parent": "976b", "harness": "claude",
                 "node": "x-1", "status": "writing"}
            ]},
            {"host": "mac2", "fresh": false, "age_s": 900, "sessions": [
                {"name": "t-two", "harness": "codex"}
            ]}
        ]})
    }

    fn questions(hosts: &[Option<&str>]) -> QuestionsFold {
        QuestionsFold {
            items: hosts
                .iter()
                .enumerate()
                .map(|(i, h)| QuestionItem {
                    id: format!("q-{i}"),
                    host: h.map(str::to_string),
                    state: ["open", "answered"][usize::from(*h == Some("answered"))].into(),
                    origin: (i == 0).then(|| crate::needs_overlay::QuestionOrigin {
                        handle: Some("w1".into()),
                        session_id: Some("aaaa1111-zzzz".into()),
                        name: Some("t-one".into()),
                        harness: Some("claude".into()),
                        host: Some("u".into()),
                    }),
                    rung: (i == 0).then(|| crate::needs_overlay::QuestionRung {
                        level: Some("user".into()),
                        ..Default::default()
                    }),
                    via: (i == 0)
                        .then(|| {
                            vec![crate::needs_overlay::QuestionHop {
                                holder_name: Some("tomas".into()),
                                evidence: Some("read PR 12".into()),
                                ..Default::default()
                            }]
                        })
                        .unwrap_or_default(),
                    tried: (i == 0).then(|| "two leads declined".into()),
                    node_title: (i == 0).then(|| "seat table drawer".into()),
                    node_stage: (i == 0).then(|| "in_review".into()),
                    class: (i == 0).then(|| "irreversible".into()),
                    deadline: (i == 0).then(|| "2026-10-30T09:00:00Z".into()),
                    on_silence: (i == 0).then(|| "proceed".into()),
                    ..QuestionItem::default()
                })
                .collect(),
            ..QuestionsFold::default()
        }
    }

    async fn read(
        cache: &HqCache,
        fleet: Result<Value, String>,
        qs: Result<QuestionsFold, String>,
        at: u64,
    ) -> Value {
        read_with_agents(cache, fleet, qs, at, &[]).await
    }

    async fn read_with_agents(
        cache: &HqCache,
        fleet: Result<Value, String>,
        qs: Result<QuestionsFold, String>,
        at: u64,
        agents: &[crate::proto::AgentRow],
    ) -> Value {
        let rows = vec![
            json!({"id": "x-1", "status": "in_review", "pr_number": 7, "merge_status": "",
                "additional_prs": [{"number": 9}, {"number": 10, "merge_status": "closed"}]}),
            json!({"id": "x-2", "status": "in_review", "pr_number": 8, "merge_status": "merged"}),
        ];
        let backlog = Inputs {
            rows: crate::node_row::rows_from_values(rows),
            ..Inputs::default()
        };
        cache.board().await.record(fleet, qs, &backlog, at, agents);
        cache.model().await
    }

    #[tokio::test]
    async fn two_machine_fold_serializes_every_seat_and_question_host() {
        let qs = questions(&[None, Some("mac2"), Some("mac2"), Some("answered")]);
        let m = read(&HqCache::default(), Ok(two_machines()), Ok(qs), 100).await;
        let seats = |i: usize| {
            m["machines"][i]["leads"][0]["seats"]
                .as_array()
                .map(Vec::len)
        };
        assert_eq!((seats(0), seats(1), seats(2)), (Some(2), Some(1), None));
        assert!(m["machines"][0]["leads"][0]["seats"][0]
            .get("local")
            .is_none());
        let imac = &m["machines"][1]["leads"][0]["seats"][0];
        assert_eq!(imac["name"], "t-two");
        assert!(
            imac.get("attach").is_none(),
            "the page hands out no command"
        );
        assert_eq!(m["waiting"]["questions"][1]["host"], "mac2");
        assert_eq!(m["waiting"]["questions"].as_array().unwrap().len(), 3);
        // The provenance fields ride through, and an asker whose session
        // joins no live local pane carries no watch pane.
        let q0 = &m["waiting"]["questions"][0];
        assert_eq!(q0["origin"]["name"], "t-one");
        assert_eq!(q0["origin"]["session_id"], "aaaa1111-zzzz");
        assert_eq!(q0["via"][0]["holder_name"], "tomas");
        assert_eq!(q0["rung"]["level"], "user");
        assert_eq!(q0["tried"], "two leads declined");
        assert_eq!(q0["node_title"], "seat table drawer");
        assert_eq!(q0["node_stage"], "in_review");
        assert_eq!(q0["class"], "irreversible");
        assert_eq!(q0["deadline"], "2026-10-30T09:00:00Z");
        assert_eq!(q0["on_silence"], "proceed");
        assert!(q0["watch_pane"].is_null());
        assert_eq!(
            m["waiting"]["prs"],
            json!([
                {"node": "x-1", "title": null, "number": 7, "url": null, "merge_status": null},
                {"node": "x-1", "title": null, "number": 9, "url": null, "merge_status": null}
            ])
        );
        assert_eq!(
            (&m["measured_at"], &m["fleet_error"]),
            (&json!(100), &Value::Null)
        );
    }
    #[tokio::test]
    async fn a_failed_read_names_its_reason_and_keeps_the_last_good_fold() {
        let cache = HqCache::default();
        let fail = |r: &str| Err::<Value, String>(r.into());
        let m = read(&cache, fail("no primary"), Ok(questions(&[None])), 1).await;
        assert_eq!(m["machines"], json!([]));
        assert_eq!(m["fleet_error"]["reason"], "no primary");
        assert_eq!(m["waiting"]["questions"].as_array().unwrap().len(), 1);
        read(&cache, Ok(two_machines()), Err("timed out".into()), 100).await;
        let m = read(
            &cache,
            fail("fleet top timed out"),
            Err("timed out".into()),
            200,
        )
        .await;
        assert_eq!(m["machines"].as_array().unwrap().len(), 2);
        assert_eq!(m["measured_at"], 100);
        assert_eq!(m["fleet_error"]["reason"], "fleet top timed out");
        assert!(m["age_s"].is_u64());
        assert!(m["waiting"].get("questions").is_none());
        assert_eq!(m["waiting"]["questions_error"], "timed out");
        let locked = Inputs {
            rows_error: Some("graph locked".into()),
            ..Inputs::default()
        };
        cache
            .board()
            .await
            .record(fail("x"), Err("x".into()), &locked, 300, &[]);
        let m = cache.model().await;
        assert_eq!(m["waiting"]["prs"].as_array().map(Vec::len), Some(2));
        assert_eq!(m["waiting"]["prs_error"], "graph locked");
    }
}
