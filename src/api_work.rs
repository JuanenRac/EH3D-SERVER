//! The interface of the generic work of the machines: the plugins and their machines (`/api/v1/plugins`, `/api/v1/work/machines`), the queue of jobs with its history
//! (`/api/v1/work/jobs`), the stream of events (`/api/v1/events/stream`, server-sent events) and the notifications (`/api/v1/notifications`). The old routes of the machines
//! and of the jobs (`/api/v1/machines`, `/api/v1/jobs`) are not touched; the `bungard` plugin works on the same machines and the same jobs. The rules are in `src/work.rs`
//! and `src/plugins.rs`, and `docs/MACHINE_PLUGINS.md` explains them.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;

use crate::api::{identify_open, ApiError, ApiJson, ApiResult, Authed, Shared, TokenQuery};
use crate::auth::{Identity, Role};
use crate::feed::{ClientGuard, FeedEvent, Filter};
use crate::jobs::now;
use crate::machines::valid_id;
use crate::plugins::PluginError;
use crate::state::AppState;
use crate::sync::Locked;
use crate::work::{Fail, WError, WorkMachine};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/v1/plugins", get(list_plugins))
        .route(
            "/api/v1/work/machines",
            get(list_machines).post(add_machine),
        )
        .route(
            "/api/v1/work/machines/:id",
            get(get_machine).put(change_machine).delete(remove_machine),
        )
        .route("/api/v1/work/jobs", get(list_jobs).post(create_job))
        .route("/api/v1/work/jobs/:id", get(get_job).delete(remove_job))
        .route("/api/v1/work/jobs/:id/start", post(start_job))
        .route("/api/v1/work/jobs/:id/pause", post(pause_job))
        .route("/api/v1/work/jobs/:id/resume", post(resume_job))
        .route("/api/v1/work/jobs/:id/cancel", post(cancel_job))
        .route("/api/v1/events/stream", get(stream_events))
        .route("/api/v1/notifications", get(list_notifications))
        .route("/api/v1/notifications/read", post(read_notifications))
}

impl From<Fail> for ApiError {
    fn from(e: Fail) -> Self {
        let status = StatusCode::from_u16(e.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        ApiError::new(status, e.code(), e.detail())
    }
}

impl From<WError> for ApiError {
    fn from(e: WError) -> Self {
        ApiError::from(Fail::Work(e))
    }
}

impl From<PluginError> for ApiError {
    fn from(e: PluginError) -> Self {
        ApiError::from(Fail::Plugin(e))
    }
}

/// Runs the work of the plugins off the threads that answer requests (a plan can take a while).
async fn run<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, Fail> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", ""))?
        .map_err(Into::into)
}

fn bad_app() -> ApiError {
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "bad_app", "")
}

// ---- plugins and machines ---------------------------------------------------------------------------------------------------------------

async fn list_plugins(State(s): State<Shared>, Authed(who, _): Authed) -> ApiResult {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "machines:view")?;
    let list: Vec<Value> = s
        .plugins
        .all()
        .iter()
        .map(|p| {
            json!({
                "kind": p.kind(), "title": p.title(), "capabilities": p.capabilities(),
                "manages_machines": p.manages_machines(),
            })
        })
        .collect();
    Ok(Json(json!({ "plugins": list })))
}

async fn list_machines(State(s): State<Shared>, Authed(who, _): Authed) -> ApiResult {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "machines:view")?;
    let state = s.clone();
    let machines = run(move || Ok(state.work_machines_view())).await?;
    Ok(Json(json!({ "machines": machines })))
}

async fn get_machine(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "machines:view")?;
    let state = s.clone();
    let found = run(move || Ok(state.work_machines_view())).await?;
    found
        .into_iter()
        .find(|m| m["id"] == id.as_str())
        .map(Json)
        .ok_or_else(|| WError::UnknownMachine.into())
}

#[derive(Deserialize)]
struct NewMachine {
    id: String,
    name: String,
    plugin: String,
    #[serde(default)]
    settings: Option<Value>,
}

/// The settings of a machine: nothing, or an object that the plugin accepts.
fn settings_of(s: &Shared, plugin: &str, settings: Option<Value>) -> Result<Value, Fail> {
    let plugin = s.plugins.get(plugin).ok_or(WError::UnknownPlugin)?;
    let settings = match settings {
        None | Some(Value::Null) => json!({}),
        Some(v) if v.is_object() => v,
        Some(_) => return Err(WError::BadSettings.into()),
    };
    plugin.check_settings(&settings)?;
    Ok(settings)
}

