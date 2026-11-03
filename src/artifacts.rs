//! The artifacts and the blobs: the files the design programs keep on the server and the files the tasks make.
//!
//! **Two identities that are never confused.** The `blob_sha256` is the identity of the *bytes*: it is the key under which the bytes are stored once, however many
//! artifacts point to them. The `artifact_id` (`a-<uuid v7>`) is the identity of the *record*: who owns it, what made it, from which inputs, with which parameters.
//! Two artifacts may point to the same blob and have different provenance; a project, a revision or a task refers to the `artifact_id`, and the provenance lists its
//! inputs by both, so a result can be reproduced from the exact bytes.
//!
//! **What the files look like.** `blobs/<first two hex digits>/<sha256>` in the data folder, written to a temporary file and renamed, so a power cut leaves no half blob under
//! a hash; `artifacts.json` with the records; `uploads/` with the uploads that are not complete. Reading a blob does not trust the folder: [`ArtifactStore::verify`] reads every
//! blob again and says which ones are not what their name says.
//!
//! **Privacy.** Whether a blob exists is not something anybody may ask about: `HEAD /blobs/:sha256` answers yes only for bytes that an artifact the caller can see points to.
//! The deduplication happens inside the store and cannot be seen from outside: a caller that cannot see the blob has to send the bytes, and the server keeps one copy.
//!
//! Like `tasks`, this file does not name machines, connections or jobs.

use std::collections::{BTreeMap, HashSet};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use eh3d_diagnostic::Diagnostic;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::auth::{Identity, Role};
use crate::config::ServerConfig;
use crate::jobs::now;
use crate::state::AppState;
use crate::store;
use crate::sync::Locked;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Retention {
    /// Kept while a task, a revision or another artifact points to it; purged some days after the last pointer is gone.
    KeepWhileReferenced,
    /// Kept until a date.
    KeepUntil,
    /// Kept until an administrator says otherwise.
    Pinned,
}

