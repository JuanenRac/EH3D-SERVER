//! The 3D printers the server watches, **read-only**: a list of printers (their family of protocol, their address and the key of each), a reader for each one
//! (`src/printer_readers.rs`) and what each said last, in the contract `eh3d.printers.status/1` (`src/printer_status.rs`). Nothing here can ask a printer to do anything:
//! there is no pause, no stop, no temperature to set, no G-code and no file to give; the machines of the plugin `printer3d` that print are another thing and are not
//! touched by this list.
//!
//! The whole list is off until the switch `printers_enabled` is on, and a printer of the family `bambu_lan` on the LAN is only reached while `bambu_lan_enabled` is on
//! too. The access code or key of a printer is kept apart from the rest, in the store of the secrets (`printer-secrets.json`, private to the account of the server), is
//! never shown (`***`), never logged and never audited. A printer with the link `sim` lives in memory and opens no connection.
//!
//! Nothing here has run against a real printer: the tests run it against printers in memory.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::bambu::normalize_fingerprint;
use crate::config::ServerConfig;
use crate::printer_drivers::TcpHttp;
use crate::printer_readers::{
    BambuReader, MoonrakerReader, ReadError, Reading, SimReader, StatusReader,
};
use crate::printer_status::{CONTRACT, FAMILIES};
use crate::state::AppState;
use crate::store;
use crate::sync::Locked;

/// How a printer is reached: in memory, or on the local network.
pub const LINKS: &[&str] = &["sim", "lan"];
/// What stands for a secret in everything the server shows. Sent back, it means "keep the one there is".
pub const SECRET_MARK: &str = "***";
/// The port of Moonraker when the address names none.
const MOONRAKER_PORT: u16 = 7125;
const MAX_NAME: usize = 64;
const MAX_SECRET: usize = 128;
const MAX_ADDRESS: usize = 253;
const MAX_SERIAL: usize = 64;

/// The limits and the times, from `server.cfg`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    pub enabled: bool,
    pub max: usize,
    pub poll_s: u64,
    pub stale_s: u64,
    pub timeout_s: u64,
    pub bambu_port: u16,
    pub bambu_timeout_s: u64,
}

impl Limits {
    pub fn from_config(c: &ServerConfig) -> Limits {
        Limits {
            enabled: c.printers_enabled,
            max: c.printers_max as usize,
            poll_s: c.printer_poll_s,
            stale_s: c.printer_stale_s,
            timeout_s: c.printer_timeout_s,
            bambu_port: c.bambu_lan_port,
            bambu_timeout_s: c.bambu_lan_timeout_s,
        }
    }

    /// How old what a printer said may be and still be taken as current: never less than two polls.
    pub fn stale(&self) -> Duration {
        Duration::from_secs(self.stale_s.max(self.poll_s.saturating_mul(2)))
    }
}

/// What the server keeps of a printer, except its secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrinterRecord {
    pub id: String,
    pub name: String,
    /// `bambu_lan` or `moonraker`.
    pub family: String,
    /// `sim` or `lan`.
    pub link: String,
    /// `host` or `host:port`; empty for a simulated printer.
    #[serde(default)]
    pub address: String,
    /// The serial number of a Bambu printer: it names the topics of its reports.
    #[serde(default)]
    pub serial: String,
    /// The SHA-256 fingerprint of the certificate of a Bambu printer; empty until a person pins it.
    #[serde(default)]
    pub fingerprint: String,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

/// A change to a printer: what is `None` stays.
#[derive(Clone, Default)]
pub struct Change {
    pub name: Option<String>,
    pub address: Option<String>,
    pub serial: Option<String>,
    pub fingerprint: Option<String>,
    pub secret: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PrinterError {
    Off,
    Unknown,
    Bad(String),
    Exists,
    Full,
    Io,
}

impl PrinterError {
    pub fn code(&self) -> &'static str {
        match self {
            PrinterError::Off => "printers_off",
            PrinterError::Unknown => "unknown_printer",
            PrinterError::Bad(_) => "bad_printer",
            PrinterError::Exists => "printer_exists",
            PrinterError::Full => "printers_full",
            PrinterError::Io => "internal",
        }
    }

    pub fn status(&self) -> u16 {
        match self {
            PrinterError::Off => 501,
            PrinterError::Unknown => 404,
            PrinterError::Bad(_) => 422,
            PrinterError::Exists | PrinterError::Full => 409,
            PrinterError::Io => 500,
        }
    }