async fn add_machine(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    ApiJson(b): ApiJson<NewMachine>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    crate::api::need(&who, Role::Admin)?;
    crate::api::need_scope(&who, "machines:manage")?;
    if !valid_id(&b.id) {
        return Err(WError::BadId.into());
    }
    if !crate::apps::valid_label(&b.name, 80) {
        return Err(WError::BadName.into());
    }
    let plugin = s.plugins.get(&b.plugin).ok_or(WError::UnknownPlugin)?;
    if !plugin.manages_machines() {
        return Err(PluginError::NotSupported(
            "the machines of this plugin are made elsewhere (bungard: POST /api/v1/machines)",
        )
        .into());
    }
    let settings = settings_of(&s, &b.plugin, b.settings)?;
    let state = s.clone();
    let id = b.id.clone();
    let taken = run(move || Ok(state.work_find(&id).is_some())).await?;
    if taken {
        return Err(WError::MachineExists.into());
    }
    let machine = WorkMachine {
        id: b.id,
        name: b.name.trim().to_string(),
        plugin: b.plugin,
        settings,
        created: now(),
    };
    s.work.machines.locked().add(machine.clone())?;
    crate::api::note(
        &s,
        "work_machine_create",
        &who,
        &[("machine", &machine.id), ("plugin", &machine.plugin)],
    );
    s.announce_machine("machine.created", &machine.id, &machine.name);
    Ok((StatusCode::CREATED, Json(machine.view())))
}

#[derive(Deserialize)]
struct MachineChange {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    settings: Option<Value>,
}

async fn change_machine(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
    ApiJson(b): ApiJson<MachineChange>,
) -> ApiResult {
    crate::api::need(&who, Role::Admin)?;
    crate::api::need_scope(&who, "machines:manage")?;
    let current = s
        .work
        .machines
        .locked()
        .get(&id)
        .cloned()
        .ok_or(WError::UnknownMachine)?;
    let name = match b.name {
        Some(n) if crate::apps::valid_label(&n, 80) => Some(n.trim().to_string()),
        Some(_) => return Err(WError::BadName.into()),
        None => None,
    };
    let settings = match b.settings {
        Some(v) => Some(settings_of(&s, &current.plugin, Some(v))?),
        None => None,
    };
    let machine = s.work.machines.locked().edit(&id, name, settings)?;
    crate::api::note(&s, "work_machine_change", &who, &[("machine", &id)]);
    s.announce_machine("machine.changed", &machine.id, &machine.name);
    Ok(Json(machine.view()))
}

async fn remove_machine(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Admin)?;
    crate::api::need_scope(&who, "machines:manage")?;
    let name = s
        .work
        .machines
        .locked()
        .get(&id)
        .map(|m| m.name.clone())
        .ok_or(WError::UnknownMachine)?;
    let open = s
        .work
        .jobs
        .locked()
        .list()
        .iter()
        .any(|j| j.machine == id && !j.state.finished());
    if open {
        return Err(WError::MachineBusy.into());
    }
    s.work.machines.locked().remove(&id)?;
    crate::api::note(&s, "work_machine_remove", &who, &[("machine", &id)]);
    s.announce_machine("machine.removed", &id, &name);
    Ok(Json(json!({ "ok": true })))
}

// ---- the queue --------------------------------------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct NewJob {
    machine: String,
    request: Value,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    app: Option<String>,
}

fn job_view(s: &AppState, job: &crate::work::WorkJob, full: bool) -> Value {
    let position = s.work.jobs.locked().position(job);
    let mut v = if full { job.full() } else { job.summary() };
    v["position"] = json!(position);
    v
}

async fn create_job(
    State(s): State<Shared>,
    Authed(who, token): Authed,
    ApiJson(b): ApiJson<NewJob>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "jobs:prepare")?;
    // the application that asks: the one it says, else the one of its login
    let app = match b.app {
        Some(a) if crate::apps::allowed(&s.cfg, &a) => a,
        Some(_) => return Err(bad_app()),
        None => s.auth.locked().app_of(&token).unwrap_or_default(),
    };
    let state = s.clone();
    let (machine, name, request, by) = (b.machine, b.name, b.request, who.clone());
    let created = {
        let app = app.clone();
        run(move || state.work_create(&by, &machine, name.as_deref(), &app, &request)).await?
    };
    crate::api::note(
        &s,
        "work_job_create",
        &who,
        &[("job", &created.id), ("machine", &created.machine)],
    );
    Ok((StatusCode::CREATED, Json(job_view(&s, &created, true))))
}