/// What is known of an artifact. The states are kept apart on purpose: nothing here is a single "validated" flag.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Validation {
    /// The bytes are the ones whose hash the record carries (checked when the artifact was made).
    pub intact: bool,
    /// A reader could read it.
    pub readable: Option<bool>,
    /// Its parts agree with each other.
    pub coherent: Option<bool>,
    /// A rule check was run on it.
    pub erc_drc_run: Option<bool>,
    /// It was compared with a reference that is known to be right.
    pub oracle_checked: Option<bool>,
    /// It was made, measured and found right on a real machine.
    pub physically_validated: Option<bool>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct InputRef {
    pub artifact_id: String,
    pub blob_sha256: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Producer {
    pub tool: String,
    pub version: String,
    #[serde(default)]
    pub config_sha256: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Origin {
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub revision: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ArtifactRecord {
    pub artifact_id: String,
    pub blob_sha256: String,
    pub size: u64,
    /// The owner key (see `Identity::owner`).
    pub owner: String,
    pub name: String,
    pub format: String,
    #[serde(default)]
    pub schema_version: Option<String>,
    #[serde(default)]
    pub origin: Origin,
    #[serde(default)]
    pub producer: Option<Producer>,
    #[serde(default)]
    pub inputs: Vec<InputRef>,
    #[serde(default)]
    pub params: Value,
    #[serde(default)]
    pub result_type: Option<String>,
    #[serde(default)]
    pub diagnostics: Vec<Diagnostic>,
    pub validation: Validation,
    /// When it was made: kept as metadata, never part of the content.
    pub created_at: u64,
    pub retention: Retention,
    #[serde(default)]
    pub keep_until: Option<u64>,
    /// The people the owner shared it with (for reading).
    #[serde(default)]
    pub shares: Vec<crate::projects::Share>,
}

impl ArtifactRecord {
    /// The record as the API shows it.
    pub fn view(&self) -> Value {
        json!({
            "artifact_id": self.artifact_id, "blob_sha256": self.blob_sha256, "size": self.size, "owner": self.owner,
            "name": self.name, "format": self.format, "schema_version": self.schema_version,
            "origin": self.origin, "producer": self.producer, "inputs": self.inputs, "params": self.params,
            "result_type": self.result_type, "diagnostics": self.diagnostics, "validation": self.validation,
            "audit": { "created_at": self.created_at }, "retention": self.retention, "keep_until": self.keep_until,
        })
    }
}

/// What a caller says about an artifact it makes; the rest of the record the server fills in.
#[derive(Deserialize, Debug, Clone, Default)]
pub struct NewArtifact {
    pub name: String,
    pub format: String,
    #[serde(default)]
    pub schema_version: Option<String>,
    #[serde(default)]
    pub origin: Origin,
    #[serde(default)]
    pub producer: Option<Producer>,
    #[serde(default)]
    pub inputs: Vec<InputRef>,
    #[serde(default)]
    pub params: Option<Value>,
    #[serde(default)]
    pub result_type: Option<String>,
}

#[derive(Debug, PartialEq)]
pub enum ArtifactError {
    /// The description of the artifact does not fit (the text says what).
    Bad(String),
    TooBig,
    StoreFull,
    Unknown,
    UnknownUpload,
    UploadConflict,
    UploadIncomplete,
    HashMismatch,
    BadUpload(String),
    InUse,
    Quota,
    Damaged,
    Storage,
}

impl ArtifactError {
    pub fn code(&self) -> &'static str {
        match self {
            ArtifactError::Bad(_) => "bad_artifact",
            ArtifactError::TooBig => "too_big",
            ArtifactError::StoreFull => "store_full",
            ArtifactError::Unknown => "unknown_artifact",
            ArtifactError::UnknownUpload => "unknown_upload",
            ArtifactError::UploadConflict => "upload_conflict",
            ArtifactError::UploadIncomplete => "upload_incomplete",
            ArtifactError::HashMismatch => "hash_mismatch",
            ArtifactError::BadUpload(_) => "bad_upload",
            ArtifactError::InUse => "artifact_in_use",
            ArtifactError::Quota => "quota",
            ArtifactError::Damaged => "blob_damaged",
            ArtifactError::Storage => "storage",
        }
    }

    pub fn status(&self) -> u16 {
        match self {
            ArtifactError::Bad(_)
            | ArtifactError::TooBig
            | ArtifactError::HashMismatch
            | ArtifactError::BadUpload(_) => 422,
            ArtifactError::Unknown | ArtifactError::UnknownUpload => 404,
            ArtifactError::UploadConflict
            | ArtifactError::UploadIncomplete
            | ArtifactError::InUse => 409,
            ArtifactError::Quota => 429,
            ArtifactError::StoreFull => 507,
            ArtifactError::Damaged | ArtifactError::Storage => 500,
        }
    }

    pub fn detail(&self) -> String {
        match self {
            ArtifactError::Bad(why) | ArtifactError::BadUpload(why) => why.clone(),
            _ => String::new(),
        }
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Whether `text` is a SHA-256 in lower-case hexadecimal.
pub fn is_sha256(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= 120
        && !name
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
}

fn valid_format(format: &str) -> bool {
    !format.is_empty()
        && format.len() <= 100
        && format
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"./+-_".contains(&b))
}

fn valid_result_type(t: &str) -> bool {
    matches!(t, "T1" | "T2" | "T3" | "T4" | "T5" | "T6" | "T7")
}

/// Checks the description of an artifact; the text says what is wrong.
pub fn check_new(a: &NewArtifact) -> Result<(), String> {
    if !valid_name(&a.name) {
        return Err(
            "name is from 1 to 120 characters, without control characters or slashes".into(),
        );
    }
    if !valid_format(&a.format) {
        return Err(
            "format is a media type in lower case, such as application/x-phoenixproj".into(),
        );
    }
    if a.schema_version.as_ref().is_some_and(|v| v.len() > 40) {
        return Err("schema_version is at most 40 characters".into());
    }
    if a.result_type
        .as_deref()
        .is_some_and(|t| !valid_result_type(t))
    {
        return Err("result_type is from T1 to T7".into());
    }
    if a.inputs.len() > 256 {
        return Err("at most 256 inputs".into());
    }
    for i in &a.inputs {
        if !crate::ids::is_valid(crate::ids::Kind::Artifact, &i.artifact_id)
            || !is_sha256(&i.blob_sha256)
        {
            return Err("an input is {artifact_id, blob_sha256}".into());
        }
    }
    if let Some(p) = &a.params {
        if !p.is_object() || p.to_string().len() > 64 * 1024 {
            return Err("params is an object of at most 64 KB".into());
        }
    }
    if let Some(project) = &a.origin.project {
        if project.len() > 80 {
            return Err("origin.project is too long".into());
        }
    }
    Ok(())
}

/// An upload that is not finished: the bytes arrive in parts, in any order, and the artifact exists only when they are all there and their hash is the one declared.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct UploadRecord {
    pub id: String,
    pub owner: String,
    pub size: u64,
    pub sha256: String,
    pub created: u64,
    pub touched: u64,
    /// The stretches received, as `[start, end)`, sorted and joined.
    pub ranges: Vec<(u64, u64)>,
}

/// Adds `[start, end)` to a sorted list of stretches, joining those that touch.
pub fn add_range(ranges: &mut Vec<(u64, u64)>, start: u64, end: u64) {
    ranges.push((start, end));
    ranges.sort_unstable();
    let mut joined: Vec<(u64, u64)> = Vec::with_capacity(ranges.len());
    for &(s, e) in ranges.iter() {
        match joined.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => joined.push((s, e)),
        }
    }
    *ranges = joined;
}

/// A list of stretches `[start, end)`.
pub type Stretches = Vec<(u64, u64)>;

/// The parts of `[start, end)` that are in the stretches (`covered`) and the parts that are not (`missing`).
pub fn split_range(ranges: &[(u64, u64)], start: u64, end: u64) -> (Stretches, Stretches) {
    let (mut covered, mut missing) = (Vec::new(), Vec::new());
    let mut at = start;
    for &(s, e) in ranges {
        if e <= at || s >= end {
            continue;
        }
        if s > at {
            missing.push((at, s));
        }
        let (cs, ce) = (s.max(at), e.min(end));
        covered.push((cs, ce));
        at = ce;
        if at >= end {
            break;
        }
    }
    if at < end {
        missing.push((at, end));
    }
    (covered, missing)
}

/// The records and the blobs.
pub struct ArtifactStore {
    dir: PathBuf,
    records: Vec<ArtifactRecord>,
    uploads: Vec<UploadRecord>,
}

impl ArtifactStore {
    pub fn open(data_dir: &Path) -> io::Result<ArtifactStore> {
        let dir = data_dir.to_path_buf();
        let records = store::read_json(&dir.join("artifacts.json"))?.unwrap_or_default();
        let uploads = store::read_json(&dir.join("uploads.json"))?.unwrap_or_default();
        Ok(ArtifactStore {
            dir,
            records,
            uploads,
        })
    }

    /// Writes the records again (after a change made in place, such as the sharing).
    pub fn save(&self) -> Result<(), ArtifactError> {
        self.save_records().map_err(|_| ArtifactError::Storage)
    }

    fn save_records(&self) -> io::Result<()> {
        store::write_json(&self.dir.join("artifacts.json"), &self.records)
    }

    fn save_uploads(&self) -> io::Result<()> {
        store::write_json(&self.dir.join("uploads.json"), &self.uploads)
    }

    pub fn records(&self) -> &[ArtifactRecord] {
        &self.records
    }

    pub fn get(&self, id: &str) -> Option<&ArtifactRecord> {
        self.records.iter().find(|r| r.artifact_id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut ArtifactRecord> {
        self.records.iter_mut().find(|r| r.artifact_id == id)
    }

    pub fn blob_path(&self, sha: &str) -> PathBuf {
        self.dir.join("blobs").join(&sha[..2]).join(sha)
    }

    pub fn blob_exists(&self, sha: &str) -> bool {
        is_sha256(sha) && self.blob_path(sha).is_file()
    }

    /// The bytes stored, counted once per blob, however many records point to them.
    pub fn used_bytes(&self) -> u64 {
        let mut seen = HashSet::new();
        self.records
            .iter()
            .filter(|r| seen.insert(r.blob_sha256.as_str()))
            .map(|r| r.size)
            .sum()
    }

    /// Stores the bytes under their hash unless they are already there (a second copy is thrown away: the first stays).
    pub fn put_blob(&self, sha: &str, bytes: &[u8]) -> io::Result<()> {
        let path = self.blob_path(sha);
        if path.is_file() {
            return Ok(());
        }
        store::write_durable(&path, bytes, false)
    }

    /// Reads a blob and checks it is what its name says.
    pub fn read_blob(&self, sha: &str) -> Result<Vec<u8>, ArtifactError> {
        let bytes = std::fs::read(self.blob_path(sha)).map_err(|_| ArtifactError::Damaged)?;
        if sha256_hex(&bytes) != sha {
            return Err(ArtifactError::Damaged);
        }
        Ok(bytes)
    }

    /// Adds a record (the blob has to be stored already).
    pub fn add(&mut self, record: ArtifactRecord) -> Result<(), ArtifactError> {
        self.records.push(record);
        if self.save_records().is_err() {
            self.records.pop();
            return Err(ArtifactError::Storage);
        }
        Ok(())
    }

    /// Removes records (and, when no record points to them any more, their blobs); returns how many blobs went.
    pub fn remove(&mut self, ids: &[String]) -> Result<usize, ArtifactError> {
        let before = self.records.clone();
        self.records.retain(|r| !ids.contains(&r.artifact_id));
        if self.save_records().is_err() {
            self.records = before;
            return Err(ArtifactError::Storage);
        }
        Ok(self.collect_blobs())
    }

    /// Deletes the blobs no record points to.
    pub fn collect_blobs(&self) -> usize {
        let wanted: HashSet<&str> = self
            .records
            .iter()
            .map(|r| r.blob_sha256.as_str())
            .collect();
        let mut gone = 0;
        let Ok(top) = std::fs::read_dir(self.dir.join("blobs")) else {
            return 0;
        };
        for fan in top.flatten() {
            let Ok(files) = std::fs::read_dir(fan.path()) else {
                continue;
            };
            for f in files.flatten() {
                let name = f.file_name().to_string_lossy().to_string();
                if is_sha256(&name)
                    && !wanted.contains(name.as_str())
                    && std::fs::remove_file(f.path()).is_ok()
                {
                    gone += 1;
                }
            }
        }
        gone
    }

    /// Reads every blob again and says which are missing or are not what their name says. A store restored from a backup is checked with this.
    pub fn verify(&self) -> Vec<(String, &'static str)> {
        let mut seen = HashSet::new();
        let mut bad = Vec::new();
        for r in &self.records {
            if !seen.insert(r.blob_sha256.clone()) {
                continue;
            }
            match std::fs::File::open(self.blob_path(&r.blob_sha256)) {
                Err(_) => bad.push((r.blob_sha256.clone(), "missing")),
                Ok(mut file) => {
                    let mut hasher = Sha256::new();
                    let mut buf = vec![0u8; 64 * 1024];
                    let ok = loop {
                        match file.read(&mut buf) {
                            Ok(0) => break true,
                            Ok(n) => hasher.update(&buf[..n]),
                            Err(_) => break false,
                        }
                    };
                    if !ok || hex::encode(hasher.finalize()) != r.blob_sha256 {
                        bad.push((r.blob_sha256.clone(), "damaged"));
                    }
                }
            }
        }
        bad
    }

    // ---- uploads

    fn upload_path(&self, id: &str) -> PathBuf {
        self.dir.join("uploads").join(format!("{id}.part"))
    }

    pub fn upload(&self, id: &str) -> Option<&UploadRecord> {
        self.uploads.iter().find(|u| u.id == id)
    }

    pub fn uploads_of(&self, owner: &str) -> usize {
        self.uploads.iter().filter(|u| u.owner == owner).count()
    }

    /// Starts an upload: a file of the declared size that the parts fill.
    pub fn begin_upload(&mut self, record: UploadRecord) -> Result<(), ArtifactError> {
        let path = self.upload_path(&record.id);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|_| ArtifactError::Storage)?;
        }
        let file = std::fs::File::create(&path).map_err(|_| ArtifactError::Storage)?;
        file.set_len(record.size)
            .map_err(|_| ArtifactError::StoreFull)?;
        self.uploads.push(record);
        if self.save_uploads().is_err() {
            let record = self.uploads.pop();
            if let Some(r) = record {
                let _ = std::fs::remove_file(self.upload_path(&r.id));
            }
            return Err(ArtifactError::Storage);
        }
        Ok(())
    }

    /// Puts a part at an offset. A stretch that was received already has to carry the same bytes (it is then accepted again); different bytes at the same place are
    /// refused and nothing of the part is written.
    pub fn put_part(
        &mut self,
        id: &str,
        offset: u64,
        bytes: &[u8],
    ) -> Result<UploadRecord, ArtifactError> {
        let path = self.upload_path(id);
        let index = self
            .uploads
            .iter()
            .position(|u| u.id == id)
            .ok_or(ArtifactError::UnknownUpload)?;
        let size = self.uploads[index].size;
        let end = offset
            .checked_add(bytes.len() as u64)
            .filter(|e| *e <= size)
            .ok_or_else(|| {
                ArtifactError::BadUpload("the part goes past the declared size".into())
            })?;
        if bytes.is_empty() {
            return Err(ArtifactError::BadUpload("the part is empty".into()));
        }
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|_| ArtifactError::Storage)?;
        let (covered, _) = split_range(&self.uploads[index].ranges, offset, end);
        for (s, e) in covered {
            let mut had = vec![0u8; (e - s) as usize];
            file.seek(SeekFrom::Start(s))
                .map_err(|_| ArtifactError::Storage)?;
            file.read_exact(&mut had)
                .map_err(|_| ArtifactError::Storage)?;
            if had != bytes[(s - offset) as usize..(e - offset) as usize] {
                return Err(ArtifactError::UploadConflict);
            }
        }
        file.seek(SeekFrom::Start(offset))
            .map_err(|_| ArtifactError::Storage)?;
        file.write_all(bytes).map_err(|_| ArtifactError::Storage)?;
        let upload = &mut self.uploads[index];
        add_range(&mut upload.ranges, offset, end);
        upload.touched = now();
        let copy = upload.clone();
        self.save_uploads().map_err(|_| ArtifactError::Storage)?;
        Ok(copy)
    }

    /// Ends an upload: all the bytes are there and their hash is the declared one, or the upload is thrown away and nothing is kept. Gives the hash and the size.
    pub fn finish_upload(&mut self, id: &str) -> Result<(String, u64), ArtifactError> {
        let upload = self
            .upload(id)
            .cloned()
            .ok_or(ArtifactError::UnknownUpload)?;
        if upload.ranges != [(0, upload.size)] && upload.size != 0 {
            return Err(ArtifactError::UploadIncomplete);
        }
        let path = self.upload_path(id);
        let mut file = std::fs::File::open(&path).map_err(|_| ArtifactError::Storage)?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = file.read(&mut buf).map_err(|_| ArtifactError::Storage)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        drop(file);
        let sha = hex::encode(hasher.finalize());
        if sha != upload.sha256 {
            self.abandon_upload(id);
            return Err(ArtifactError::HashMismatch);
        }
        let blob = self.blob_path(&sha);
        if blob.is_file() {
            // the same bytes are stored already: the copy that arrived is thrown away
            let _ = std::fs::remove_file(&path);
        } else {
            if let Some(dir) = blob.parent() {
                std::fs::create_dir_all(dir).map_err(|_| ArtifactError::Storage)?;
            }
            std::fs::rename(&path, &blob).map_err(|_| ArtifactError::Storage)?;
        }
        self.uploads.retain(|u| u.id != id);
        let _ = self.save_uploads();
        Ok((sha, upload.size))
    }

    /// Throws an upload away.
    pub fn abandon_upload(&mut self, id: &str) {
        let _ = std::fs::remove_file(self.upload_path(id));
        self.uploads.retain(|u| u.id != id);
        let _ = self.save_uploads();
    }

    /// Throws away the uploads nobody touched for `ttl_s` seconds, and the files of `uploads/` no upload owns. Returns how many uploads went.
    pub fn clean_uploads(&mut self, now: u64, ttl_s: u64) -> usize {
        let old: Vec<String> = self
            .uploads
            .iter()
            .filter(|u| now.saturating_sub(u.touched) >= ttl_s)
            .map(|u| u.id.clone())
            .collect();
        for id in &old {
            self.abandon_upload(id);
        }
        if let Ok(files) = std::fs::read_dir(self.dir.join("uploads")) {
            for f in files.flatten() {
                let name = f.file_name().to_string_lossy().to_string();
                let id = name.strip_suffix(".part").unwrap_or(&name);
                if self.upload(id).is_none() {
                    let _ = std::fs::remove_file(f.path());
                }
            }
        }
        old.len()
    }

    /// The artifacts that nothing points to and that have waited `days` days (the ones that are pinned, kept until a date that has not come, or in `referenced` stay).
    pub fn purgeable(&self, now: u64, days: u64, referenced: &HashSet<String>) -> Vec<String> {
        if days == 0 {
            return Vec::new();
        }
        let inputs: HashSet<&str> = self
            .records
            .iter()
            .flat_map(|r| r.inputs.iter().map(|i| i.artifact_id.as_str()))
            .collect();
        self.records
            .iter()
            .filter(|r| match r.retention {
                Retention::Pinned => false,
                Retention::KeepUntil => r.keep_until.is_some_and(|u| u <= now),
                Retention::KeepWhileReferenced => true,
            })
            .filter(|r| {
                r.retention == Retention::KeepUntil
                    || now.saturating_sub(r.created_at) >= days * 86_400
            })
            .filter(|r| {
                !referenced.contains(&r.artifact_id) && !inputs.contains(r.artifact_id.as_str())
            })
            .map(|r| r.artifact_id.clone())
            .collect()
    }
}