    pub fn detail(&self) -> String {
        match self {
            PrinterError::Off => {
                "the list of printers is off on this server (printers_enabled)".into()
            }
            PrinterError::Unknown => "there is no such printer".into(),
            PrinterError::Bad(why) => why.clone(),
            PrinterError::Exists => {
                "a printer with this id, this address or this serial is already registered".into()
            }
            PrinterError::Full => "the list of printers is full (printers_max)".into(),
            PrinterError::Io => "the list of printers could not be written".into(),
        }
    }
}

/// The check of what a person typed for a printer and its secret.
pub fn check_record(r: &PrinterRecord, secret: &str) -> Result<(), PrinterError> {
    let bad = |why: &str| Err(PrinterError::Bad(why.to_string()));
    if !FAMILIES.contains(&r.family.as_str()) {
        return bad("the family of a printer is bambu_lan or moonraker");
    }
    if !LINKS.contains(&r.link.as_str()) {
        return bad("the link of a printer is sim or lan");
    }
    if r.id.is_empty()
        || r.id.len() > 32
        || !r
            .id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return bad(
            "the id of a printer is made of lowercase letters, digits and hyphens (up to 32)",
        );
    }
    if r.name.trim().is_empty()
        || r.name.chars().count() > MAX_NAME
        || r.name.chars().any(char::is_control)
    {
        return bad("the name of a printer is 1 to 64 characters with no control characters");
    }
    if secret.len() > MAX_SECRET
        || secret == SECRET_MARK
        || secret.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return bad("the access code or key of a printer has no spaces or control characters and is at most 128 characters");
    }
    if r.serial.len() > MAX_SERIAL || !r.serial.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return bad("the serial of a printer is made of letters and digits (up to 64)");
    }
    if !r.fingerprint.is_empty() && normalize_fingerprint(&r.fingerprint).is_none() {
        return bad("the fingerprint of a certificate is its SHA-256: 64 hexadecimal digits, with or without colons");
    }
    if r.link == "sim" {
        return if r.address.is_empty() {
            Ok(())
        } else {
            bad("a simulated printer has no address")
        };
    }
    if host_and_port(&r.address).is_none() {
        return bad(
            "the address of a printer is host or host:port, with letters, digits, dots and hyphens",
        );
    }
    if r.family == "bambu_lan" && (r.serial.is_empty() || secret.is_empty()) {
        return bad("a Bambu printer on the LAN needs its serial and its access code");
    }
    Ok(())
}

/// The host and the port of `host` or `host:port`. Only letters, digits, dots and hyphens make a host, so nothing in it can end a line of a request.
fn host_and_port(address: &str) -> Option<(&str, Option<u16>)> {
    if address.len() > MAX_ADDRESS {
        return None;
    }
    let (host, port) = match address.split_once(':') {
        Some((h, p)) => {
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            (h, Some(p.parse::<u16>().ok().filter(|p| *p > 0)?))
        }
        None => (address, None),
    };
    (!host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-'))
    .then_some((host, port))
}

/// The reader of a printer, from what is registered.
fn make_reader(r: &PrinterRecord, secret: &str, limits: &Limits) -> Box<dyn StatusReader> {
    if r.link == "sim" {
        return Box::new(SimReader::new(&r.family));
    }
    let (host, port) = host_and_port(&r.address).unwrap_or(("", None));
    if r.family == "bambu_lan" {
        return Box::new(BambuReader::new(
            host,
            port.unwrap_or(limits.bambu_port),
            &r.serial,
            secret,
            Some(&r.fingerprint),
            Duration::from_secs(limits.bambu_timeout_s),
        ));
    }
    let url = format!("{host}:{}", port.unwrap_or(MOONRAKER_PORT));
    match TcpHttp::new(&url, Duration::from_secs(limits.timeout_s)) {
        Ok(http) => Box::new(MoonrakerReader::new(http, Some(secret.to_string()))),
        // an address that does not read never gets this far; if it did, the printer is only unreachable
        Err(_) => Box::new(NoReader),
    }
}

struct NoReader;

impl StatusReader for NoReader {
    fn read(&mut self) -> Result<Reading, ReadError> {
        Err(ReadError::Unreachable)
    }
}

/// The store of the secrets. The error of a file that does not read is said without what the reader of it found, which could be a piece of a secret.
fn read_secrets(path: &Path) -> io::Result<BTreeMap<String, String>> {
    store::read_json(path)
        .map(Option::unwrap_or_default)
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} does not read", path.display()),
            )
        })
}

/// Marks a printer as free again when the asking ends, also when it ends badly.
struct Free<'a>(&'a AtomicBool);

impl Drop for Free<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// The reader of one printer and whether it is being asked now.
pub struct Slot {
    reader: Mutex<Box<dyn StatusReader>>,
    busy: AtomicBool,
}

