//! The nodes of a pick-and-place machine (EH3D-NODE-PNP-FEEDER and EH3D-NODE-PNP-HEAD) seen through the server: the server keeps a list of them (the address and the token
//! of each, and the pick-and-place machine each one belongs to), asks them for their status and tells every client what they are doing, in `GET /api/v1/pnp/nodes` and in
//! the fleet.
//!
//! The nodes speak HTTP and JSON (their `docs/CONTRACT.md`): `GET /api/status` answers with or without the token, and a node that has not heard a request with its token for a
//! while goes to its safe state by itself, so the server asks something with the token on every poll (a read of the first feeder or of the first nozzle). The server keeps
//! nothing of a node but what it said last, and **degrades towards what is safer**: a node that did not answer for `pnp_node_silence_s`, that refuses the token, that says it is
//! safe or that fails its own check is not ready; so is an answer that is not what the contract says. A machine linked to a node that is not ready does not start a job.
//!
//! Nothing here has run against a real feeder or head: the tests run it against servers in memory that answer the words of the contract.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::config::ServerConfig;
use crate::state::AppState;
use crate::store;
use crate::sync::Locked;

/// The kinds of node.
pub const KINDS: &[&str] = &["feeder", "head"];
/// What stands for a token in everything the server shows.
pub const SECRET_MARK: &str = "********";
/// The most feeders and nozzles the summary of a node keeps.
const MAX_FEEDERS: usize = 64;
const MAX_NOZZLES: usize = 16;
/// The longest text of a node that is kept.
const MAX_TEXT: usize = 64;
/// The most a status may weigh (the node's is a line of JSON).
const MAX_STATUS_BYTES: usize = 48 * 1024;
const MAX_NAME: usize = 64;
const MAX_TOKEN: usize = 200;
/// How long without hearing the server before a node goes to its safe state, as its contract sets it by default: `link_lost_ms` of the head and `server_lost_ms` of the
/// feeder. Used while a node does not say its own in its status.
pub const HEAD_LINK_LOST_MS: u64 = 2_000;
pub const FEEDER_LINK_LOST_MS: u64 = 15_000;
/// The most questions to one node that wait for their answer at once: a slow answer does not stop the next question, and a node that never answers is not flooded.
const MAX_IN_FLIGHT: usize = 3;

/// The key of the status in which a node of this kind says its own time.
fn link_lost_key(kind: &str) -> &'static str {
    if kind == "feeder" {
        "server_lost_ms"
    } else {
        "link_lost_ms"
    }
}

/// The time a node of this kind is taken to have while it does not say it.
pub fn assumed_link_lost_ms(kind: &str) -> u64 {
    if kind == "feeder" {
        FEEDER_LINK_LOST_MS
    } else {
        HEAD_LINK_LOST_MS
    }
}

/// Whether asking every `poll_s` seconds is too slow for a node that goes safe after `link_lost_ms` without being asked.
pub fn poll_too_slow(poll_s: u64, link_lost_ms: u64) -> bool {
    poll_s.saturating_mul(1_000) >= link_lost_ms
}

/// The limits and the times, from `server.cfg`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    pub enabled: bool,
    pub max: usize,
    pub poll_s: u64,
    pub silence_s: u64,
    pub timeout_s: u64,
}

impl Limits {
    pub fn from_config(c: &ServerConfig) -> Limits {
        Limits {
            enabled: c.pnp_nodes_enabled,
            max: c.pnp_nodes_max as usize,
            poll_s: c.pnp_node_poll_s,
            silence_s: c.pnp_node_silence_s,
            timeout_s: c.pnp_node_timeout_s,
        }
    }

    /// How long a node may be quiet before it counts as silent: never less than two polls, so that one lost answer is not silence.
    pub fn silence(&self) -> Duration {
        Duration::from_secs(self.silence_s.max(self.poll_s.saturating_mul(2)))
    }
}

/// What the server keeps of a node.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeRecord {
    pub id: String,
    pub kind: String,
    pub name: String,
    /// `host:port` of the node.
    pub address: String,
    /// The token of the node; empty when it has none. Never shown.
    #[serde(default)]
    pub token: String,
    /// The pick-and-place machine (a machine of the plugin `pnp`) the node belongs to; empty when it is not linked.
    #[serde(default)]
    pub machine: String,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, PartialEq, Eq)]
pub enum NodeError {
    Off,
    Unknown,
    Bad(String),
    Exists,
    Full,
    Io,
}

impl NodeError {
    pub fn code(&self) -> &'static str {
        match self {
            NodeError::Off => "pnp_nodes_off",
            NodeError::Unknown => "unknown_pnp_node",
            NodeError::Bad(_) => "bad_pnp_node",
            NodeError::Exists => "pnp_node_exists",
            NodeError::Full => "pnp_nodes_full",
            NodeError::Io => "internal",
        }
    }

    pub fn status(&self) -> u16 {
        match self {
            NodeError::Off => 501,
            NodeError::Unknown => 404,
            NodeError::Bad(_) => 422,
            NodeError::Exists | NodeError::Full => 409,
            NodeError::Io => 500,
        }
    }

    pub fn detail(&self) -> String {
        match self {
            NodeError::Off => "the nodes of the pick-and-place machines are off on this server (pnp_nodes_enabled)".into(),
            NodeError::Unknown => "there is no such node".into(),
            NodeError::Bad(why) => why.clone(),
            NodeError::Exists => "a node with this id or this address is already registered".into(),
            NodeError::Full => "the list of nodes is full (pnp_nodes_max)".into(),
            NodeError::Io => "the list of nodes could not be written".into(),
        }
    }
}