#[derive(Deserialize)]
struct JobFilter {
    machine: Option<String>,
    state: Option<String>,
    app: Option<String>,
    mine: Option<bool>,
    limit: Option<usize>,
    offset: Option<usize>,
}

async fn list_jobs(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Query(f): Query<JobFilter>,
) -> ApiResult {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "machines:view")?;
    let state = s.clone();
    run(move || {
        state.work_refresh_all();
        Ok(())
    })
    .await?;
    let limit = f.limit.unwrap_or(50).clamp(1, 500);
    let offset = f.offset.unwrap_or(0);
    let me = who.owner();
    let book = s.work.jobs.locked();
    let found: Vec<&crate::work::WorkJob> = book
        .list()
        .iter()
        .rev()
        .filter(|j| f.machine.as_deref().is_none_or(|m| j.machine == m))
        .filter(|j| f.state.as_deref().is_none_or(|st| j.state.name() == st))
        .filter(|j| f.app.as_deref().is_none_or(|a| j.app == a))
        .filter(|j| !f.mine.unwrap_or(false) || j.owner == me)
        .collect();
    let total = found.len();
    let items: Vec<Value> = found
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|j| {
            let mut v = j.summary();
            v["position"] = json!(book.position(j));
            v
        })
        .collect();
    Ok(Json(
        json!({ "total": total, "offset": offset, "limit": limit, "items": items }),
    ))
}

async fn get_job(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "machines:view")?;
    let state = s.clone();
    let wanted = id.clone();
    let job = run(move || {
        state
            .work_refresh_one(&wanted)
            .ok_or_else(|| Fail::Work(WError::UnknownJob))
    })
    .await?;
    Ok(Json(job_view(&s, &job, true)))
}

async fn start_job(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "jobs:start")?;
    let state = s.clone();
    let (wanted, by) = (id.clone(), who.clone());
    let job = run(move || state.work_start(&wanted, &by)).await?;
    crate::api::note(&s, "work_job_start", &who, &[("job", &id)]);
    Ok(Json(job_view(&s, &job, true)))
}

async fn pause_job(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "jobs:start")?;
    let state = s.clone();
    let (wanted, by) = (id.clone(), who.clone());
    let job = run(move || state.work_pause(&wanted, &by)).await?;
    crate::api::note(&s, "work_job_pause", &who, &[("job", &id)]);
    Ok(Json(job_view(&s, &job, true)))
}

async fn resume_job(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "jobs:start")?;
    let state = s.clone();
    let (wanted, by) = (id.clone(), who.clone());
    let job = run(move || state.work_resume(&wanted, &by)).await?;
    crate::api::note(&s, "work_job_resume", &who, &[("job", &id)]);
    Ok(Json(job_view(&s, &job, true)))
}

async fn cancel_job(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "jobs:cancel")?;
    let state = s.clone();
    let (wanted, by) = (id.clone(), who.clone());
    let job = run(move || state.work_cancel(&wanted, &by)).await?;
    crate::api::note(&s, "work_job_cancel", &who, &[("job", &id)]);
    Ok(Json(job_view(&s, &job, true)))
}

async fn remove_job(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "jobs:cancel")?;
    s.work_remove(&id)?;
    crate::api::note(&s, "work_job_remove", &who, &[("job", &id)]);
    Ok(Json(json!({ "ok": true })))
}

// ---- the stream of events ---------------------------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct StreamQuery {
    token: Option<String>,
    ticket: Option<String>,
    machine: Option<String>,
    project: Option<String>,
    job: Option<String>,
    app: Option<String>,
    /// Kinds separated by commas; `job` also matches `job.state`.
    kinds: Option<String>,
    last_event_id: Option<u64>,
}

/// How long the stream waits in silence before it asks whether the login is still good.
const RECHECK: Duration = Duration::from_secs(30);

struct Live {
    rx: broadcast::Receiver<FeedEvent>,
    backlog: VecDeque<FeedEvent>,
    /// The number of the last event sent: the ones at or below it are not sent twice.
    high: u64,
    hello: Option<String>,
    who: Identity,
    token: String,
    filter: Filter,
    state: Shared,
    _place: ClientGuard,
}