impl Slot {
    /// Asks the printer, unless it is already being asked (a printer that is slow does not get a pile of questions).
    pub fn ask(&self) -> Option<Result<Reading, ReadError>> {
        if self.busy.swap(true, Ordering::SeqCst) {
            return None;
        }
        let _free = Free(&self.busy);
        Some(self.reader.locked().read())
    }
}

/// What the readings said of a printer. Kept in memory only.
#[derive(Clone, Debug, Default)]
struct Seen {
    tried: Option<Instant>,
    /// When the printer last said something, and what.
    heard: Option<(Instant, Reading)>,
    error: Option<ReadError>,
    ok: u64,
    failed: u64,
    /// What was last told to the clients.
    told: String,
}

struct Inner {
    records: BTreeMap<String, PrinterRecord>,
    secrets: BTreeMap<String, String>,
    seen: HashMap<String, Seen>,
    slots: HashMap<String, Arc<Slot>>,
}

/// The printers registered on this server.
pub struct Printers {
    path: PathBuf,
    secrets_path: PathBuf,
    pub limits: Limits,
    inner: Mutex<Inner>,
}

/// How a printer is, from the facts. The order of the rules is the safe one: what is not current is never shown as current.
pub fn health(
    enabled: bool,
    off: bool,
    tried_ago: Option<Duration>,
    heard_ago: Option<Duration>,
    stale: Duration,
    error: Option<&ReadError>,
) -> &'static str {
    if !enabled {
        return "disabled";
    }
    if off {
        return "off";
    }
    let Some(tried_ago) = tried_ago else {
        return "unknown";
    };
    match error {
        Some(ReadError::Refused) => return "refused",
        Some(
            ReadError::NotPinned { .. } | ReadError::Changed { .. } | ReadError::BadFingerprint,
        ) => return "untrusted",
        Some(ReadError::BadReport) => return "bad_report",
        Some(ReadError::Unreachable | ReadError::NoReport) => return "offline",
        None => {}
    }
    match heard_ago {
        Some(ago) if ago <= stale && tried_ago <= stale => "online",
        _ => "stale",
    }
}

impl Printers {
    pub fn open(cfg: &ServerConfig) -> io::Result<Printers> {
        let path = cfg.data_dir.join("printers.json");
        let secrets_path = cfg.data_dir.join("printer-secrets.json");
        let list: Vec<PrinterRecord> = store::read_json(&path)?.unwrap_or_default();
        let secrets = read_secrets(&secrets_path)?;
        Ok(Printers {
            path,
            secrets_path,
            limits: Limits::from_config(cfg),
            inner: Mutex::new(Inner {
                records: list.into_iter().map(|r| (r.id.clone(), r)).collect(),
                secrets,
                seen: HashMap::new(),
                slots: HashMap::new(),
            }),
        })
    }

    /// Whether the files of the printers in a data folder read (for `--check`). The number of printers.
    pub fn file_reads(dir: &Path) -> io::Result<usize> {
        let list: Option<Vec<PrinterRecord>> = store::read_json(&dir.join("printers.json"))?;
        read_secrets(&dir.join("printer-secrets.json"))?;
        Ok(list.map_or(0, |l| l.len()))
    }

    /// Writes the list and the secrets. The copy the store would keep of the secrets is not kept: a key that is removed is gone.
    fn save(&self, inner: &Inner) -> Result<(), PrinterError> {
        let secrets = serde_json::to_vec_pretty(&inner.secrets).map_err(|_| PrinterError::Io)?;
        store::write_durable(&self.secrets_path, &secrets, false).map_err(|_| PrinterError::Io)?;
        let list: Vec<&PrinterRecord> = inner.records.values().collect();
        store::write_json(&self.path, &list).map_err(|_| PrinterError::Io)
    }