/// What the last polls said of a node. Kept in memory only.
#[derive(Clone, Debug, Default)]
struct Seen {
    first_try: Option<Instant>,
    last_try: Option<Instant>,
    last_ok: Option<Instant>,
    last_failed: bool,
    ok: u64,
    failed: u64,
    error: String,
    summary: Option<Value>,
    /// When the question whose answer is kept was sent: the answer to an earlier one that comes later is old news.
    asked: Option<Instant>,
    /// The time without the server after which the node said it goes safe, when it says it.
    link_lost_ms: Option<u64>,
}

struct Inner {
    records: BTreeMap<String, NodeRecord>,
    seen: HashMap<String, Seen>,
}

/// The nodes registered on this server.
pub struct PnpNodes {
    path: PathBuf,
    pub limits: Limits,
    inner: Mutex<Inner>,
}

/// What a poll found: the summary of the status, or why it failed. A summary comes with an error when the status was good but the token was refused.
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub summary: Option<Value>,
    pub error: Option<String>,
}

impl Outcome {
    pub fn failed(why: &str) -> Outcome {
        Outcome {
            summary: None,
            error: Some(why.to_string()),
        }
    }
}

/// How the quiet or the answer of a node reads, from the facts. The order of the rules is the safe one: a node that is not heard is silent whatever it said last.
#[allow(clippy::too_many_arguments)]
pub fn health(
    enabled: bool,
    tried: bool,
    quiet_for: Option<Duration>,
    silence: Duration,
    last_failed: bool,
    error: &str,
    summary: Option<&Value>,
) -> &'static str {
    if !enabled {
        return "disabled";
    }
    if !tried {
        return "unknown";
    }
    if quiet_for.is_none_or(|q| q > silence) {
        return "silent";
    }
    if last_failed {
        return if error == "token_rejected" {
            "refused"
        } else {
            "unreachable"
        };
    }
    let Some(sum) = summary else {
        return "unreachable";
    };
    if sum["safe"].as_bool() != Some(false) {
        return "safe";
    }
    match sum["selfcheck"].as_str() {
        Some("ok") => "ok",
        Some("warn") => "warn",
        _ => "fail",
    }
}

/// Whether a node that is in this health may be relied on.
pub fn ready(health: &str) -> bool {
    matches!(health, "ok" | "warn")
}

fn clean(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(|s| {
        s.chars()
            .filter(|c| !c.is_control())
            .take(MAX_TEXT)
            .collect::<String>()
    })
}

fn uint(v: &Value, key: &str, max: u64) -> Option<u64> {
    v.get(key).and_then(Value::as_u64).map(|n| n.min(max))
}

fn put(out: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        out.insert(key.to_string(), value);
    }
}

/// The summary of a status answer of a node of this kind: only the keys of the contract, with their types checked and their sizes bound. An answer that is not an object,
/// that does not say whether the node is safe or that has a verdict of its own check the contract does not have is refused (`bad_answer`): a node that cannot be read is
/// not trusted.
pub fn parse_status(kind: &str, body: &[u8]) -> Result<Value, &'static str> {
    if body.len() > MAX_STATUS_BYTES {
        return Err("too_big");
    }
    let v: Value = serde_json::from_slice(body).map_err(|_| "bad_answer")?;
    if !v.is_object() {
        return Err("bad_answer");
    }
    let safe = v["safe"].as_bool().ok_or("bad_answer")?;
    let selfcheck = match v["selfcheck"].as_str() {
        Some(s @ ("ok" | "warn" | "fail")) => s,
        _ => return Err("bad_answer"),
    };
    let mut o = Map::new();
    o.insert("safe".into(), json!(safe));
    o.insert("selfcheck".into(), json!(selfcheck));
    for key in ["node", "version", "safe_reason", "selfcheck_codes"] {
        put(&mut o, key, clean(&v, key).map(Value::from));
    }
    put(
        &mut o,
        "uptime_s",
        uint(&v, "uptime_s", u64::from(u32::MAX)).map(Value::from),
    );
    let key = link_lost_key(kind);
    put(
        &mut o,
        key,
        uint(&v, key, u64::from(u32::MAX)).map(Value::from),
    );
    match kind {
        "feeder" => {
            put(&mut o, "bus", clean(&v, "bus").map(Value::from));
            put(&mut o, "slots", uint(&v, "slots", 255).map(Value::from));
            put(&mut o, "online", uint(&v, "online", 255).map(Value::from));
            let (mut low, mut empty, mut faulted) = (0u64, 0u64, 0u64);
            let mut list = Vec::new();
            for f in v["feeders"]
                .as_array()
                .into_iter()
                .flatten()
                .take(MAX_FEEDERS)
            {
                let state = clean(f, "state").unwrap_or_default();
                let fault = clean(f, "fault").unwrap_or_default();
                low += u64::from(f["low"].as_bool() == Some(true));
                empty += u64::from(state == "empty");
                faulted += u64::from(!fault.is_empty() && fault != "none");
                list.push(json!({
                    "slot": uint(f, "slot", 255), "online": f["online"].as_bool(), "state": state,
                    "fault": fault, "remaining": uint(f, "remaining", 65_535), "low": f["low"].as_bool(),
                }));
            }
            o.insert("feeders".into(), Value::Array(list));
            o.insert("low".into(), json!(low));
            o.insert("empty".into(), json!(empty));
            o.insert("faulted".into(), json!(faulted));
        }
        _ => {
            put(&mut o, "safe_code", clean(&v, "safe_code").map(Value::from));
            put(
                &mut o,
                "nozzle_count",
                uint(&v, "nozzle_count", 255).map(Value::from),
            );
            for key in ["pump", "xy_clear", "xy_moving", "calibrated"] {
                put(&mut o, key, v[key].as_bool().map(Value::from));
            }
            put(
                &mut o,
                "xy_blocking",
                clean(&v, "xy_blocking").map(Value::from),
            );
            put(
                &mut o,
                "z_safe_mm",
                v["z_safe_mm"]
                    .as_f64()
                    .filter(|z| z.is_finite())
                    .map(Value::from),
            );
            let mut faulted = 0u64;
            let mut list = Vec::new();
            for n in v["nozzles"]
                .as_array()
                .into_iter()
                .flatten()
                .take(MAX_NOZZLES)
            {
                let fault = clean(n, "fault").unwrap_or_default();
                faulted += u64::from(!fault.is_empty() && fault != "none");
                list.push(json!({
                    "n": uint(n, "n", 255), "phase": clean(n, "phase"), "fault": fault, "alarm": clean(n, "alarm"),
                    "part": n["part"].as_bool(), "vacuum_ok": n["vacuum_ok"].as_bool(), "raised": n["raised"].as_bool(),
                    "kpa": n["kpa"].as_f64().filter(|k| k.is_finite()),
                }));
            }
            o.insert("nozzles".into(), Value::Array(list));
            o.insert("faulted".into(), json!(faulted));
        }
    }
    Ok(Value::Object(o))
}