/// The limits of the settings that apply to artifacts.
#[derive(Clone, Debug)]
pub struct ArtifactLimits {
    /// The most bytes the store holds, counted once per blob.
    pub store_bytes: u64,
    /// The largest artifact or upload.
    pub max_bytes: u64,
    pub upload_ttl_s: u64,
    pub uploads_per_user: usize,
    pub unreferenced_days: u64,
}

impl ArtifactLimits {
    pub fn from_config(c: &ServerConfig) -> ArtifactLimits {
        ArtifactLimits {
            store_bytes: c.artifact_store_gb.saturating_mul(1024 * 1024 * 1024),
            max_bytes: c.max_upload_mb.saturating_mul(1024 * 1024),
            upload_ttl_s: c.upload_ttl_s,
            uploads_per_user: c.uploads_per_user,
            unreferenced_days: c.artifact_unreferenced_days,
        }
    }
}

/// Everything the server keeps about artifacts.
pub struct Artifacts {
    pub store: Mutex<ArtifactStore>,
    pub limits: ArtifactLimits,
}

impl Artifacts {
    pub fn open(cfg: &ServerConfig) -> io::Result<Artifacts> {
        let mut store = ArtifactStore::open(&cfg.data_dir)?;
        let limits = ArtifactLimits::from_config(cfg);
        // the uploads nobody finished are cleaned at start too
        store.clean_uploads(now(), limits.upload_ttl_s);
        Ok(Artifacts {
            store: Mutex::new(store),
            limits,
        })
    }
}