    pub fn len(&self) -> usize {
        self.inner.locked().records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn clashes(inner: &Inner, r: &PrinterRecord) -> bool {
        if r.link != "lan" {
            return false;
        }
        inner.records.values().any(|o| {
            o.id != r.id
                && o.link == "lan"
                && ((o.address.eq_ignore_ascii_case(&r.address) && o.family == r.family)
                    || (!r.serial.is_empty() && o.serial.eq_ignore_ascii_case(&r.serial)))
        })
    }

    /// Registers a printer. An empty id gets the first free `printer-<n>`.
    pub fn add(&self, mut r: PrinterRecord, secret: &str) -> Result<PrinterRecord, PrinterError> {
        let mut inner = self.inner.locked();
        if r.id.is_empty() {
            r.id = (1..)
                .map(|n| format!("printer-{n}"))
                .find(|id| !inner.records.contains_key(id))
                .unwrap_or_default();
        }
        check_record(&r, secret)?;
        if inner.records.len() >= self.limits.max {
            return Err(PrinterError::Full);
        }
        if inner.records.contains_key(&r.id) || Self::clashes(&inner, &r) {
            return Err(PrinterError::Exists);
        }
        inner.records.insert(r.id.clone(), r.clone());
        if !secret.is_empty() {
            inner.secrets.insert(r.id.clone(), secret.to_string());
        }
        if let Err(e) = self.save(&inner) {
            inner.records.remove(&r.id);
            inner.secrets.remove(&r.id);
            return Err(e);
        }
        Ok(r)
    }

    /// Changes a printer. A secret that is the mark is kept; an empty one is removed.
    pub fn update(&self, id: &str, change: Change) -> Result<PrinterRecord, PrinterError> {
        let mut inner = self.inner.locked();
        let before = inner
            .records
            .get(id)
            .cloned()
            .ok_or(PrinterError::Unknown)?;
        let old_secret = inner.secrets.get(id).cloned().unwrap_or_default();
        let mut r = before.clone();
        if let Some(v) = change.name {
            r.name = v;
        }
        if let Some(v) = change.address {
            r.address = v;
        }
        if let Some(v) = change.serial {
            // the shortened serial that was shown means "keep the serial"
            if !v.starts_with(SECRET_MARK) {
                r.serial = v;
            }
        }
        if let Some(v) = change.fingerprint {
            r.fingerprint = v;
        }
        if let Some(v) = change.enabled {
            r.enabled = v;
        }
        let secret = match change.secret {
            Some(v) if v != SECRET_MARK => v,
            _ => old_secret.clone(),
        };
        check_record(&r, &secret)?;
        if Self::clashes(&inner, &r) {
            return Err(PrinterError::Exists);
        }
        inner.records.insert(id.to_string(), r.clone());
        if secret.is_empty() {
            inner.secrets.remove(id);
        } else {
            inner.secrets.insert(id.to_string(), secret.clone());
        }
        if let Err(e) = self.save(&inner) {
            inner.records.insert(id.to_string(), before);
            if old_secret.is_empty() {
                inner.secrets.remove(id);
            } else {
                inner.secrets.insert(id.to_string(), old_secret);
            }
            return Err(e);
        }
        // what was heard through another address or key says nothing of the new one
        let renamed_only = PrinterRecord {
            name: before.name.clone(),
            ..r.clone()
        } == before
            && secret == old_secret;
        if !renamed_only {
            inner.seen.remove(id);
            inner.slots.remove(id);
        }
        Ok(r)
    }

    pub fn remove(&self, id: &str) -> Result<(), PrinterError> {
        let mut inner = self.inner.locked();
        let before = inner.records.remove(id).ok_or(PrinterError::Unknown)?;
        let secret = inner.secrets.remove(id);
        if let Err(e) = self.save(&inner) {
            inner.records.insert(id.to_string(), before);
            if let Some(s) = secret {
                inner.secrets.insert(id.to_string(), s);
            }
            return Err(e);
        }
        inner.seen.remove(id);
        inner.slots.remove(id);
        Ok(())
    }

    /// Whether a printer is not to be reached at all because a switch is off.
    fn is_off(r: &PrinterRecord, bambu_lan_enabled: bool) -> bool {
        r.family == "bambu_lan" && r.link == "lan" && !bambu_lan_enabled
    }

    /// The readers to ask now, one for each printer that is switched on and may be reached. A printer that may not be reached loses its reader, and so its connection.
    pub fn to_ask(&self, bambu_lan_enabled: bool) -> Vec<(String, Arc<Slot>)> {
        let mut inner = self.inner.locked();
        let inner = &mut *inner;
        let mut out = Vec::new();
        if !self.limits.enabled {
            return out;
        }
        for r in inner.records.values() {
            if !r.enabled || Self::is_off(r, bambu_lan_enabled) {
                inner.slots.remove(&r.id);
                inner.seen.remove(&r.id);
                continue;
            }
            let slot = inner.slots.entry(r.id.clone()).or_insert_with(|| {
                let secret = inner.secrets.get(&r.id).map_or("", String::as_str);
                Arc::new(Slot {
                    reader: Mutex::new(make_reader(r, secret, &self.limits)),
                    busy: AtomicBool::new(false),
                })
            });
            out.push((r.id.clone(), slot.clone()));
        }
        out
    }

    /// Writes down what a reading found at `now`, if `slot` is still the reader of that printer (a printer that was changed or removed while it was asked is not touched).
    /// Gives the news for the clients when something they would see changed.
    pub fn record(
        &self,
        id: &str,
        slot: &Arc<Slot>,
        result: Result<Reading, ReadError>,
        now: Instant,
    ) -> Option<Value> {
        let mut inner = self.inner.locked();
        if !inner.slots.get(id).is_some_and(|s| Arc::ptr_eq(s, slot)) {
            return None;
        }
        let stale = self.limits.stale();
        let seen = inner.seen.entry(id.to_string()).or_default();
        seen.tried = Some(now);
        match result {
            Ok(reading) => {
                seen.ok += 1;
                seen.error = None;
                // the age of what was said counts: an old report does not become new because it was read again
                let at = now.checked_sub(reading.report_age).unwrap_or(now);
                seen.heard = Some((at, reading));
            }
            Err(e) => {
                seen.failed += 1;
                seen.error = Some(e);
                // what a printer said before is not kept as what it says now
                seen.heard = None;
            }
        }
        let heard_ago = seen
            .heard
            .as_ref()
            .map(|(at, _)| now.saturating_duration_since(*at));
        let h = health(
            true,
            false,
            Some(Duration::ZERO),
            heard_ago,
            stale,
            seen.error.as_ref(),
        );
        let (state, alerts) = seen
            .heard
            .as_ref()
            .map_or(("unknown", Vec::new()), |(_, r)| {
                (
                    r.status.state,
                    r.status.alerts.iter().map(|a| a.code.clone()).collect(),
                )
            });
        let news =
            json!({ "type": "printer", "id": id, "health": h, "state": state, "alerts": alerts });
        let text = news.to_string();
        if seen.told == text {
            return None;
        }
        seen.told = text;
        Some(news)
    }

    fn view_of(
        &self,
        inner: &Inner,
        r: &PrinterRecord,
        bambu_lan_enabled: bool,
        now: Instant,
    ) -> Value {
        let empty = Seen::default();
        let s = inner.seen.get(&r.id).unwrap_or(&empty);
        let off = Self::is_off(r, bambu_lan_enabled);
        let ago = |at: Instant| now.saturating_duration_since(at);
        let h = health(
            r.enabled,
            off,
            s.tried.map(ago),
            s.heard.as_ref().map(|(at, _)| ago(*at)),
            self.limits.stale(),
            s.error.as_ref(),
        );
        // only a printer that is online or whose last words are old shows what it said
        let said = s.heard.as_ref().filter(|_| matches!(h, "online" | "stale"));
        let serial = if r.serial.is_empty() {
            String::new()
        } else {
            let skip = r.serial.chars().count().saturating_sub(4);
            format!(
                "{SECRET_MARK}{}",
                r.serial.chars().skip(skip).collect::<String>()
            )
        };
        json!({
            "id": r.id, "name": r.name, "family": r.family, "link": r.link, "address": r.address,
            "serial": serial, "fingerprint": r.fingerprint,
            "secret": if inner.secrets.contains_key(&r.id) { SECRET_MARK } else { "" },
            "enabled": r.enabled,
            "health": h,
            "current": h == "online",
            "status": said.map(|(_, reading)| &reading.status),
            "capabilities": said.map_or_else(Vec::new, |(_, reading)| reading.status.capabilities()),
            "control": false,
            "age_s": said.map(|(at, _)| ago(*at).as_secs()),
            "polls": { "ok": s.ok, "failed": s.failed },
            "last_error": match (off, &s.error) {
                (true, _) => "bambu_lan_off",
                (false, Some(e)) => e.word(),
                (false, None) => "",
            },
            "certificate_seen": s.error.as_ref().and_then(ReadError::certificate_seen).unwrap_or(""),
            "evidence": if r.link == "sim" { "simulator" } else { "unvalidated" },
            "simulator_only": r.link == "sim",
        })
    }

    pub fn list_views(&self, bambu_lan_enabled: bool, now: Instant) -> Vec<Value> {
        let inner = self.inner.locked();
        inner
            .records
            .values()
            .map(|r| self.view_of(&inner, r, bambu_lan_enabled, now))
            .collect()
    }

    pub fn view(&self, id: &str, bambu_lan_enabled: bool, now: Instant) -> Option<Value> {
        let inner = self.inner.locked();
        inner
            .records
            .get(id)
            .map(|r| self.view_of(&inner, r, bambu_lan_enabled, now))
    }

    /// The whole answer of the list: the contract, the printers, how many there are in each health and the limits.
    pub fn document(&self, bambu_lan_enabled: bool, now: Instant) -> Value {
        let printers = self.list_views(bambu_lan_enabled, now);
        let mut by: BTreeMap<String, u64> = BTreeMap::new();
        let mut alerts = 0u64;
        for p in &printers {
            *by.entry(p["health"].as_str().unwrap_or("unknown").to_string())
                .or_default() += 1;
            alerts += u64::from(
                p["status"]["alerts"]
                    .as_array()
                    .is_some_and(|a| !a.is_empty()),
            );
        }
        let l = &self.limits;
        json!({
            "schema": CONTRACT,
            "read_only": true,
            "printers": printers,
            "summary": { "registered": self.len(), "with_alerts": alerts, "by_health": by },
            "limits": {
                "max": l.max, "poll_s": l.poll_s, "stale_s": l.stale().as_secs(), "timeout_s": l.timeout_s,
                "families": FAMILIES, "links": LINKS,
            },
        })
    }
}

/// Asks every printer that may be reached, each on its own thread so that a slow one does not delay the others, and tells the clients what changed. The handles end when
/// the readings do.
pub fn poll_all(st: &Arc<AppState>) -> Vec<tokio::task::JoinHandle<()>> {
    st.printers
        .to_ask(st.policy.get().bambu_lan_enabled)
        .into_iter()
        .map(|(id, slot)| {
            let st = st.clone();
            tokio::task::spawn_blocking(move || {
                if let Some(result) = slot.ask() {
                    if let Some(news) = st.printers.record(&id, &slot, result, Instant::now()) {
                        st.publish(news);
                    }
                }
            })
        })
        .collect()
}

/// The loop that asks the printers every `printer_poll_s` seconds, while the switch is on. With the switch off it does nothing.
pub fn spawn(st: Arc<AppState>) -> Option<tokio::task::JoinHandle<()>> {
    if !st.printers.limits.enabled {
        return None;
    }
    let every = Duration::from_secs(st.printers.limits.poll_s);
    Some(tokio::spawn(async move {
        loop {
            drop(poll_all(&st));
            tokio::time::sleep(every).await;
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::printer_status::Alert;

    const KEY: &str = "code-not-to-show";

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn config(dir: &Path) -> ServerConfig {
        ServerConfig {
            data_dir: dir.to_path_buf(),
            printers_enabled: true,
            ..ServerConfig::default()
        }
    }

    fn registry(name: &str, change: impl FnOnce(&mut ServerConfig)) -> (Printers, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "eh3d-server-printers-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = config(&dir);
        change(&mut cfg);
        (Printers::open(&cfg).unwrap(), dir)
    }

    fn sim(id: &str, family: &str) -> PrinterRecord {
        PrinterRecord {
            id: id.into(),
            name: format!("printer {id}"),
            family: family.into(),
            link: "sim".into(),
            address: String::new(),
            serial: String::new(),
            fingerprint: String::new(),
            enabled: true,
        }
    }

    fn bambu(address: &str, serial: &str) -> PrinterRecord {
        PrinterRecord {
            link: "lan".into(),
            address: address.into(),
            serial: serial.into(),
            ..sim("", "bambu_lan")
        }
    }

    #[test]
    fn several_printers_are_registered_and_their_secrets_are_kept_apart_and_never_shown() {
        let (p, dir) = registry("secret", |_| {});
        let a = p.add(bambu("10.0.0.5", "01P00A111111111"), KEY).unwrap();
        let b = p
            .add(bambu("10.0.0.6:8883", "03919C222222222"), "other-code")
            .unwrap();
        let c = p.add(sim("", "moonraker"), "").unwrap();
        assert_eq!(
            (a.id.as_str(), b.id.as_str(), c.id.as_str()),
            ("printer-1", "printer-2", "printer-3")
        );
        let shown = p.document(true, Instant::now()).to_string();
        assert!(
            !shown.contains(KEY) && !shown.contains("other-code"),
            "{shown}"
        );
        assert!(
            !shown.contains("01P00A111111111"),
            "the serial is shortened"
        );
        let view = p.view(&a.id, true, Instant::now()).unwrap();
        assert_eq!(
            (view["secret"].as_str(), view["serial"].as_str()),
            (Some("***"), Some("***1111"))
        );
        assert_eq!(view["control"], false);
        assert_eq!(p.view(&c.id, true, Instant::now()).unwrap()["secret"], "");
        let list = std::fs::read_to_string(dir.join("printers.json")).unwrap();
        assert!(!list.contains(KEY), "the list holds no secret");
        let secrets = std::fs::read_to_string(dir.join("printer-secrets.json")).unwrap();
        assert!(secrets.contains(KEY));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("printer-secrets.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0);
        }
        // the mark and the shortened serial that were shown keep what is there
        p.update(
            &a.id,
            Change {
                secret: Some(SECRET_MARK.into()),
                serial: Some("***1111".into()),
                name: Some("left".into()),
                ..Change::default()
            },
        )
        .unwrap();
        let again = Printers::open(&config(&dir)).unwrap();
        assert_eq!(again.len(), 3);
        assert_eq!(again.inner.locked().secrets[&a.id], KEY);
        assert_eq!(
            again.inner.locked().records[&a.id].serial,
            "01P00A111111111"
        );
        // a printer that is removed leaves no secret behind, nor a copy of it
        p.remove(&a.id).unwrap();
        assert_eq!(p.remove(&a.id).unwrap_err(), PrinterError::Unknown);
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("printer-secrets")
            {
                assert!(
                    !std::fs::read_to_string(&path).unwrap().contains(KEY),
                    "{}",
                    path.display()
                );
            }
        }
        assert_eq!(Printers::file_reads(&dir).unwrap(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn what_does_not_fit_is_refused_and_the_list_is_bound() {
        let (p, dir) = registry("bad", |c| c.printers_max = 2);
        let ok = bambu("printer.local", "ABC123");
        for (bad, secret) in [
            (sim("", "marlin"), ""),
            (
                PrinterRecord {
                    link: "wan".into(),
                    ..sim("", "moonraker")
                },
                "",
            ),
            (sim("UP", "moonraker"), ""),
            (
                PrinterRecord {
                    name: " ".into(),
                    ..sim("", "moonraker")
                },
                "",
            ),
            (
                PrinterRecord {
                    address: "10.0.0.1".into(),
                    ..sim("", "moonraker")
                },
                "",
            ),
            (
                PrinterRecord {
                    address: String::new(),
                    ..ok.clone()
                },
                "c",
            ),
            (
                PrinterRecord {
                    address: "http://x".into(),
                    ..ok.clone()
                },
                "c",
            ),
            (
                PrinterRecord {
                    address: "user@x".into(),
                    ..ok.clone()
                },
                "c",
            ),
            (
                PrinterRecord {
                    address: "x\r\nHost: y".into(),
                    ..ok.clone()
                },
                "c",
            ),
            (
                PrinterRecord {
                    address: "x:0".into(),
                    ..ok.clone()
                },
                "c",
            ),
            (
                PrinterRecord {
                    address: "x:70000".into(),
                    ..ok.clone()
                },
                "c",
            ),
            (
                PrinterRecord {
                    address: "x:+80".into(),
                    ..ok.clone()
                },
                "c",
            ),
            (
                PrinterRecord {
                    address: "a".repeat(254),
                    ..ok.clone()
                },
                "c",
            ),
            (
                PrinterRecord {
                    serial: "A#".into(),
                    ..ok.clone()
                },
                "c",
            ),
            (
                PrinterRecord {
                    serial: String::new(),
                    ..ok.clone()
                },
                "c",
            ),
            (
                PrinterRecord {
                    fingerprint: "abc".into(),
                    ..ok.clone()
                },
                "c",
            ),
            (ok.clone(), ""),
            (ok.clone(), "a b"),
            (ok.clone(), "a\r\nb"),
            (ok.clone(), "***"),
        ] {
            assert!(
                matches!(p.add(bad.clone(), secret), Err(PrinterError::Bad(_))),
                "{bad:?} {secret:?}"
            );
        }
        assert!(p.is_empty());
        p.add(ok.clone(), "c").unwrap();
        assert_eq!(
            p.add(bambu("PRINTER.local", "XYZ"), "c").unwrap_err(),
            PrinterError::Exists
        );
        assert_eq!(
            p.add(bambu("other.local", "abc123"), "c").unwrap_err(),
            PrinterError::Exists
        );
        p.add(sim("", "bambu_lan"), "").unwrap();
        assert_eq!(
            p.add(sim("", "bambu_lan"), "").unwrap_err(),
            PrinterError::Full
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn health_never_shows_as_current_what_is_not() {
        let stale = secs(90);
        let h =
            |enabled, off, tried: Option<u64>, heard: Option<u64>, error: Option<&ReadError>| {
                health(enabled, off, tried.map(secs), heard.map(secs), stale, error)
            };
        assert_eq!(h(false, false, Some(0), Some(0), None), "disabled");
        assert_eq!(h(true, true, Some(0), Some(0), None), "off");
        assert_eq!(h(true, false, None, None, None), "unknown");
        assert_eq!(h(true, false, Some(1), Some(1), None), "online");
        assert_eq!(h(true, false, Some(1), Some(91), None), "stale");
        assert_eq!(
            h(true, false, Some(91), Some(1), None),
            "stale",
            "a reading that hangs"
        );
        assert_eq!(h(true, false, Some(1), None, None), "stale");
        for (error, word) in [
            (ReadError::Unreachable, "offline"),
            (ReadError::NoReport, "offline"),
            (ReadError::Refused, "refused"),
            (ReadError::NotPinned { seen: "a".into() }, "untrusted"),
            (ReadError::Changed { seen: "a".into() }, "untrusted"),
            (ReadError::BadFingerprint, "untrusted"),
            (ReadError::BadReport, "bad_report"),
        ] {
            assert_eq!(h(true, false, Some(1), Some(1), Some(&error)), word);
        }
        let l = Limits {
            poll_s: 200,
            ..Limits::from_config(&ServerConfig::default())
        };
        assert_eq!(l.stale(), secs(400));
    }

    #[test]
    fn a_reading_is_shown_a_failure_hides_it_and_only_changes_are_told() {
        let (p, dir) = registry("read", |_| {});
        let id = p.add(sim("", "bambu_lan"), "").unwrap().id;
        let (_, slot) = p.to_ask(false).remove(0);
        let now = Instant::now();
        let reading = slot.ask().unwrap().unwrap();
        let news = p.record(&id, &slot, Ok(reading.clone()), now).unwrap();
        assert_eq!(
            (news["health"].as_str(), news["state"].as_str()),
            (Some("online"), Some("printing"))
        );
        assert!(
            p.record(&id, &slot, Ok(reading.clone()), now).is_none(),
            "nothing new"
        );
        let view = p.view(&id, false, now).unwrap();
        assert_eq!(view["health"], "online");
        assert_eq!(view["evidence"], "simulator");
        assert!(view["status"]["progress_pct"].is_number());
        let later = p.view(&id, false, now + secs(91)).unwrap();
        assert_eq!(
            (later["health"].as_str(), later["current"].as_bool()),
            (Some("stale"), Some(false))
        );
        let mut warned = reading.clone();
        warned.status.alerts.push(Alert {
            source: "hms",
            severity: "error",
            code: "HMS_1".into(),
            message: String::new(),
        });
        let news = p.record(&id, &slot, Ok(warned), now).unwrap();
        assert_eq!(news["alerts"][0], "HMS_1");
        assert_eq!(p.document(false, now)["summary"]["with_alerts"], 1);
        // an old report does not become new by being read
        let mut old = reading;
        old.report_age = secs(100);
        let news = p.record(&id, &slot, Ok(old), now + secs(200)).unwrap();
        assert_eq!(news["health"], "stale");
        let news = p
            .record(&id, &slot, Err(ReadError::Unreachable), now)
            .unwrap();
        assert_eq!(news["health"], "offline");
        let view = p.view(&id, false, now).unwrap();
        assert_eq!(
            (view["status"].clone(), view["last_error"].as_str()),
            (Value::Null, Some("unreachable"))
        );
        // a reading that ends after the printer changed is thrown away
        p.update(
            &id,
            Change {
                enabled: Some(false),
                ..Change::default()
            },
        )
        .unwrap();
        assert!(p.record(&id, &slot, Err(ReadError::Refused), now).is_none());
        assert_eq!(p.view(&id, false, now).unwrap()["health"], "disabled");
        assert!(p.to_ask(false).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_is_asked_while_a_switch_is_off() {
        let (p, dir) = registry("off", |c| c.printers_enabled = false);
        p.add(sim("", "moonraker"), "").unwrap();
        assert!(p.to_ask(true).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
        let (p, dir) = registry("bambu-off", |_| {});
        let real = p.add(bambu("10.0.0.5", "ABC"), KEY).unwrap().id;
        let pretend = p.add(sim("", "bambu_lan"), "").unwrap().id;
        let asked: Vec<String> = p.to_ask(false).into_iter().map(|(id, _)| id).collect();
        assert_eq!(asked, vec![pretend]);
        let view = p.view(&real, false, Instant::now()).unwrap();
        assert_eq!(
            (view["health"].as_str(), view["last_error"].as_str()),
            (Some("off"), Some("bambu_lan_off"))
        );
        assert_eq!(p.to_ask(true).len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_printer_that_is_being_asked_is_not_asked_again() {
        let (p, dir) = registry("busy", |_| {});
        p.add(sim("", "moonraker"), "").unwrap();
        let (_, slot) = p.to_ask(false).remove(0);
        slot.busy.store(true, Ordering::SeqCst);
        assert!(slot.ask().is_none());
        slot.busy.store(false, Ordering::SeqCst);
        assert!(slot.ask().is_some() && slot.ask().is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_damaged_file_stops_the_start_and_says_no_secret() {
        let (_, dir) = registry("damaged", |_| {});
        std::fs::write(dir.join("printers.json"), "{ broken").unwrap();
        assert!(Printers::open(&config(&dir)).is_err());
        std::fs::remove_file(dir.join("printers.json")).unwrap();
        std::fs::write(
            dir.join("printer-secrets.json"),
            format!("{{\"a\":{{\"{KEY}\""),
        )
        .unwrap();
        assert!(Printers::open(&config(&dir)).is_err());
        let said = Printers::file_reads(&dir).unwrap_err().to_string();
        assert!(!said.contains(KEY), "{said}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