/// The check of what a person typed for a node: the kind, the name, the address, the token and the machine link are looked at by the caller where they need the state.
pub fn check_record(r: &NodeRecord) -> Result<(), NodeError> {
    let bad = |why: &str| Err(NodeError::Bad(why.to_string()));
    if !KINDS.contains(&r.kind.as_str()) {
        return bad("the kind of a node is feeder or head");
    }
    if r.id.is_empty()
        || r.id.len() > 32
        || !r
            .id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return bad("the id of a node is made of lowercase letters, digits and hyphens (up to 32)");
    }
    if r.name.trim().is_empty()
        || r.name.chars().count() > MAX_NAME
        || r.name.chars().any(char::is_control)
    {
        return bad("the name of a node is 1 to 64 characters with no control characters");
    }
    if crate::head::host_port(&r.address).is_err()
        || r.address.chars().any(|c| c.is_control() || c == '@')
    {
        return bad("the address of a node is host:port");
    }
    if r.token.len() > MAX_TOKEN || r.token.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return bad(
            "the token of a node has no spaces or control characters and is at most 200 characters",
        );
    }
    Ok(())
}

impl PnpNodes {
    pub fn open(cfg: &ServerConfig) -> io::Result<PnpNodes> {
        let path = cfg.data_dir.join("pnp-nodes.json");
        let list: Vec<NodeRecord> = store::read_json(&path)?.unwrap_or_default();
        Ok(PnpNodes {
            path,
            limits: Limits::from_config(cfg),
            inner: Mutex::new(Inner {
                records: list.into_iter().map(|r| (r.id.clone(), r)).collect(),
                seen: HashMap::new(),
            }),
        })
    }

    /// Whether the file of the nodes in a data folder reads (for `--check`).
    pub fn file_reads(dir: &Path) -> io::Result<usize> {
        let list: Option<Vec<NodeRecord>> = store::read_json(&dir.join("pnp-nodes.json"))?;
        Ok(list.map_or(0, |l| l.len()))
    }

    fn save(&self, inner: &Inner) -> Result<(), NodeError> {
        let list: Vec<&NodeRecord> = inner.records.values().collect();
        store::write_json(&self.path, &list).map_err(|_| NodeError::Io)
    }