impl Live {
    fn allowed(&self, e: &FeedEvent) -> bool {
        self.filter.passes(e)
            && e.visible_to(&self.who)
            && e.scope().is_none_or(|scope| self.who.allows(scope))
    }
}

fn sse_event(e: &FeedEvent) -> Event {
    Event::default()
        .id(e.id.to_string())
        .event(e.kind.clone())
        .data(e.text())
}

async fn stream_events(
    State(s): State<Shared>,
    parts: Parts,
    Query(q): Query<StreamQuery>,
) -> Result<Response, ApiError> {
    let (who, token) = identify_open(
        &s,
        &parts,
        &TokenQuery {
            token: q.token.clone(),
            ticket: q.ticket.clone(),
        },
    )?;
    let place = s.feed.join().ok_or_else(|| {
        ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_clients",
            "as many clients listen as events_clients_max allows",
        )
    })?;
    let last = parts
        .headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .or(q.last_event_id);
    let filter = Filter {
        machine: q.machine,
        project: q.project,
        job: q.job,
        app: q.app,
        kinds: q
            .kinds
            .as_deref()
            .unwrap_or("")
            .split(',')
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(str::to_string)
            .collect(),
    };
    // listen first, then ask for what was missed: nothing falls between the two, and the number of the last event sent keeps a repeat out
    let rx = s.feed.subscribe();
    let newest = s.feed.last_id();
    let mut live = Live {
        rx,
        backlog: VecDeque::new(),
        high: last.unwrap_or(newest),
        hello: None,
        who,
        token,
        filter,
        state: s.clone(),
        _place: place,
    };
    let mut gap = false;
    if let Some(last) = last {
        let kept = s.feed.since(last);
        gap = last < newest && kept.first().is_none_or(|e| e.id > last + 1);
        for e in kept {
            if live.allowed(&e) {
                live.backlog.push_back(e);
            }
        }
    }
    live.hello = Some(
        json!({
            "last_id": newest, "resumed": last.is_some(), "resync": gap,
            "clients": s.feed.clients(),
        })
        .to_string(),
    );
    let stream = futures_util::stream::unfold(live, |mut live| async move {
        loop {
            if let Some(text) = live.hello.take() {
                let event = Event::default().event("hello").data(text);
                return Some((Ok::<Event, Infallible>(event), live));
            }
            if let Some(e) = live.backlog.pop_front() {
                live.high = live.high.max(e.id);
                return Some((Ok(sse_event(&e)), live));
            }
            match tokio::time::timeout(RECHECK, live.rx.recv()).await {
                // quiet for a while: is the login still good?
                Err(_) => {
                    let still = live
                        .state
                        .auth
                        .locked()
                        .identify(&live.token, now())
                        .is_some();
                    if !still {
                        return None;
                    }
                }
                Ok(Ok(e)) => {
                    if e.id > live.high && live.allowed(&e) {
                        live.high = e.id;
                        return Some((Ok(sse_event(&e)), live));
                    }
                }
                Ok(Err(RecvError::Lagged(missed))) => {
                    let event = Event::default()
                        .event("resync")
                        .data(json!({ "missed": missed }).to_string());
                    return Some((Ok(event), live));
                }
                Ok(Err(RecvError::Closed)) => return None,
            }
        }
    });
    let keep_alive = KeepAlive::new()
        .interval(Duration::from_secs(s.cfg.events_ping_s))
        .text("ping");
    Ok(Sse::new(stream).keep_alive(keep_alive).into_response())
}

// ---- the notifications ------------------------------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct NoteQuery {
    unread: Option<bool>,
    limit: Option<usize>,
}

async fn list_notifications(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Query(q): Query<NoteQuery>,
) -> ApiResult {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "machines:view")?;
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let list = s
        .work
        .notes
        .locked()
        .for_user(&who, q.unread.unwrap_or(false), limit);
    Ok(Json(json!({ "notifications": list })))
}

#[derive(Deserialize)]
struct ReadBody {
    #[serde(default)]
    ids: Vec<u64>,
    #[serde(default)]
    all: bool,
}

async fn read_notifications(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    ApiJson(b): ApiJson<ReadBody>,
) -> ApiResult {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "machines:view")?;
    let marked = s.work.notes.locked().mark_read(&who, &b.ids, b.all);
    Ok(Json(json!({ "marked": marked })))
}