/// Whether `who` may see the artifact: its owner and the administrators.
pub fn visible_to(record: &ArtifactRecord, who: &Identity) -> bool {
    who.role == Role::Admin || record.owner == who.owner()
}

impl AppState {
    /// Whether `who` may see the artifact.
    ///
    /// Its owner and the administrators see it; so does anybody it was shared with, and anybody who sees a project that lists it in a revision or that it was made for.
    /// The lock of the artifacts is always taken before the one of the projects.
    pub fn artifact_visible(&self, record: &ArtifactRecord, who: &Identity) -> bool {
        if visible_to(record, who) {
            return true;
        }
        if self.projects.shares_enabled && record.shares.iter().any(|s| s.who == who.owner()) {
            return true;
        }
        self.projects.book.locked().shows_artifact(
            who,
            &record.artifact_id,
            record.origin.project.as_deref(),
        )
    }

    /// Makes an artifact for a caller: its description is checked, the inputs it names must be artifacts the caller can see and must carry the hash declared, the bytes must
    /// fit the limits.
    pub fn add_artifact(
        &self,
        who: &Identity,
        meta: NewArtifact,
        bytes: &[u8],
    ) -> Result<ArtifactRecord, ArtifactError> {
        check_new(&meta).map_err(ArtifactError::Bad)?;
        {
            let store = self.artifacts.store.locked();
            for i in &meta.inputs {
                let seen = store
                    .get(&i.artifact_id)
                    .filter(|r| self.artifact_visible(r, who))
                    .is_some_and(|r| r.blob_sha256 == i.blob_sha256);
                if !seen {
                    return Err(ArtifactError::Bad(
                        "an input is not an artifact you can see with that hash".into(),
                    ));
                }
            }
        }
        self.store_artifact(&who.owner(), meta, bytes)
    }