    pub fn len(&self) -> usize {
        self.inner.locked().records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Registers a node. An empty id gets the first free `<kind>-<n>`.
    pub fn add(&self, mut r: NodeRecord) -> Result<NodeRecord, NodeError> {
        let mut inner = self.inner.locked();
        if r.id.is_empty() {
            r.id = (1..)
                .map(|n| format!("{}-{n}", r.kind))
                .find(|id| !inner.records.contains_key(id))
                .unwrap_or_default();
        }
        check_record(&r)?;
        if inner.records.len() >= self.limits.max {
            return Err(NodeError::Full);
        }
        let host = crate::head::host_port(&r.address).unwrap_or_default();
        let same_address =
            |o: &NodeRecord| crate::head::host_port(&o.address).is_ok_and(|h| h == host);
        if inner.records.contains_key(&r.id) || inner.records.values().any(same_address) {
            return Err(NodeError::Exists);
        }
        inner.records.insert(r.id.clone(), r.clone());
        if let Err(e) = self.save(&inner) {
            inner.records.remove(&r.id);
            return Err(e);
        }
        Ok(r)
    }

    /// Changes the name, the address, the token, the machine or the switch of a node; what is `None` stays.
    pub fn update(
        &self,
        id: &str,
        name: Option<String>,
        address: Option<String>,
        token: Option<String>,
        machine: Option<String>,
        enabled: Option<bool>,
    ) -> Result<NodeRecord, NodeError> {
        let mut inner = self.inner.locked();
        let before = inner.records.get(id).cloned().ok_or(NodeError::Unknown)?;
        let mut r = before.clone();
        if let Some(v) = name {
            r.name = v;
        }
        if let Some(v) = address {
            r.address = v;
        }
        if let Some(v) = token {
            // the stars that were shown mean "keep the token"
            if v != SECRET_MARK {
                r.token = v;
            }
        }
        if let Some(v) = machine {
            r.machine = v;
        }
        if let Some(v) = enabled {
            r.enabled = v;
        }
        check_record(&r)?;
        let host = crate::head::host_port(&r.address).unwrap_or_default();
        let clash = inner
            .records
            .values()
            .any(|o| o.id != r.id && crate::head::host_port(&o.address).is_ok_and(|h| h == host));
        if clash {
            return Err(NodeError::Exists);
        }
        inner.records.insert(id.to_string(), r.clone());
        if let Err(e) = self.save(&inner) {
            inner.records.insert(id.to_string(), before);
            return Err(e);
        }
        // what was seen of another address or token says nothing of the new one
        if r.address != before.address || r.token != before.token || r.enabled != before.enabled {
            inner.seen.remove(id);
        }
        Ok(r)
    }

    pub fn remove(&self, id: &str) -> Result<(), NodeError> {
        let mut inner = self.inner.locked();
        let before = inner.records.remove(id).ok_or(NodeError::Unknown)?;
        if let Err(e) = self.save(&inner) {
            inner.records.insert(id.to_string(), before);
            return Err(e);
        }
        inner.seen.remove(id);
        Ok(())
    }

    /// The address, the token and the kind of a node, to ask it.
    pub fn target(&self, id: &str) -> Option<NodeRecord> {
        self.inner.locked().records.get(id).cloned()
    }

    /// The nodes the poller asks: the ones that are switched on.
    pub fn targets(&self) -> Vec<NodeRecord> {
        self.inner
            .locked()
            .records
            .values()
            .filter(|r| r.enabled)
            .cloned()
            .collect()
    }

    /// Writes down what a poll found, at `now`. Gives the health before and after, so that a change can be told.
    pub fn record(
        &self,
        id: &str,
        outcome: Outcome,
        now: Instant,
    ) -> Option<(&'static str, &'static str)> {
        self.record_asked(id, outcome, now, now)
    }

    /// The same for a question sent at `asked`: several may wait at once, and the answer to one sent before the one already kept changes nothing.
    pub fn record_asked(
        &self,
        id: &str,
        outcome: Outcome,
        asked: Instant,
        now: Instant,
    ) -> Option<(&'static str, &'static str)> {
        let mut inner = self.inner.locked();
        let rec = inner.records.get(id)?.clone();
        let before = Self::health_of(&rec, inner.seen.get(id), now, self.limits.silence());
        let poll_s = self.limits.poll_s;
        let seen = inner.seen.entry(id.to_string()).or_default();
        if seen.asked.is_some_and(|kept| asked < kept) {
            return Some((before, before));
        }
        seen.asked = Some(asked);
        let said = outcome
            .summary
            .as_ref()
            .and_then(|sum| sum[link_lost_key(&rec.kind)].as_u64());
        if let Some(ms) = said {
            if seen.link_lost_ms != Some(ms) && poll_too_slow(poll_s, ms) {
                eprintln!(
                    "eh3d-server: warning: the node {id} goes to its safe state after {ms} ms without being asked and it is asked every {poll_s} s (pnp_node_poll_s)"
                );
            }
            seen.link_lost_ms = Some(ms);
        }
        seen.first_try.get_or_insert(now);
        seen.last_try = Some(now);
        match &outcome.error {
            None => {
                seen.last_ok = Some(now);
                seen.last_failed = false;
                seen.ok += 1;
                seen.error.clear();
            }
            Some(why) => {
                seen.last_failed = true;
                seen.failed += 1;
                seen.error = why.clone();
            }
        }
        if let Some(sum) = outcome.summary {
            seen.summary = Some(sum);
        } else if outcome.error.is_some() {
            // an answer that was not good is not kept as what the node said
            seen.summary = None;
        }
        // a status that was good but whose token was refused counts as an answer heard, but the node will not trust this server: it is not "ok"
        let after = Self::health_of(&rec, inner.seen.get(id), now, self.limits.silence());
        Some((before, after))
    }

    fn health_of(
        r: &NodeRecord,
        seen: Option<&Seen>,
        now: Instant,
        silence: Duration,
    ) -> &'static str {
        let empty = Seen::default();
        let s = seen.unwrap_or(&empty);
        let quiet_for = s
            .last_ok
            .or(s.first_try)
            .map(|t| now.saturating_duration_since(t));
        health(
            r.enabled,
            s.last_try.is_some(),
            quiet_for,
            silence,
            s.last_failed,
            &s.error,
            s.summary.as_ref(),
        )
    }

    fn view_of(&self, inner: &Inner, r: &NodeRecord, now: Instant) -> Value {
        let seen = inner.seen.get(&r.id);
        let h = Self::health_of(r, seen, now, self.limits.silence());
        let empty = Seen::default();
        let s = seen.unwrap_or(&empty);
        let link_lost_ms = s
            .link_lost_ms
            .unwrap_or_else(|| assumed_link_lost_ms(&r.kind));
        json!({
            "link_lost_ms": link_lost_ms,
            "link_lost_declared": s.link_lost_ms.is_some(),
            "poll_too_slow": poll_too_slow(self.limits.poll_s, link_lost_ms),
            "id": r.id, "kind": r.kind, "name": r.name, "address": r.address,
            "token": if r.token.is_empty() { "" } else { SECRET_MARK },
            "machine": r.machine, "enabled": r.enabled,
            "health": h, "ready": ready(h),
            "silent_for_s": s.last_ok.or(s.first_try).map(|t| now.saturating_duration_since(t).as_secs()),
            "polls": { "ok": s.ok, "failed": s.failed },
            "last_error": s.error,
            "status": s.summary,
            "evidence": "simulator", "simulator_only": true,
        })
    }

