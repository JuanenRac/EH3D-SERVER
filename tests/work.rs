//! The generic work of the machines end to end through the real router: the plugins, their machines, the queue of jobs with its history, the adapter of the Bungard
//! machines, the stream of events (server-sent events) and the notifications.

mod common;

use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use common::{admin, call, scoped_token, server, server_with, user, Server};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn demo_machine(s: &Server, boss: &str, id: &str, step_ms: u64) {
    let (status, body) = call(
        s,
        Method::POST,
        "/api/v1/work/machines",
        Some(boss),
        Some(json!({"id": id, "name": "Demo", "plugin": "demo", "settings": {"step_ms": step_ms}})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

async fn job_on(s: &Server, token: &str, machine: &str, steps: u64) -> Value {
    let (status, body) = call(
        s,
        Method::POST,
        "/api/v1/work/jobs",
        Some(token),
        Some(json!({"machine": machine, "request": {"kind": "demo", "steps": steps}, "name": "run", "app": "studio"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body
}

async fn act(s: &Server, token: &str, id: &str, what: &str) -> (StatusCode, Value) {
    call(
        s,
        Method::POST,
        &format!("/api/v1/work/jobs/{id}/{what}"),
        Some(token),
        None,
    )
    .await
}

async fn wait_for(s: &Server, token: &str, id: &str, wanted: &str) -> Value {
    let limit = Instant::now() + Duration::from_secs(60);
    loop {
        let (_, body) = call(
            s,
            Method::GET,
            &format!("/api/v1/work/jobs/{id}"),
            Some(token),
            None,
        )
        .await;
        if body["state"] == wanted {
            return body;
        }
        assert!(Instant::now() < limit, "waiting for {wanted}: {body}");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

#[tokio::test]
async fn the_plugins_and_their_machines_are_listed_and_managed_by_an_administrator() {
    let s = server("work-machines");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    let (status, plugins) = call(&s, Method::GET, "/api/v1/plugins", Some(&operator), None).await;
    assert_eq!(status, StatusCode::OK);
    let kinds: Vec<&str> = plugins["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, vec!["bungard", "demo"]);
    // only an administrator makes machines
    let body = json!({"id": "demo-1", "name": "Demo", "plugin": "demo"});
    let (status, _) = call(
        &s,
        Method::POST,
        "/api/v1/work/machines",
        Some(&operator),
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, made) = call(
        &s,
        Method::POST,
        "/api/v1/work/machines",
        Some(&boss),
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    let (status, again) = call(
        &s,
        Method::POST,
        "/api/v1/work/machines",
        Some(&boss),
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(again["error"], "machine_exists");
    for (body, code) in [
        (
            json!({"id": "x", "name": "X", "plugin": "laser"}),
            "unknown_plugin",
        ),
        (
            json!({"id": "x", "name": "X", "plugin": "bungard"}),
            "not_supported",
        ),
        (
            json!({"id": "Bad Id", "name": "X", "plugin": "demo"}),
            "bad_id",
        ),
        (json!({"id": "x", "name": "", "plugin": "demo"}), "bad_name"),
        (
            json!({"id": "x", "name": "X", "plugin": "demo", "settings": {"step_ms": "slow"}}),
            "bad_plugin_request",
        ),
        (
            json!({"id": "x", "name": "X", "plugin": "demo", "settings": [1]}),
            "bad_plugin_request",
        ),
    ] {
        let (status, r) = call(
            &s,
            Method::POST,
            "/api/v1/work/machines",
            Some(&boss),
            Some(body),
        )
        .await;
        assert!(
            status.is_client_error() || status.as_u16() == 501,
            "{code}: {status}"
        );
        assert_eq!(r["error"], code);
    }
    // a machine of the old routes is a machine of the bungard plugin here, and its id is taken
    let (status, _) = call(
        &s,
        Method::POST,
        "/api/v1/machines",
        Some(&boss),
        Some(json!({"id": "bm", "name": "Bungard", "model": "ccd-mtc", "connection": {"kind": "demo"}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, list) = call(
        &s,
        Method::GET,
        "/api/v1/work/machines",
        Some(&operator),
        None,
    )
    .await;
    let plugin_of = |id: &str| {
        list["machines"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == id)
            .map(|m| m["plugin"].as_str().unwrap().to_string())
    };
    assert_eq!(plugin_of("bm").as_deref(), Some("bungard"));
    assert_eq!(plugin_of("demo-1").as_deref(), Some("demo"));
    let (status, taken) = call(
        &s,
        Method::POST,
        "/api/v1/work/machines",
        Some(&boss),
        Some(json!({"id": "bm", "name": "X", "plugin": "demo"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(taken["error"], "machine_exists");
    // change and remove
    let (status, changed) = call(
        &s,
        Method::PUT,
        "/api/v1/work/machines/demo-1",
        Some(&boss),
        Some(json!({"name": "Demo 2", "settings": {"step_ms": 5}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    assert_eq!(changed["name"], "Demo 2");
    let (status, _) = call(
        &s,
        Method::GET,
        "/api/v1/work/machines/demo-1",
        Some(&operator),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, gone) = call(
        &s,
        Method::GET,
        "/api/v1/work/machines/nothing",
        Some(&operator),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(gone["error"], "unknown_machine");
    let (status, _) = call(
        &s,
        Method::DELETE,
        "/api/v1/work/machines/demo-1",
        Some(&boss),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        &s,
        Method::DELETE,
        "/api/v1/work/machines/demo-1",
        Some(&boss),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_job_goes_from_queued_to_running_held_and_cancelled_with_its_history() {
    let s = server("work-lifecycle");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    let viewer = user(&s, &boss, "view1", "viewer").await;
    // a step takes an hour: the job stays where it is put
    demo_machine(&s, &boss, "slow", 3_600_000).await;
    let first = job_on(&s, &operator, "slow", 3).await;
    let second = job_on(&s, &operator, "slow", 3).await;
    let (first_id, second_id) = (
        first["id"].as_str().unwrap().to_string(),
        second["id"].as_str().unwrap().to_string(),
    );
    assert!(first_id.starts_with("w-"));
    assert_eq!(
        (first["state"].as_str(), first["position"].as_u64()),
        (Some("queued"), Some(1))
    );
    assert_eq!(second["position"], 2);
    assert_eq!(first["app"], "studio");
    // a viewer sees the queue and does nothing to it
    let (status, _) = act(&s, &viewer, &first_id, "start").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, page) = call(
        &s,
        Method::GET,
        "/api/v1/work/jobs?state=queued",
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["total"], 2);
    // not running: nothing to hold
    let (status, r) = act(&s, &operator, &first_id, "pause").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(r["error"], "not_in_state");
    // one job at a time on a machine
    let (status, started) = act(&s, &operator, &first_id, "start").await;
    assert_eq!(status, StatusCode::OK, "{started}");
    assert_eq!(started["state"], "running");
    let (status, r) = act(&s, &operator, &second_id, "start").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(r["error"], "machine_busy");
    let (status, r) = act(&s, &operator, &first_id, "start").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(r["error"], "not_ready");
    let (_, machines) = call(
        &s,
        Method::GET,
        "/api/v1/work/machines/slow",
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(
        (machines["state"].as_str(), machines["job"].as_str()),
        (Some("busy"), Some(first_id.as_str()))
    );
    // hold, go on, hold again
    let (status, held) = act(&s, &operator, &first_id, "pause").await;
    assert_eq!(status, StatusCode::OK, "{held}");
    assert_eq!(held["state"], "paused");
    let (status, r) = act(&s, &operator, &first_id, "pause").await;
    assert_eq!(
        (status, r["error"].as_str()),
        (StatusCode::CONFLICT, Some("not_in_state"))
    );
    let (status, went_on) = act(&s, &operator, &first_id, "resume").await;
    assert_eq!(status, StatusCode::OK, "{went_on}");
    assert_eq!(went_on["state"], "running");
    // a job that runs is not removed; it is cancelled, and then it may be
    let (status, r) = call(
        &s,
        Method::DELETE,
        &format!("/api/v1/work/jobs/{first_id}"),
        Some(&operator),
        None,
    )
    .await;
    assert_eq!(
        (status, r["error"].as_str()),
        (StatusCode::CONFLICT, Some("job_active"))
    );
    let (status, stopped) = act(&s, &operator, &first_id, "cancel").await;
    assert_eq!(status, StatusCode::OK, "{stopped}");
    assert_eq!(stopped["state"], "cancelled");
    let states: Vec<&str> = stopped["history"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["state"].as_str().unwrap())
        .collect();
    assert_eq!(
        states,
        vec!["queued", "running", "paused", "running", "cancelled"]
    );
    let (status, r) = act(&s, &operator, &first_id, "cancel").await;
    assert_eq!(
        (status, r["error"].as_str()),
        (StatusCode::CONFLICT, Some("not_cancellable"))
    );
    // the machine is free again; a queued job can be cancelled before it starts
    let (status, _) = act(&s, &operator, &second_id, "start").await;
    assert_eq!(status, StatusCode::OK);
    let third = job_on(&s, &operator, "slow", 2).await;
    let third_id = third["id"].as_str().unwrap();
    let (_, gone) = act(&s, &operator, third_id, "cancel").await;
    assert_eq!(gone["state"], "cancelled");
    let (status, _) = call(
        &s,
        Method::DELETE,
        &format!("/api/v1/work/jobs/{third_id}"),
        Some(&operator),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, r) = call(
        &s,
        Method::GET,
        &format!("/api/v1/work/jobs/{third_id}"),
        Some(&operator),
        None,
    )
    .await;
    assert_eq!(
        (status, r["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("unknown_job"))
    );
}

#[tokio::test]
async fn a_job_finishes_by_itself_tells_its_owner_and_is_found_by_the_filters() {
    let s = server("work-finish");
    let boss = admin(&s).await;
    let ana = user(&s, &boss, "ana1", "operator").await;
    let bob = user(&s, &boss, "bob1", "operator").await;
    demo_machine(&s, &boss, "fast", 0).await;
    let job = job_on(&s, &ana, "fast", 5).await;
    let id = job["id"].as_str().unwrap().to_string();
    let (status, _) = act(&s, &ana, &id, "start").await;
    assert_eq!(status, StatusCode::OK);
    let done = wait_for(&s, &ana, &id, "done").await;
    assert_eq!(
        (
            done["done"].as_u64(),
            done["total"].as_u64(),
            done["percent"].as_u64()
        ),
        (Some(5), Some(5), Some(100))
    );
    assert!(done["finished"].as_u64().is_some());
    assert_eq!(
        done["history"].as_array().unwrap().last().unwrap()["by"],
        "machine"
    );
    // the owner is told, the others are not
    let (_, mine) = call(
        &s,
        Method::GET,
        "/api/v1/notifications?unread=true",
        Some(&ana),
        None,
    )
    .await;
    let list = mine["notifications"].as_array().unwrap();
    assert_eq!(list.len(), 1, "{mine}");
    assert_eq!(list[0]["kind"], "job.done");
    assert_eq!(list[0]["data"]["job"], id.as_str());
    let (_, others) = call(&s, Method::GET, "/api/v1/notifications", Some(&bob), None).await;
    assert_eq!(others["notifications"].as_array().unwrap().len(), 0);
    let (_, all) = call(&s, Method::GET, "/api/v1/notifications", Some(&boss), None).await;
    assert_eq!(all["notifications"].as_array().unwrap().len(), 1);
    let (status, marked) = call(
        &s,
        Method::POST,
        "/api/v1/notifications/read",
        Some(&ana),
        Some(json!({"all": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(marked["marked"], 1);
    let (_, unread) = call(
        &s,
        Method::GET,
        "/api/v1/notifications?unread=true",
        Some(&ana),
        None,
    )
    .await;
    assert_eq!(unread["notifications"].as_array().unwrap().len(), 0);
    // the filters
    let other = job_on(&s, &bob, "fast", 1).await;
    assert_eq!(other["state"], "queued");
    for (query, total) in [
        ("?state=done", 1),
        ("?state=queued", 1),
        ("?mine=true", 1),
        ("?machine=fast", 2),
        ("?machine=nothing", 0),
        ("?app=studio", 2),
        ("?app=android", 0),
        ("", 2),
    ] {
        let (_, page) = call(
            &s,
            Method::GET,
            &format!("/api/v1/work/jobs{query}"),
            Some(&ana),
            None,
        )
        .await;
        assert_eq!(page["total"], total, "{query}");
    }
}

#[tokio::test]
async fn a_request_the_plugin_does_not_take_and_a_full_queue_are_refused() {
    let s = server_with("work-limits", |c| c.work_queue_max = 1);
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    demo_machine(&s, &boss, "slow", 3_600_000).await;
    for (body, wanted, code) in [
        (
            json!({"machine": "slow", "request": {"kind": "drill"}}),
            422,
            "bad_plugin_request",
        ),
        (
            json!({"machine": "slow", "request": {"kind": "demo", "steps": 0}}),
            422,
            "bad_plugin_request",
        ),
        (
            json!({"machine": "slow", "request": {"kind": "demo", "steps": 100001}}),
            422,
            "bad_plugin_request",
        ),
        (
            json!({"machine": "nothing", "request": {"kind": "demo"}}),
            404,
            "unknown_machine",
        ),
        (
            json!({"machine": "slow", "request": {"kind": "demo"}, "app": "Not An App"}),
            422,
            "bad_app",
        ),
    ] {
        let (status, r) = call(
            &s,
            Method::POST,
            "/api/v1/work/jobs",
            Some(&operator),
            Some(body),
        )
        .await;
        assert_eq!(status.as_u16(), wanted, "{code}");
        assert_eq!(r["error"], code);
    }
    job_on(&s, &operator, "slow", 1).await;
    let (status, r) = call(
        &s,
        Method::POST,
        "/api/v1/work/jobs",
        Some(&operator),
        Some(json!({"machine": "slow", "request": {"kind": "demo"}})),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r["error"], "queue_full");
    // a machine with an open job is not removed
    let (status, r) = call(
        &s,
        Method::DELETE,
        "/api/v1/work/machines/slow",
        Some(&boss),
        None,
    )
    .await;
    assert_eq!(
        (status, r["error"].as_str()),
        (StatusCode::CONFLICT, Some("machine_busy"))
    );
    // a token limited by scope does what its scopes say
    let preparer = scoped_token(&s, &boss, "operator", &["jobs:prepare", "machines:view"]).await;
    let (status, r) = call(&s, Method::GET, "/api/v1/work/jobs", Some(&preparer), None).await;
    assert_eq!(status, StatusCode::OK, "{r}");
    let id = r["items"][0]["id"].as_str().unwrap().to_string();
    let (status, r) = act(&s, &preparer, &id, "start").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(r["error"], "forbidden_scope");
    let (status, r) = call(
        &s,
        Method::POST,
        "/api/v1/work/machines",
        Some(&preparer),
        Some(json!({"id": "x", "name": "X", "plugin": "demo"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(r["error"], "forbidden");
}

fn drill_job() -> Value {
    json!({
        "kind": "drill", "name": "board",
        "tools": [{"number": 1, "feed_mm_min": 1000, "spindle_rpm": 24000}],
        "holes": [{"tool": 1, "x_mm": 10, "y_mm": 10}, {"tool": 1, "x_mm": 20, "y_mm": 10}]
    })
}

#[tokio::test]
async fn the_bungard_plugin_works_on_the_same_machines_and_the_same_jobs_as_the_old_routes() {
    let s = server("work-bungard");
    let boss = admin(&s).await;
    let (status, _) = call(
        &s,
        Method::POST,
        "/api/v1/machines",
        Some(&boss),
        Some(json!({"id": "bm", "name": "Bungard", "model": "ccd-mtc", "connection": {"kind": "demo"}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // a free G-code job through the generic queue
    let (status, made) = call(
        &s,
        Method::POST,
        "/api/v1/work/jobs",
        Some(&boss),
        Some(json!({"machine": "bm", "request": {"kind": "gcode", "name": "frame", "gcode": "(a frame)\nG21 G90\nG0 X10 Y10\nG1 X40 F300\nM30\n"}})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    assert_eq!(made["plugin"], "bungard");
    let id = made["id"].as_str().unwrap().to_string();
    let native = made["native"].as_str().unwrap().to_string();
    // the same job is in the book of the old routes
    let (status, old) = call(
        &s,
        Method::GET,
        &format!("/api/v1/jobs/{native}"),
        Some(&boss),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(old["state"], "ready");
    assert_eq!(old["name"], "frame");
    let (status, started) = act(&s, &boss, &id, "start").await;
    assert_eq!(status, StatusCode::OK, "{started}");
    let done = wait_for(&s, &boss, &id, "done").await;
    assert_eq!(done["done"], done["total"]);
    let (_, old) = call(
        &s,
        Method::GET,
        &format!("/api/v1/jobs/{native}"),
        Some(&boss),
        None,
    )
    .await;
    assert_eq!(old["state"], "done");
    // a job made through the old routes starts through the old routes, and the plugin sees its state
    let (_, drill) = call(
        &s,
        Method::POST,
        "/api/v1/machines/bm/jobs",
        Some(&boss),
        Some(drill_job()),
    )
    .await;
    assert_eq!(drill["state"], "ready");
    // a request of the wrong kind is refused by the planning
    let (status, r) = call(
        &s,
        Method::POST,
        "/api/v1/work/jobs",
        Some(&boss),
        Some(json!({"machine": "bm", "request": {"kind": "teapot"}})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r["error"], "bad_plugin_request");
    // a drilling job waits at the dialog of the tool; it cannot be held, only stopped
    let (status, made) = call(
        &s,
        Method::POST,
        "/api/v1/work/jobs",
        Some(&boss),
        Some(json!({"machine": "bm", "request": drill_job()})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    let id = made["id"].as_str().unwrap().to_string();
    assert_eq!(made["view"]["kind"], "drill");
    let (status, _) = act(&s, &boss, &id, "start").await;
    assert_eq!(status, StatusCode::OK);
    let limit = Instant::now() + Duration::from_secs(60);
    loop {
        let (_, now) = call(
            &s,
            Method::GET,
            &format!("/api/v1/work/jobs/{id}"),
            Some(&boss),
            None,
        )
        .await;
        if now["message"]
            .as_str()
            .is_some_and(|m| m.starts_with("waiting for the operator"))
        {
            break;
        }
        assert!(Instant::now() < limit, "{now}");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    let (status, r) = act(&s, &boss, &id, "pause").await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{r}");
    assert_eq!(r["error"], "not_supported");
    let (status, stopped) = act(&s, &boss, &id, "cancel").await;
    assert_eq!(status, StatusCode::OK, "{stopped}");
    assert_eq!(stopped["state"], "cancelled");
    // the operator who is needed was told
    let (_, notes) = call(&s, Method::GET, "/api/v1/notifications", Some(&boss), None).await;
    let kinds: Vec<&str> = notes["notifications"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"job.waiting"), "{kinds:?}");
}

// ---- the stream of events -----------------------------------------------------------------------------------------------------------

struct Stream {
    body: Body,
    text: String,
}

#[derive(Debug)]
struct Sse {
    id: Option<String>,
    event: String,
    data: Value,
}

impl Stream {
    /// The next event of the stream, or `None` when nothing comes within the time.
    async fn next(&mut self, within: Duration) -> Option<Sse> {
        let limit = Instant::now() + within;
        loop {
            if let Some(end) = self.text.find("\n\n") {
                let block: String = self.text.drain(..end + 2).collect();
                let (mut id, mut event, mut data) = (None, String::new(), String::new());
                for line in block.lines() {
                    if let Some(v) = line.strip_prefix("id: ") {
                        id = Some(v.to_string());
                    } else if let Some(v) = line.strip_prefix("event: ") {
                        event = v.to_string();
                    } else if let Some(v) = line.strip_prefix("data: ") {
                        data.push_str(v);
                    }
                }
                if event.is_empty() {
                    // a comment of the keep-alive
                    continue;
                }
                return Some(Sse {
                    id,
                    event,
                    data: serde_json::from_str(&data).unwrap_or(Value::Null),
                });
            }
            let left = limit.checked_duration_since(Instant::now())?;
            match tokio::time::timeout(left, self.body.frame()).await {
                Ok(Some(Ok(frame))) => {
                    if let Ok(bytes) = frame.into_data() {
                        self.text.push_str(&String::from_utf8_lossy(&bytes));
                    }
                }
                _ => return None,
            }
        }
    }
}

async fn open_stream(
    s: &Server,
    token: &str,
    query: &str,
    last: Option<&str>,
) -> (StatusCode, Option<Stream>, Value) {
    let mut request = Request::builder()
        .method(Method::GET)
        .uri(format!("/api/v1/events/stream{query}"))
        .header("authorization", format!("Bearer {token}"));
    if let Some(last) = last {
        request = request.header("last-event-id", last);
    }
    let response = s
        .app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    if status != StatusCode::OK {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        return (
            status,
            None,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        );
    }
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("text/event-stream")
    );
    (
        status,
        Some(Stream {
            body: response.into_body(),
            text: String::new(),
        }),
        Value::Null,
    )
}

const SHORT: Duration = Duration::from_millis(400);
const LONG: Duration = Duration::from_secs(10);

#[tokio::test]
async fn the_stream_tells_what_happens_and_a_client_that_reconnects_gets_what_it_missed() {
    let s = server("work-stream");
    let boss = admin(&s).await;
    let ana = user(&s, &boss, "ana1", "operator").await;
    demo_machine(&s, &boss, "fast", 0).await;
    // a client that is already listening hears the new events as they come
    let (status, live, _) = open_stream(&s, &ana, "", None).await;
    assert_eq!(status, StatusCode::OK);
    let mut live = live.unwrap();
    let hello = live.next(LONG).await.unwrap();
    assert_eq!(hello.event, "hello");
    assert_eq!(hello.data["last_id"], 1);
    assert_eq!(hello.data["resumed"], false);
    assert!(hello.id.is_none());
    let job = job_on(&s, &ana, "fast", 2).await;
    let id = job["id"].as_str().unwrap().to_string();
    let created = live.next(LONG).await.unwrap();
    assert_eq!(created.event, "job.created");
    assert_eq!(created.id.as_deref(), Some("2"));
    assert_eq!(created.data["job"], id.as_str());
    assert_eq!(created.data["data"]["state"], "queued");
    act(&s, &ana, &id, "start").await;
    wait_for(&s, &ana, &id, "done").await;
    let mut kinds = Vec::new();
    while let Some(e) = live.next(SHORT).await {
        kinds.push(e.event);
    }
    assert!(kinds.iter().any(|k| k == "job.state"), "{kinds:?}");
    assert!(kinds.iter().any(|k| k == "notification"), "{kinds:?}");
    // reconnecting after the first event: the others come first, in order
    let (_, again, _) = open_stream(&s, &ana, "", Some("1")).await;
    let mut again = again.unwrap();
    let hello = again.next(LONG).await.unwrap();
    assert_eq!(
        (hello.data["resumed"].clone(), hello.data["resync"].clone()),
        (json!(true), json!(false))
    );
    let first = again.next(LONG).await.unwrap();
    assert_eq!(
        (first.event.as_str(), first.id.as_deref()),
        ("job.created", Some("2"))
    );
    // the filters
    let (_, only_job, _) =
        open_stream(&s, &ana, &format!("?job={id}&kinds=job.state"), Some("0")).await;
    let mut only_job = only_job.unwrap();
    only_job.next(LONG).await.unwrap();
    while let Some(e) = only_job.next(SHORT).await {
        assert_eq!(e.event, "job.state");
        assert_eq!(e.data["job"], id.as_str());
    }
    let (_, other_machine, _) = open_stream(&s, &ana, "?machine=nothing", Some("0")).await;
    let mut other_machine = other_machine.unwrap();
    other_machine.next(LONG).await.unwrap();
    assert!(other_machine.next(SHORT).await.is_none());
}

#[tokio::test]
async fn the_stream_tells_each_person_only_what_they_may_hear() {
    let s = server_with("work-stream-owner", |c| c.events_clients_max = 2);
    let boss = admin(&s).await;
    let ana = user(&s, &boss, "ana1", "operator").await;
    let bob = user(&s, &boss, "bob1", "operator").await;
    let (status, made) = call(
        &s,
        Method::POST,
        "/api/v1/apps/studio/projects",
        Some(&ana),
        Some(json!({"name": "mine"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    let (_, project, _) = open_stream(&s, &bob, "", Some("0")).await;
    let mut for_bob = project.unwrap();
    assert_eq!(for_bob.next(LONG).await.unwrap().event, "hello");
    // bob does not hear the project of ana
    assert!(for_bob.next(SHORT).await.is_none());
    let (_, project, _) = open_stream(&s, &ana, "", Some("0")).await;
    let mut for_ana = project.unwrap();
    for_ana.next(LONG).await.unwrap();
    let heard = for_ana.next(LONG).await.unwrap();
    assert_eq!(heard.event, "project.created");
    assert_eq!(heard.data["data"]["name"], "mine");
    // a token limited to the scope of the machines does not hear projects either
    let machines_only = scoped_token(&s, &boss, "viewer", &["machines:view"]).await;
    let (status, third, _) = open_stream(&s, &machines_only, "", Some("0")).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(third.is_none());
    drop(for_bob);
    let (status, third, _) = open_stream(&s, &machines_only, "", Some("0")).await;
    assert_eq!(status, StatusCode::OK);
    let mut third = third.unwrap();
    third.next(LONG).await.unwrap();
    assert!(third.next(SHORT).await.is_none());
    // no login, no stream
    let (status, none, body) = open_stream(&s, "not-a-token", "", None).await;
    assert_eq!(
        (status, none.is_none(), body["error"].as_str()),
        (StatusCode::UNAUTHORIZED, true, Some("unauthorized"))
    );
}