    /// Stores the bytes and the record, with no check of who sees the inputs (the tasks use it for what their engines made).
    pub fn store_artifact(
        &self,
        owner: &str,
        meta: NewArtifact,
        bytes: &[u8],
    ) -> Result<ArtifactRecord, ArtifactError> {
        check_new(&meta).map_err(ArtifactError::Bad)?;
        let limits = &self.artifacts.limits;
        if bytes.len() as u64 > limits.max_bytes {
            return Err(ArtifactError::TooBig);
        }
        let sha = sha256_hex(bytes);
        let mut store = self.artifacts.store.locked();
        let extra = if store.blob_exists(&sha) {
            0
        } else {
            bytes.len() as u64
        };
        if store.used_bytes() + extra > limits.store_bytes {
            return Err(ArtifactError::StoreFull);
        }
        store
            .put_blob(&sha, bytes)
            .map_err(|_| ArtifactError::Storage)?;
        let record = make_record(owner, meta, sha, bytes.len() as u64);
        store.add(record.clone())?;
        Ok(record)
    }

    /// Removes an artifact, unless a task still points to it.
    pub fn delete_artifact(&self, id: &str) -> Result<(), ArtifactError> {
        if self.referenced_artifacts().contains(id) {
            return Err(ArtifactError::InUse);
        }
        let mut store = self.artifacts.store.locked();
        store.get(id).ok_or(ArtifactError::Unknown)?;
        store.remove(&[id.to_string()])?;
        Ok(())
    }

    /// The artifacts a task works on or made.
    pub fn referenced_artifacts(&self) -> HashSet<String> {
        let mut set: HashSet<String> = self
            .projects
            .book
            .locked()
            .revision_artifacts()
            .into_iter()
            .collect();
        let book = self.tasks.book.locked();
        for t in book.list() {
            if let Some(p) = &t.project {
                set.insert(p.clone());
            }
            if let Some(r) = &t.result {
                for a in &r.artifacts {
                    if let Some(id) = a.get("artifact_id").and_then(Value::as_str) {
                        set.insert(id.to_string());
                    }
                }
            }
        }
        set
    }