    pub fn list_views(&self, now: Instant) -> Vec<Value> {
        let inner = self.inner.locked();
        inner
            .records
            .values()
            .map(|r| self.view_of(&inner, r, now))
            .collect()
    }

    pub fn view(&self, id: &str, now: Instant) -> Option<Value> {
        let inner = self.inner.locked();
        inner.records.get(id).map(|r| self.view_of(&inner, r, now))
    }

    /// The views of the nodes registered for a machine.
    pub fn views_for(&self, machine: &str, now: Instant) -> Vec<Value> {
        let inner = self.inner.locked();
        inner
            .records
            .values()
            .filter(|r| r.machine == machine)
            .map(|r| self.view_of(&inner, r, now))
            .collect()
    }

    /// The nodes of a machine that are not ready, as `(id, health)`. A node that is switched off does not hold the machine.
    pub fn not_ready_for(&self, machine: &str, now: Instant) -> Vec<(String, &'static str)> {
        if machine.is_empty() {
            return Vec::new();
        }
        let inner = self.inner.locked();
        inner
            .records
            .values()
            .filter(|r| r.machine == machine && r.enabled)
            .filter_map(|r| {
                let h = Self::health_of(r, inner.seen.get(&r.id), now, self.limits.silence());
                (!ready(h)).then(|| (r.id.clone(), h))
            })
            .collect()
    }

    /// What the nodes that are switched on make doubtful about the settings: for now, only that they are asked too slowly to stay out of their safe state.
    pub fn warnings(&self) -> Vec<Value> {
        let inner = self.inner.locked();
        let poll_s = self.limits.poll_s;
        inner
            .records
            .values()
            .filter(|r| r.enabled)
            .filter_map(|r| {
                let declared = inner.seen.get(&r.id).and_then(|s| s.link_lost_ms);
                let ms = declared.unwrap_or_else(|| assumed_link_lost_ms(&r.kind));
                poll_too_slow(poll_s, ms).then(|| {
                    json!({
                        "code": "poll_too_slow", "node": r.id, "poll_s": poll_s, "link_lost_ms": ms,
                        "link_lost_declared": declared.is_some(),
                    })
                })
            })
            .collect()
    }

    /// What to say on the console when the server starts (nothing while the switch is off): an interval that a head with the usual time cannot live with, and every
    /// registered node it is too slow for.
    pub fn startup_warnings(&self) -> Vec<String> {
        if !self.limits.enabled {
            return Vec::new();
        }
        let poll_s = self.limits.poll_s;
        let mut lines = Vec::new();
        if poll_too_slow(poll_s, HEAD_LINK_LOST_MS) {
            lines.push(format!(
                "pnp_node_poll_s is {poll_s}: a head node goes to its safe state after link_lost_ms without being asked ({HEAD_LINK_LOST_MS} ms unless it is changed); set it to 1"
            ));
        }
        lines.extend(self.warnings().iter().map(|w| {
            format!(
                "the node {} is asked every {poll_s} s and goes to its safe state after {} ms without being asked",
                w["node"].as_str().unwrap_or_default(),
                w["link_lost_ms"]
            )
        }));
        lines
    }

    /// The numbers of the fleet: how many nodes there are and in what health.
    pub fn counts(&self, now: Instant) -> Value {
        let inner = self.inner.locked();
        let mut by: BTreeMap<&'static str, u64> = BTreeMap::new();
        let mut ready_n = 0u64;
        for r in inner.records.values() {
            let h = Self::health_of(r, inner.seen.get(&r.id), now, self.limits.silence());
            *by.entry(h).or_default() += 1;
            ready_n += u64::from(ready(h));
        }
        let total = inner.records.len() as u64;
        json!({ "registered": total, "ready": ready_n, "not_ready": total - ready_n, "by_health": by })
    }
}

/// The path of the authenticated read that stands for a request with the token: the first feeder or the first nozzle.
fn heartbeat_path(kind: &str) -> &'static str {
    if kind == "feeder" {
        "/api/feeder?slot=1"
    } else {
        "/api/nozzle?n=1"
    }
}

/// Asks one node for its status and then something with the token, and writes down what it said. Gives the health before and after.
pub async fn poll_node(st: &Arc<AppState>, id: &str) -> Option<(&'static str, &'static str)> {
    let rec = st.pnp_nodes.target(id)?;
    let limit = Duration::from_secs(st.pnp_nodes.limits.timeout_s);
    let ask = |path: &'static str, token: String| {
        let address = rec.address.clone();
        async move {
            tokio::time::timeout(limit, crate::head::fetch(&address, &token, "GET", path)).await
        }
    };
    let asked = Instant::now();
    let outcome = match ask("/api/status", rec.token.clone()).await {
        Ok(Ok((200, _, body))) => match parse_status(&rec.kind, &body) {
            Ok(summary) => {
                // the status is open; the token is proved by a read that needs it
                match ask(heartbeat_path(&rec.kind), rec.token.clone()).await {
                    Ok(Ok((401 | 403, _, _))) => Outcome {
                        summary: Some(summary),
                        error: Some("token_rejected".into()),
                    },
                    Ok(Ok(_)) => Outcome {
                        summary: Some(summary),
                        error: None,
                    },
                    _ => Outcome::failed("unreachable"),
                }
            }
            Err(why) => Outcome::failed(why),
        },
        Ok(Ok((status, _, _))) => Outcome::failed(&format!("http_{status}")),
        _ => Outcome::failed("unreachable"),
    };
    st.pnp_nodes
        .record_asked(id, outcome, asked, Instant::now())
}

/// Tells the clients that a node changed its health.
pub fn publish_change(st: &Arc<AppState>, id: &str, health: &str) {
    st.publish(json!({ "type": "pnp_node", "id": id, "health": health, "ready": ready(health) }));
}

/// The questions that wait for their answer, by node.
type Waiting = Arc<Mutex<HashMap<String, usize>>>;

/// One question that is waiting; dropped (also by a panic) it counts no more.
struct Question {
    waiting: Waiting,
    id: String,
}

impl Question {
    /// Counts one more question to the node, unless as many as allowed already wait.
    fn begin(waiting: &Waiting, id: &str) -> Option<Question> {
        let mut map = waiting.locked();
        let n = map.entry(id.to_string()).or_insert(0);
        if *n >= MAX_IN_FLIGHT {
            return None;
        }
        *n += 1;
        Some(Question {
            waiting: waiting.clone(),
            id: id.to_string(),
        })
    }
}

impl Drop for Question {
    fn drop(&mut self) {
        if let Some(n) = self.waiting.locked().get_mut(&self.id) {
            *n = n.saturating_sub(1);
        }
    }
}

/// The loop that asks every node that is switched on every `pnp_node_poll_s` seconds, while the switch is on. Each question goes on a connection of its own and does not
/// wait for the answers of the others, of the same node or of another: a head that is in the middle of a long move, a slow answer or a node that is gone never make the
/// server quiet towards a node that goes safe when it does not hear it. With the switch off it does nothing.
pub fn spawn(st: Arc<AppState>) -> Option<tokio::task::JoinHandle<()>> {
    if !st.pnp_nodes.limits.enabled {
        return None;
    }
    for line in st.pnp_nodes.startup_warnings() {
        eprintln!("eh3d-server: warning: {line}");
    }
    let every = Duration::from_secs(st.pnp_nodes.limits.poll_s);
    Some(tokio::spawn(async move {
        let waiting = Waiting::default();
        let mut tick = tokio::time::interval(every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            for r in st.pnp_nodes.targets() {
                let Some(question) = Question::begin(&waiting, &r.id) else {
                    continue;
                };
                let st = st.clone();
                tokio::spawn(async move {
                    if let Some((before, after)) = poll_node(&st, &question.id).await {
                        if before != after {
                            publish_change(&st, &question.id, after);
                        }
                    }
                });
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEEDER: &str = r#"{"node":"feeder-a","version":"0.0.1","slots":3,"online":3,"safe":false,"safe_reason":"none","bus":"rs485","uptime_s":12,"selfcheck":"ok","selfcheck_codes":"","feeders":[{"slot":1,"online":true,"state":"idle","fault":"none","remaining":40,"low":false},{"slot":2,"online":true,"state":"empty","fault":"none","remaining":0,"low":true},{"slot":3,"online":true,"state":"fault","fault":"FDR-J01","remaining":9,"low":true}]}"#;
    const HEAD: &str = r#"{"node":"head-a","version":"0.0.1","nozzle_count":2,"safe":false,"safe_reason":"none","safe_code":"none","pump":false,"xy_clear":true,"xy_moving":false,"z_safe_mm":10.0,"calibrated":false,"selfcheck":"warn","selfcheck_codes":"HDN-C01","nozzles":[{"n":1,"phase":"idle","fault":"none","alarm":"none","part":false,"vacuum_ok":false,"raised":true,"kpa":null},{"n":2,"phase":"fault","fault":"HEAD-S01","alarm":"sensor_out_of_range","part":false,"vacuum_ok":false,"raised":true,"kpa":3.5}]}"#;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn a_feeder_status_is_summed_up_with_its_low_empty_and_faulted_slots() {
        let s = parse_status("feeder", FEEDER.as_bytes()).unwrap();
        assert_eq!(
            (
                s["slots"].clone(),
                s["low"].clone(),
                s["empty"].clone(),
                s["faulted"].clone()
            ),
            (json!(3), json!(2), json!(1), json!(1))
        );
        assert_eq!(s["feeders"].as_array().unwrap().len(), 3);
        assert_eq!(s["bus"], "rs485");
    }

    #[test]
    fn a_head_status_is_summed_up_with_its_nozzles_and_faults() {
        let s = parse_status("head", HEAD.as_bytes()).unwrap();
        assert_eq!(
            (
                s["xy_clear"].clone(),
                s["faulted"].clone(),
                s["selfcheck"].clone()
            ),
            (json!(true), json!(1), json!("warn"))
        );
        assert_eq!(s["nozzles"][1]["alarm"], "sensor_out_of_range");
    }

    #[test]
    fn an_answer_that_is_not_the_contract_is_refused() {
        for body in [
            "",
            "null",
            "[]",
            "{",
            r#"{"selfcheck":"ok"}"#,
            r#"{"safe":"no","selfcheck":"ok"}"#,
            r#"{"safe":false}"#,
            r#"{"safe":false,"selfcheck":"fine"}"#,
            r#"{"safe":false,"selfcheck":3}"#,
        ] {
            assert_eq!(
                parse_status("feeder", body.as_bytes()),
                Err("bad_answer"),
                "{body}"
            );
        }
        let big = format!(
            r#"{{"safe":false,"selfcheck":"ok","pad":"{}"}}"#,
            "x".repeat(MAX_STATUS_BYTES)
        );
        assert_eq!(parse_status("head", big.as_bytes()), Err("too_big"));
    }

    #[test]
    fn the_texts_of_a_node_are_cut_and_cleaned() {
        let body = format!(
            r#"{{"safe":false,"selfcheck":"ok","node":"{}\u0007x"}}"#,
            "n".repeat(500)
        );
        let s = parse_status("feeder", body.as_bytes()).unwrap();
        let name = s["node"].as_str().unwrap();
        assert!(name.chars().count() <= MAX_TEXT && !name.chars().any(char::is_control));
    }

    #[test]
    fn health_degrades_towards_what_is_safer() {
        let ok = parse_status("feeder", FEEDER.as_bytes()).unwrap();
        let silence = secs(20);
        let h = |enabled, tried, quiet: Option<u64>, failed, error: &str, sum: Option<&Value>| {
            health(enabled, tried, quiet.map(secs), silence, failed, error, sum)
        };
        assert_eq!(h(false, true, Some(0), false, "", Some(&ok)), "disabled");
        assert_eq!(h(true, false, None, false, "", None), "unknown");
        assert_eq!(h(true, true, Some(1), false, "", Some(&ok)), "ok");
        assert_eq!(
            h(true, true, Some(21), false, "", Some(&ok)),
            "silent",
            "a quiet node is silent whatever it said last"
        );
        assert_eq!(h(true, true, None, false, "", Some(&ok)), "silent");
        assert_eq!(
            h(true, true, Some(3), true, "unreachable", None),
            "unreachable"
        );
        assert_eq!(
            h(true, true, Some(3), true, "token_rejected", Some(&ok)),
            "refused"
        );
        assert_eq!(h(true, true, Some(3), false, "", None), "unreachable");
        let mut safe = ok.clone();
        safe["safe"] = json!(true);
        assert_eq!(h(true, true, Some(1), false, "", Some(&safe)), "safe");
        let mut unsaid = ok.clone();
        unsaid.as_object_mut().unwrap().remove("safe");
        assert_eq!(
            h(true, true, Some(1), false, "", Some(&unsaid)),
            "safe",
            "a node that does not say it is not safe is taken as safe"
        );
        let mut fail = ok.clone();
        fail["selfcheck"] = json!("fail");
        assert_eq!(h(true, true, Some(1), false, "", Some(&fail)), "fail");
        for good in ["ok", "warn"] {
            assert!(ready(good));
        }
        for bad in [
            "unknown",
            "disabled",
            "silent",
            "unreachable",
            "refused",
            "safe",
            "fail",
        ] {
            assert!(!ready(bad), "{bad}");
        }
    }

    #[test]
    fn with_a_fixed_seed_no_status_makes_a_node_ready_when_it_says_it_is_not() {
        // a fixed seed: the same thousand made-up answers on every run
        let mut seed: u64 = 0x5EED_0D03;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let pieces = [
            "true", "false", "null", "1", "\"ok\"", "\"warn\"", "\"fail\"", "\"x\"", "[]", "{}",
            "-5", "1e999",
        ];
        for _ in 0..1000 {
            let pick = |n: u64| pieces[(n % pieces.len() as u64) as usize];
            let body = format!(
                r#"{{"safe":{},"selfcheck":{},"feeders":[{{"slot":{},"state":{}}}],"nozzles":{}}}"#,
                pick(next()),
                pick(next()),
                pick(next()),
                pick(next()),
                pick(next())
            );
            for kind in KINDS {
                if let Ok(sum) = parse_status(kind, body.as_bytes()) {
                    let h = health(true, true, Some(secs(1)), secs(20), false, "", Some(&sum));
                    if sum["safe"] != json!(false) || sum["selfcheck"] == json!("fail") {
                        assert!(!ready(h), "{body} gave {h}");
                    }
                }
            }
        }
    }

    fn registry(name: &str, change: impl FnOnce(&mut ServerConfig)) -> (PnpNodes, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("eh3d-server-pnpn-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = ServerConfig {
            data_dir: dir.clone(),
            pnp_nodes_enabled: true,
            ..ServerConfig::default()
        };
        change(&mut cfg);
        (PnpNodes::open(&cfg).unwrap(), dir)
    }

    fn rec(id: &str, kind: &str, address: &str) -> NodeRecord {
        NodeRecord {
            id: id.into(),
            kind: kind.into(),
            name: format!("node {id}"),
            address: address.into(),
            token: "secret-token".into(),
            machine: String::new(),
            enabled: true,
        }
    }

    #[test]
    fn nodes_are_registered_kept_on_disk_and_never_repeated() {
        let (n, dir) = registry("add", |_| {});
        let a = n.add(rec("", "feeder", "127.0.0.1:9001")).unwrap();
        assert_eq!(a.id, "feeder-1");
        assert_eq!(
            n.add(rec("", "feeder", "127.0.0.1:9002")).unwrap().id,
            "feeder-2"
        );
        assert_eq!(
            n.add(rec("", "head", "127.0.0.1:9003")).unwrap().id,
            "head-1"
        );
        assert_eq!(
            n.add(rec("", "head", "127.0.0.1:9001")).unwrap_err(),
            NodeError::Exists,
            "the same address twice"
        );
        assert_eq!(
            n.add(rec("head-1", "head", "127.0.0.1:9004")).unwrap_err(),
            NodeError::Exists,
            "the same id twice"
        );
        let reopened = PnpNodes::open(&ServerConfig {
            data_dir: dir.clone(),
            pnp_nodes_enabled: true,
            ..ServerConfig::default()
        })
        .unwrap();
        assert_eq!(reopened.len(), 3);
        assert_eq!(PnpNodes::file_reads(&dir).unwrap(), 3);
        n.remove("head-1").unwrap();
        assert_eq!(n.remove("head-1").unwrap_err(), NodeError::Unknown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn what_does_not_fit_is_refused_and_the_list_is_bound() {
        let (n, dir) = registry("bad", |c| c.pnp_nodes_max = 2);
        for bad in [
            rec("", "robot", "127.0.0.1:9001"),
            rec("UP", "head", "127.0.0.1:9001"),
            rec("", "head", ""),
            rec("", "head", "http://host:80"),
            rec("", "head", "user@host:80"),
            NodeRecord {
                token: "a b".into(),
                ..rec("", "head", "127.0.0.1:9001")
            },
            NodeRecord {
                token: "a\r\nX: y".into(),
                ..rec("", "head", "127.0.0.1:9001")
            },
            NodeRecord {
                token: "t".repeat(201),
                ..rec("", "head", "127.0.0.1:9001")
            },
            NodeRecord {
                name: " ".into(),
                ..rec("", "head", "127.0.0.1:9001")
            },
        ] {
            assert!(
                matches!(n.add(bad.clone()), Err(NodeError::Bad(_))),
                "{bad:?}"
            );
        }
        n.add(rec("", "head", "127.0.0.1:9001")).unwrap();
        n.add(rec("", "head", "127.0.0.1:9002")).unwrap();
        assert_eq!(
            n.add(rec("", "head", "127.0.0.1:9003")).unwrap_err(),
            NodeError::Full
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_node_that_is_not_heard_becomes_silent_and_holds_its_machine() {
        let (n, dir) = registry("silence", |c| {
            c.pnp_node_poll_s = 2;
            c.pnp_node_silence_s = 10;
        });
        let mut r = rec("", "feeder", "127.0.0.1:9001");
        r.machine = "pnp-1".into();
        let id = n.add(r).unwrap().id;
        let t0 = Instant::now();
        assert_eq!(n.not_ready_for("pnp-1", t0), vec![(id.clone(), "unknown")]);
        let sum = parse_status("feeder", FEEDER.as_bytes()).unwrap();
        let ok = Outcome {
            summary: Some(sum),
            error: None,
        };
        assert_eq!(n.record(&id, ok.clone(), t0), Some(("unknown", "ok")));
        assert!(n.not_ready_for("pnp-1", t0 + secs(5)).is_empty());
        assert_eq!(
            n.not_ready_for("pnp-1", t0 + secs(11)),
            vec![(id.clone(), "silent")]
        );
        // a failed poll inside the silence is "unreachable", and the status it had is dropped
        let failed = n
            .record(&id, Outcome::failed("unreachable"), t0 + secs(4))
            .unwrap();
        assert_eq!(failed, ("ok", "unreachable"));
        assert_eq!(n.view(&id, t0 + secs(4)).unwrap()["status"], Value::Null);
        // another machine is not held; an empty machine name holds nothing
        assert!(n.not_ready_for("pnp-2", t0 + secs(100)).is_empty());
        assert!(n.not_ready_for("", t0 + secs(100)).is_empty());
        // a node that is switched off holds nothing
        n.update(&id, None, None, None, None, Some(false)).unwrap();
        assert!(n.not_ready_for("pnp-1", t0 + secs(100)).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_answer_to_an_earlier_question_that_comes_late_changes_nothing() {
        let (n, dir) = registry("late", |_| {});
        let id = n.add(rec("", "feeder", "127.0.0.1:9001")).unwrap().id;
        let t0 = Instant::now();
        let ok = Outcome {
            summary: Some(parse_status("feeder", FEEDER.as_bytes()).unwrap()),
            error: None,
        };
        n.record_asked(&id, ok.clone(), t0 + secs(2), t0 + secs(2));
        let late = n.record_asked(&id, Outcome::failed("unreachable"), t0, t0 + secs(3));
        assert_eq!(late, Some(("ok", "ok")));
        assert_eq!(n.view(&id, t0 + secs(3)).unwrap()["health"], "ok");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_silence_is_never_less_than_two_polls() {
        let l = Limits {
            enabled: true,
            max: 4,
            poll_s: 30,
            silence_s: 20,
            timeout_s: 3,
        };
        assert_eq!(l.silence(), secs(60));
        let l = Limits {
            poll_s: 5,
            silence_s: 20,
            ..l
        };
        assert_eq!(l.silence(), secs(20));
    }

    #[test]
    fn the_token_is_never_shown_and_the_stars_keep_it() {
        let (n, dir) = registry("secret", |_| {});
        let id = n.add(rec("", "head", "127.0.0.1:9001")).unwrap().id;
        let view = n.view(&id, Instant::now()).unwrap();
        assert_eq!(view["token"], SECRET_MARK);
        assert!(!view.to_string().contains("secret-token"));
        n.update(
            &id,
            Some("renamed".into()),
            None,
            Some(SECRET_MARK.into()),
            None,
            None,
        )
        .unwrap();
        assert_eq!(n.target(&id).unwrap().token, "secret-token");
        n.update(&id, None, None, Some(String::new()), None, None)
            .unwrap();
        assert_eq!(n.view(&id, Instant::now()).unwrap()["token"], "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_damaged_file_is_not_taken_for_an_empty_list() {
        let dir =
            std::env::temp_dir().join(format!("eh3d-server-pnpn-damaged-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("pnp-nodes.json"), "{ broken").unwrap();
        assert!(PnpNodes::file_reads(&dir).is_err());
        let cfg = ServerConfig {
            data_dir: dir.clone(),
            ..ServerConfig::default()
        };
        assert!(PnpNodes::open(&cfg).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