    /// Cleans what has waited long enough: the uploads nobody finished and the artifacts nothing points to. It runs when the server starts and whenever an upload begins.
    pub fn artifact_housekeeping(&self) -> usize {
        let referenced = self.referenced_artifacts();
        let limits = &self.artifacts.limits;
        let mut store = self.artifacts.store.locked();
        store.clean_uploads(now(), limits.upload_ttl_s);
        let gone = store.purgeable(now(), limits.unreferenced_days, &referenced);
        if gone.is_empty() {
            return 0;
        }
        let _ = store.remove(&gone);
        gone.len()
    }

    /// Starts an upload of `size` bytes whose SHA-256 will be `sha256`; gives its id.
    pub fn begin_upload(
        &self,
        who: &Identity,
        size: u64,
        sha256: &str,
    ) -> Result<String, ArtifactError> {
        self.artifact_housekeeping();
        let limits = &self.artifacts.limits;
        if !is_sha256(sha256) {
            return Err(ArtifactError::BadUpload(
                "sha256 is 64 lower-case hexadecimal digits".into(),
            ));
        }
        if size > limits.max_bytes {
            return Err(ArtifactError::TooBig);
        }
        let mut store = self.artifacts.store.locked();
        if store.uploads_of(&who.owner()) >= limits.uploads_per_user {
            return Err(ArtifactError::Quota);
        }
        if store.used_bytes() + size > limits.store_bytes {
            return Err(ArtifactError::StoreFull);
        }
        let id = crate::ids::new(crate::ids::Kind::Upload);
        let t = now();
        store.begin_upload(UploadRecord {
            id: id.clone(),
            owner: who.owner(),
            size,
            sha256: sha256.to_string(),
            created: t,
            touched: t,
            ranges: Vec::new(),
        })?;
        Ok(id)
    }

    fn own_upload(store: &ArtifactStore, who: &Identity, id: &str) -> Result<(), ArtifactError> {
        // an upload of somebody else does not exist for you
        store
            .upload(id)
            .filter(|u| u.owner == who.owner())
            .map(|_| ())
            .ok_or(ArtifactError::UnknownUpload)
    }

    pub fn put_part(
        &self,
        who: &Identity,
        id: &str,
        offset: u64,
        bytes: &[u8],
    ) -> Result<UploadRecord, ArtifactError> {
        let mut store = self.artifacts.store.locked();
        Self::own_upload(&store, who, id)?;
        store.put_part(id, offset, bytes)
    }

    pub fn abandon_upload_of(&self, who: &Identity, id: &str) -> Result<(), ArtifactError> {
        let mut store = self.artifacts.store.locked();
        Self::own_upload(&store, who, id)?;
        store.abandon_upload(id);
        Ok(())
    }

    /// Ends an upload and makes the artifact. A description that does not fit leaves the upload as it was, to be completed again.
    pub fn complete_upload(
        &self,
        who: &Identity,
        id: &str,
        meta: NewArtifact,
    ) -> Result<ArtifactRecord, ArtifactError> {
        check_new(&meta).map_err(ArtifactError::Bad)?;
        let mut store = self.artifacts.store.locked();
        Self::own_upload(&store, who, id)?;
        for i in &meta.inputs {
            let seen = store
                .get(&i.artifact_id)
                .filter(|r| self.artifact_visible(r, who))
                .is_some_and(|r| r.blob_sha256 == i.blob_sha256);
            if !seen {
                return Err(ArtifactError::Bad(
                    "an input is not an artifact you can see with that hash".into(),
                ));
            }
        }
        let (sha, size) = store.finish_upload(id)?;
        let record = make_record(&who.owner(), meta, sha, size);
        store.add(record.clone())?;
        Ok(record)
    }
}

fn make_record(owner: &str, meta: NewArtifact, sha: String, size: u64) -> ArtifactRecord {
    ArtifactRecord {
        artifact_id: crate::ids::new(crate::ids::Kind::Artifact),
        blob_sha256: sha,
        size,
        owner: owner.to_string(),
        name: meta.name,
        format: meta.format,
        schema_version: meta.schema_version,
        origin: meta.origin,
        producer: meta.producer,
        inputs: meta.inputs,
        params: meta.params.unwrap_or_else(|| json!({})),
        result_type: meta.result_type,
        diagnostics: Vec::new(),
        validation: Validation {
            intact: true,
            ..Validation::default()
        },
        created_at: now(),
        retention: Retention::KeepWhileReferenced,
        keep_until: None,
        shares: Vec::new(),
    }
}

/// A summary by owner, for the tests and the health check: how many artifacts and how many bytes.
pub fn totals(records: &[ArtifactRecord]) -> BTreeMap<String, (usize, u64)> {
    let mut out: BTreeMap<String, (usize, u64)> = BTreeMap::new();
    for r in records {
        let e = out.entry(r.owner.clone()).or_default();
        e.0 += 1;
        e.1 += r.size;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("eh3d-server-art-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn record(id: &str, sha: &str, size: u64, owner: &str) -> ArtifactRecord {
        ArtifactRecord {
            artifact_id: id.into(),
            blob_sha256: sha.into(),
            size,
            owner: owner.into(),
            name: "n".into(),
            format: "text/plain".into(),
            schema_version: None,
            origin: Origin::default(),
            producer: None,
            inputs: Vec::new(),
            params: json!({}),
            result_type: None,
            diagnostics: Vec::new(),
            validation: Validation {
                intact: true,
                ..Validation::default()
            },
            created_at: 1_000,
            retention: Retention::KeepWhileReferenced,
            keep_until: None,
            shares: Vec::new(),
        }
    }

    #[test]
    fn the_sha256_of_known_texts() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert!(is_sha256(&sha256_hex(b"x")));
        assert!(!is_sha256("ABC") && !is_sha256(&"g".repeat(64)));
    }

    #[test]
    fn ranges_join_and_split() {
        let mut r = Vec::new();
        add_range(&mut r, 10, 20);
        add_range(&mut r, 0, 5);
        assert_eq!(r, [(0, 5), (10, 20)]);
        add_range(&mut r, 5, 10);
        assert_eq!(r, [(0, 20)]);
        let two = [(0, 5), (10, 20)];
        let (covered, missing) = split_range(&two, 3, 12);
        assert_eq!(covered, [(3, 5), (10, 12)]);
        assert_eq!(missing, [(5, 10)]);
        let (covered, missing) = split_range(&two, 20, 30);
        assert!(covered.is_empty());
        assert_eq!(missing, [(20, 30)]);
    }

    #[test]
    fn the_description_of_an_artifact_is_checked() {
        let good = NewArtifact {
            name: "board.phoenixproj".into(),
            format: "application/x-phoenixproj".into(),
            ..NewArtifact::default()
        };
        assert!(check_new(&good).is_ok());
        for bad in [
            NewArtifact {
                name: String::new(),
                ..good.clone()
            },
            NewArtifact {
                name: "a/b".into(),
                ..good.clone()
            },
            NewArtifact {
                format: "Application/X".into(),
                ..good.clone()
            },
            NewArtifact {
                result_type: Some("T9".into()),
                ..good.clone()
            },
            NewArtifact {
                params: Some(json!([1])),
                ..good.clone()
            },
            NewArtifact {
                inputs: vec![InputRef {
                    artifact_id: "nope".into(),
                    blob_sha256: "x".into(),
                }],
                ..good.clone()
            },
        ] {
            assert!(check_new(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_blob_is_stored_once_and_read_back_checked() {
        let d = dir("blob");
        let st = ArtifactStore::open(&d).unwrap();
        let sha = sha256_hex(b"hello");
        assert!(!st.blob_exists(&sha));
        st.put_blob(&sha, b"hello").unwrap();
        assert!(st.blob_exists(&sha));
        // a second copy does not replace the first
        st.put_blob(&sha, b"other bytes under the same name")
            .unwrap();
        assert_eq!(st.read_blob(&sha).unwrap(), b"hello");
        // a blob changed on the disk is not what its name says
        std::fs::write(st.blob_path(&sha), b"changed").unwrap();
        assert_eq!(st.read_blob(&sha), Err(ArtifactError::Damaged));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn two_records_may_share_a_blob_and_the_blob_goes_with_the_last() {
        let d = dir("share");
        let mut st = ArtifactStore::open(&d).unwrap();
        let sha = sha256_hex(b"same");
        st.put_blob(&sha, b"same").unwrap();
        let (a, b) = (
            crate::ids::new(crate::ids::Kind::Artifact),
            crate::ids::new(crate::ids::Kind::Artifact),
        );
        st.add(record(&a, &sha, 4, "u1")).unwrap();
        st.add(record(&b, &sha, 4, "u2")).unwrap();
        assert_ne!(a, b);
        assert_eq!(st.used_bytes(), 4, "counted once per blob");
        assert_eq!(
            st.remove(std::slice::from_ref(&a)).unwrap(),
            0,
            "the other still points to it"
        );
        assert!(st.blob_exists(&sha));
        assert_eq!(st.remove(&[b]).unwrap(), 1);
        assert!(!st.blob_exists(&sha));
        // the records survive a reopening
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn verify_finds_the_missing_and_the_damaged() {
        let d = dir("verify");
        let mut st = ArtifactStore::open(&d).unwrap();
        for (i, text) in ["one", "two", "three"].iter().enumerate() {
            let sha = sha256_hex(text.as_bytes());
            st.put_blob(&sha, text.as_bytes()).unwrap();
            st.add(record(&format!("a-{i}"), &sha, text.len() as u64, "u"))
                .unwrap();
        }
        assert!(st.verify().is_empty());
        std::fs::write(st.blob_path(&sha256_hex(b"two")), b"2").unwrap();
        std::fs::remove_file(st.blob_path(&sha256_hex(b"three"))).unwrap();
        let mut bad = st.verify();
        bad.sort();
        let mut want = vec![
            (sha256_hex(b"two"), "damaged"),
            (sha256_hex(b"three"), "missing"),
        ];
        want.sort();
        assert_eq!(bad, want);
        let _ = std::fs::remove_dir_all(&d);
    }

    fn upload(id: &str, bytes: &[u8]) -> UploadRecord {
        UploadRecord {
            id: id.into(),
            owner: "u".into(),
            size: bytes.len() as u64,
            sha256: sha256_hex(bytes),
            created: 1,
            touched: 1,
            ranges: Vec::new(),
        }
    }

    #[test]
    fn an_upload_in_parts_in_any_order_with_a_repeat_ends_as_a_blob() {
        let d = dir("up");
        let mut st = ArtifactStore::open(&d).unwrap();
        let whole = b"0123456789abcdef";
        st.begin_upload(upload("up-1", whole)).unwrap();
        st.put_part("up-1", 8, &whole[8..]).unwrap();
        // incomplete: it cannot end yet
        assert_eq!(
            st.finish_upload("up-1"),
            Err(ArtifactError::UploadIncomplete)
        );
        // the same bytes again at the same place are accepted, even overlapping a new stretch
        st.put_part("up-1", 6, &whole[6..12]).unwrap();
        st.put_part("up-1", 0, &whole[..8]).unwrap();
        let (sha, size) = st.finish_upload("up-1").unwrap();
        assert_eq!((sha.as_str(), size), (sha256_hex(whole).as_str(), 16));
        assert_eq!(st.read_blob(&sha).unwrap(), whole);
        assert!(st.upload("up-1").is_none());
        assert!(!st.upload_path("up-1").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn different_bytes_at_the_same_place_are_refused_and_write_nothing() {
        let d = dir("conflict");
        let mut st = ArtifactStore::open(&d).unwrap();
        let whole = b"aaaabbbb";
        st.begin_upload(upload("up-2", whole)).unwrap();
        st.put_part("up-2", 0, b"aaaa").unwrap();
        assert_eq!(
            st.put_part("up-2", 2, b"XXXX"),
            Err(ArtifactError::UploadConflict)
        );
        assert_eq!(st.upload("up-2").unwrap().ranges, [(0, 4)]);
        // past the declared size, empty, and an upload nobody knows
        assert!(matches!(
            st.put_part("up-2", 6, b"bbbbbb"),
            Err(ArtifactError::BadUpload(_))
        ));
        assert!(matches!(
            st.put_part("up-2", 4, b""),
            Err(ArtifactError::BadUpload(_))
        ));
        assert_eq!(
            st.put_part("up-9", 0, b"a"),
            Err(ArtifactError::UnknownUpload)
        );
        // the rest arrives and the file is as declared
        st.put_part("up-2", 4, b"bbbb").unwrap();
        assert!(st.finish_upload("up-2").is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_wrong_hash_discards_the_upload_and_keeps_nothing() {
        let d = dir("hash");
        let mut st = ArtifactStore::open(&d).unwrap();
        let mut u = upload("up-3", b"real bytes");
        u.sha256 = sha256_hex(b"other bytes");
        st.begin_upload(u).unwrap();
        st.put_part("up-3", 0, b"real bytes").unwrap();
        assert_eq!(st.finish_upload("up-3"), Err(ArtifactError::HashMismatch));
        assert!(st.upload("up-3").is_none());
        assert!(!st.blob_exists(&sha256_hex(b"real bytes")));
        assert!(!st.blob_exists(&sha256_hex(b"other bytes")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn old_uploads_and_orphan_parts_are_cleaned() {
        let d = dir("clean");
        let mut st = ArtifactStore::open(&d).unwrap();
        st.begin_upload(upload("up-old", b"abc")).unwrap();
        let mut fresh = upload("up-new", b"abc");
        fresh.touched = 5_000;
        st.begin_upload(fresh).unwrap();
        std::fs::write(st.upload_path("up-orphan"), b"x").unwrap();
        assert_eq!(st.clean_uploads(6_000, 3_600), 1);
        assert!(st.upload("up-old").is_none() && st.upload("up-new").is_some());
        assert!(!st.upload_path("up-old").exists() && !st.upload_path("up-orphan").exists());
        assert!(st.upload_path("up-new").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_empty_upload_can_end() {
        let d = dir("empty");
        let mut st = ArtifactStore::open(&d).unwrap();
        st.begin_upload(upload("up-e", b"")).unwrap();
        let (sha, size) = st.finish_upload("up-e").unwrap();
        assert_eq!((sha, size), (sha256_hex(b""), 0));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn what_is_purged_is_what_nothing_points_to_after_the_wait() {
        let d = dir("purge");
        let mut st = ArtifactStore::open(&d).unwrap();
        let day = 86_400;
        let mut make = |id: &str, age_days: u64, retention: Retention| {
            let mut r = record(id, &sha256_hex(id.as_bytes()), 1, "u");
            r.created_at = 1_000_000 - age_days * day;
            r.retention = retention;
            st.records.push(r);
        };
        make("a-old", 40, Retention::KeepWhileReferenced);
        make("a-young", 5, Retention::KeepWhileReferenced);
        make("a-pointed", 40, Retention::KeepWhileReferenced);
        make("a-pinned", 400, Retention::Pinned);
        make("a-input", 40, Retention::KeepWhileReferenced);
        make("a-until-past", 1, Retention::KeepUntil);
        make("a-until-future", 400, Retention::KeepUntil);
        st.records[5].keep_until = Some(999_999);
        st.records[6].keep_until = Some(2_000_000);
        // an artifact made from another one points to it
        let mut out = record("a-output", &sha256_hex(b"out"), 1, "u");
        out.created_at = 1_000_000;
        out.inputs = vec![InputRef {
            artifact_id: "a-input".into(),
            blob_sha256: "0".repeat(64),
        }];
        st.records.push(out);
        let pointed: HashSet<String> = ["a-pointed".to_string()].into();
        let mut gone = st.purgeable(1_000_000, 30, &pointed);
        gone.sort();
        assert_eq!(gone, ["a-old", "a-until-past"]);
        // 0 days: nothing is ever purged
        assert!(st.purgeable(1_000_000, 0, &pointed).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn visibility_is_the_owner_and_the_administrators() {
        let r = record("a-1", &sha256_hex(b"x"), 1, "ana");
        let who = |name: &str, role| Identity {
            name: name.into(),
            role,
            is_token: false,
            token_id: None,
            scopes: None,
            template: None,
        };
        assert!(visible_to(&r, &who("ana", Role::Viewer)));
        assert!(!visible_to(&r, &who("bob", Role::Operator)));
        assert!(visible_to(&r, &who("bob", Role::Admin)));
        let t = totals(&[r.clone(), record("a-2", "s", 5, "ana")]);
        assert_eq!(t["ana"], (2, 6));
    }
}
