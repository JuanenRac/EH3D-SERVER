//! The project store: the projects and the files of every application, kept by the server.
//!
//! A project belongs to one application (`studio`, `android`, `pcb`...) and to a person. It holds files with a name that may have folders in it (`layers/top.gbr`); a file
//! is never overwritten: every write is a new **version** with its SHA-256, and the old versions stay (up to `project_versions_kept`). A write over an existing file must say
//! which version it was made from (`If-Match`, the SHA-256 of the version) so two people never overwrite each other without knowing; a **lock** with an expiry tells the others
//! that somebody is editing. The project can be imported from and exported to the `.eh3dproj` container of EH3D-COMMON.
//!
//! On the disk, under the data folder: `pstore/apps/<app>/<project>/meta.json` (the record, written through a temporary file) and `blobs/<sha256>` (the bytes of a version,
//! written the same way and checked against their hash every time they are read).

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use eh3d_container::spec::{INTEGRITY_ENTRY, PROJECT_ENTRY};
use eh3d_container::{
    check_name, collision_key, AtomicOptions, Container, ContainerSpec, ContainerWriter, Policy,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::auth::{Identity, Role};
use crate::config::ServerConfig;
use crate::ids::{self, Kind};
use crate::store;

/// The longest name of a file, in bytes (the limit of the `.eh3dproj` container).
const MAX_FILE_NAME: usize = 240;

/// The limits of the store, from `server.cfg`.
#[derive(Clone, Debug)]
pub struct Limits {
    pub max_file: u64,
    pub max_project: u64,
    pub max_files: usize,
    pub max_projects: usize,
    pub versions_kept: usize,
    pub lock_ttl: u64,
    pub require_if_match: bool,
    pub shared: bool,
}

impl Limits {
    pub fn from_config(cfg: &ServerConfig) -> Limits {
        Limits {
            max_file: cfg.project_max_file_mb * 1024 * 1024,
            max_project: cfg.project_max_mb * 1024 * 1024,
            max_files: cfg.project_max_files,
            max_projects: cfg.project_max_projects,
            versions_kept: cfg.project_versions_kept,
            lock_ttl: cfg.project_lock_ttl_s,
            require_if_match: cfg.project_require_if_match,
            shared: cfg.project_shared,
        }
    }
}

/// One version of a file.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Version {
    /// 1 for the first, then up by one (also after old versions were dropped).
    pub n: u64,
    pub sha256: String,
    pub size: u64,
    pub at: u64,
    pub by: String,
    #[serde(default)]
    pub comment: String,
}

impl Version {
    pub fn view(&self) -> Value {
        json!({
            "version": self.n, "sha256": self.sha256, "size": self.size, "at": self.at,
            "by": self.by, "comment": self.comment,
        })
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct FileEntry {
    pub versions: Vec<Version>,
    /// Removed from the list of files; the versions stay and a new write brings it back.
    #[serde(default)]
    pub deleted: bool,
}

impl FileEntry {
    fn current(&self) -> Option<&Version> {
        if self.deleted {
            None
        } else {
            self.versions.last()
        }
    }
}

/// Who is editing a project, until when.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Lock {
    /// The owner key of the person (`Identity::owner`).
    pub holder: String,
    /// The name to show.
    pub holder_name: String,
    pub since: u64,
    pub expires: u64,
}

impl Lock {
    pub fn view(&self) -> Value {
        json!({ "holder": self.holder_name, "since": self.since, "expires": self.expires })
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ProjectMeta {
    pub id: String,
    pub app: String,
    pub name: String,
    pub owner: String,
    pub created: u64,
    pub updated: u64,
    /// Goes up with every change; it is the ETag of the project.
    pub rev: u64,
    #[serde(default)]
    pub deleted: bool,
    pub files: BTreeMap<String, FileEntry>,
    #[serde(default)]
    pub lock: Option<Lock>,
}

impl ProjectMeta {
    pub fn etag(&self) -> String {
        format!("\"{}\"", self.rev)
    }

    /// The lock, when it has not expired.
    pub fn live_lock(&self, at: u64) -> Option<&Lock> {
        self.lock.as_ref().filter(|l| l.expires > at)
    }

    pub fn file_list(&self) -> Vec<Value> {
        self.files
            .iter()
            .filter_map(|(name, f)| {
                f.current().map(|v| {
                    json!({
                        "name": name, "version": v.n, "sha256": v.sha256, "size": v.size,
                        "at": v.at, "by": v.by,
                    })
                })
            })
            .collect()
    }

    /// The bytes of the project: every distinct version counted once.
    pub fn bytes(&self) -> u64 {
        let mut seen: HashMap<&str, u64> = HashMap::new();
        for v in self.files.values().flat_map(|f| f.versions.iter()) {
            seen.insert(v.sha256.as_str(), v.size);
        }
        seen.values().sum()
    }

    pub fn view(&self, at: u64) -> Value {
        json!({
            "id": self.id, "app": self.app, "name": self.name, "owner": self.owner,
            "created": self.created, "updated": self.updated, "etag": self.etag(),
            "deleted": self.deleted, "bytes": self.bytes(),
            "lock": self.live_lock(at).map(Lock::view),
            "files": self.file_list(),
        })
    }

    pub fn summary(&self, at: u64) -> Value {
        json!({
            "id": self.id, "app": self.app, "name": self.name, "owner": self.owner,
            "created": self.created, "updated": self.updated, "etag": self.etag(),
            "deleted": self.deleted, "files": self.file_list().len(),
            "locked": self.live_lock(at).is_some(),
        })
    }
}

#[derive(Debug, PartialEq)]
pub enum PError {
    BadApp,
    BadName,
    BadFilename(String),
    FileTooBig,
    Quota(&'static str),
    Unknown,
    UnknownFile,
    UnknownVersion,
    Locked(String),
    NotLocked,
    PreconditionRequired,
    PreconditionFailed,
    BadContainer(String),
    Damaged,
    Forbidden,
    Storage,
}

impl PError {
    pub fn code(&self) -> &'static str {
        match self {
            PError::BadApp => "bad_app",
            PError::BadName => "bad_name",
            PError::BadFilename(_) => "bad_filename",
            PError::FileTooBig => "file_too_big",
            PError::Quota(_) => "project_quota",
            PError::Unknown => "unknown_project",
            PError::UnknownFile => "unknown_file",
            PError::UnknownVersion => "unknown_version",
            PError::Locked(_) => "locked",
            PError::NotLocked => "not_locked",
            PError::PreconditionRequired => "precondition_required",
            PError::PreconditionFailed => "precondition_failed",
            PError::BadContainer(_) => "bad_container",
            PError::Damaged => "blob_damaged",
            PError::Forbidden => "forbidden",
            PError::Storage => "storage",
        }
    }

    pub fn status(&self) -> u16 {
        match self {
            PError::BadApp | PError::BadName | PError::BadFilename(_) => 422,
            PError::BadContainer(_) => 422,
            PError::FileTooBig => 413,
            PError::Quota(_) => 507,
            PError::Unknown | PError::UnknownFile | PError::UnknownVersion => 404,
            PError::Locked(_) => 423,
            PError::NotLocked => 409,
            PError::PreconditionRequired => 428,
            PError::PreconditionFailed => 412,
            PError::Forbidden => 403,
            PError::Damaged | PError::Storage => 500,
        }
    }

    pub fn detail(&self) -> String {
        match self {
            PError::BadFilename(why) | PError::BadContainer(why) => why.clone(),
            PError::Quota(why) => (*why).to_string(),
            PError::Locked(who) => format!("{who} is editing this project"),
            _ => String::new(),
        }
    }
}

/// Checks the name of a file of a project: the strict rule of the `.eh3dproj` container (relative, forward slashes, no `.` or `..`, no drive letters, no control
/// characters, normal form C, no part that ends with a dot or a space, no reserved device name of Windows), and not the name the container keeps for itself.
pub fn check_file_name(name: &str) -> Result<(), PError> {
    if name == INTEGRITY_ENTRY {
        return Err(PError::BadFilename(format!(
            "{INTEGRITY_ENTRY} is written by the container"
        )));
    }
    check_name(Policy::Strict, name, MAX_FILE_NAME).map_err(|v| PError::BadFilename(v.to_string()))
}

/// Whether an `If-Match` value (quoted or not, weak or strong) names the version with this hash; `*` names any.
pub fn tag_matches(tag: &str, sha256: &str) -> bool {
    let tag = tag.trim();
    if tag == "*" {
        return true;
    }
    let tag = tag.strip_prefix("W/").unwrap_or(tag);
    tag.trim_matches('"') == sha256
}

fn sha_of(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn project_dir(root: &Path, m: &ProjectMeta) -> PathBuf {
    root.join("apps").join(&m.app).join(&m.id)
}

fn blob_path(root: &Path, m: &ProjectMeta, sha: &str) -> PathBuf {
    project_dir(root, m).join("blobs").join(sha)
}

fn save(root: &Path, m: &ProjectMeta) -> Result<(), PError> {
    store::write_json(&project_dir(root, m).join("meta.json"), m).map_err(|_| PError::Storage)
}

/// Reads the bytes of a version and checks them against their hash.
pub fn read_verified(path: &Path, version: &Version) -> Result<Vec<u8>, PError> {
    let bytes = std::fs::read(path).map_err(|_| PError::Damaged)?;
    if bytes.len() as u64 != version.size || sha_of(&bytes) != version.sha256 {
        return Err(PError::Damaged);
    }
    Ok(bytes)
}

fn check_lock(m: &ProjectMeta, who: &Identity, at: u64) -> Result<(), PError> {
    match m.live_lock(at) {
        Some(l) if l.holder != who.owner() => Err(PError::Locked(l.holder_name.clone())),
        _ => Ok(()),
    }
}

/// What a write did.
#[derive(Debug)]
pub struct PutOutcome {
    pub version: Version,
    /// False when the bytes were the ones of the current version and nothing was written.
    pub created: bool,
}

/// Adds a version of `name` to `work`: the quotas, the bytes on the disk, the record and the dropping of the versions past the limit. The caller saves.
#[allow(clippy::too_many_arguments)]
fn apply_put(
    root: &Path,
    limits: &Limits,
    work: &mut ProjectMeta,
    who: &Identity,
    name: &str,
    bytes: &[u8],
    comment: &str,
    at: u64,
) -> Result<PutOutcome, PError> {
    check_file_name(name)?;
    if bytes.len() as u64 > limits.max_file {
        return Err(PError::FileTooBig);
    }
    let key = collision_key(name);
    if work
        .files
        .keys()
        .any(|k| k != name && collision_key(k) == key)
    {
        return Err(PError::BadFilename(
            "a file whose name differs only in letter case or form already exists".into(),
        ));
    }
    let sha = sha_of(bytes);
    let current = work.files.get(name).and_then(|f| f.current()).cloned();
    if let Some(cur) = &current {
        if cur.sha256 == sha {
            return Ok(PutOutcome {
                version: cur.clone(),
                created: false,
            });
        }
    } else if work.files.values().filter(|f| !f.deleted).count() >= limits.max_files {
        return Err(PError::Quota("the project would pass project_max_files"));
    }
    let known = work
        .files
        .values()
        .flat_map(|f| f.versions.iter())
        .any(|v| v.sha256 == sha);
    if !known && work.bytes() + bytes.len() as u64 > limits.max_project {
        return Err(PError::Quota("the project would pass project_max_mb"));
    }
    let path = blob_path(root, work, &sha);
    if !path.is_file() {
        store::write_durable(&path, bytes, false).map_err(|_| PError::Storage)?;
    }
    let version = Version {
        n: work
            .files
            .get(name)
            .and_then(|f| f.versions.last())
            .map_or(1, |v| v.n + 1),
        sha256: sha,
        size: bytes.len() as u64,
        at,
        by: who.name.clone(),
        comment: comment.chars().take(200).collect(),
    };
    let entry = work.files.entry(name.to_string()).or_default();
    entry.versions.push(version.clone());
    entry.deleted = false;
    let mut dropped: Vec<String> = Vec::new();
    if limits.versions_kept > 0 && entry.versions.len() > limits.versions_kept {
        let extra = entry.versions.len() - limits.versions_kept;
        dropped = entry.versions.drain(..extra).map(|v| v.sha256).collect();
    }
    work.rev += 1;
    work.updated = at;
    for sha in dropped {
        let used = work
            .files
            .values()
            .flat_map(|f| f.versions.iter())
            .any(|v| v.sha256 == sha);
        if !used {
            let _ = std::fs::remove_file(blob_path(root, work, &sha));
        }
    }
    Ok(PutOutcome {
        version,
        created: true,
    })
}

/// Removes from the disk the bytes no version points to (what a refused import left behind).
fn sweep(root: &Path, m: &ProjectMeta) {
    let dir = project_dir(root, m).join("blobs");
    let Ok(list) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in list.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let used = m
            .files
            .values()
            .flat_map(|f| f.versions.iter())
            .any(|v| v.sha256 == name);
        if !used {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

pub struct ProjectStore {
    pub book: Mutex<PBook>,
}

impl ProjectStore {
    pub fn open(cfg: &ServerConfig) -> io::Result<ProjectStore> {
        Ok(ProjectStore {
            book: Mutex::new(PBook::open(cfg)?),
        })
    }
}

pub struct PBook {
    root: PathBuf,
    pub limits: Limits,
    projects: HashMap<String, ProjectMeta>,
}

impl PBook {
    pub fn open(cfg: &ServerConfig) -> io::Result<PBook> {
        let root = cfg.data_dir.join("pstore");
        let mut projects = HashMap::new();
        if let Ok(apps) = std::fs::read_dir(root.join("apps")) {
            for app in apps.flatten() {
                let Ok(dirs) = std::fs::read_dir(app.path()) else {
                    continue;
                };
                for dir in dirs.flatten() {
                    if let Some(meta) =
                        store::read_json::<ProjectMeta>(&dir.path().join("meta.json"))?
                    {
                        projects.insert(meta.id.clone(), meta);
                    }
                }
            }
        }
        Ok(PBook {
            root,
            limits: Limits::from_config(cfg),
            projects,
        })
    }

    /// The owner of a project, whatever application it is in.
    pub fn owner_of(&self, id: &str) -> Option<String> {
        self.projects.get(id).map(|m| m.owner.clone())
    }

    /// The folder for the temporary files of the exports.
    pub fn scratch(&self) -> PathBuf {
        self.root.join("tmp")
    }

    fn may_see(&self, m: &ProjectMeta, who: &Identity) -> bool {
        if m.deleted && who.role != Role::Admin {
            return false;
        }
        who.role == Role::Admin || m.owner == who.owner() || self.limits.shared
    }

    /// The project of this application the caller may see; for anybody else it does not exist.
    pub fn visible(&self, app: &str, id: &str, who: &Identity) -> Result<&ProjectMeta, PError> {
        self.projects
            .get(id)
            .filter(|m| m.app == app && self.may_see(m, who))
            .ok_or(PError::Unknown)
    }

    fn visible_copy(&self, app: &str, id: &str, who: &Identity) -> Result<ProjectMeta, PError> {
        self.visible(app, id, who).cloned()
    }

    fn commit(&mut self, work: ProjectMeta) -> Result<(), PError> {
        save(&self.root, &work)?;
        self.projects.insert(work.id.clone(), work);
        Ok(())
    }

    /// The projects of the application the caller sees, the last changed first; `q` keeps those whose name has it (any letter case).
    pub fn list(
        &self,
        app: &str,
        who: &Identity,
        q: &str,
        offset: usize,
        limit: usize,
    ) -> (usize, Vec<&ProjectMeta>) {
        let needle = q.to_lowercase();
        let mut found: Vec<&ProjectMeta> = self
            .projects
            .values()
            .filter(|m| m.app == app && self.may_see(m, who))
            .filter(|m| needle.is_empty() || m.name.to_lowercase().contains(&needle))
            .collect();
        found.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| a.id.cmp(&b.id)));
        let total = found.len();
        let page = found.into_iter().skip(offset).take(limit).collect();
        (total, page)
    }

    pub fn create(
        &mut self,
        app: &str,
        who: &Identity,
        name: &str,
        at: u64,
    ) -> Result<ProjectMeta, PError> {
        if !crate::apps::valid_label(name, 80) {
            return Err(PError::BadName);
        }
        let mine = self.projects.values().filter(|m| m.app == app).count();
        if mine >= self.limits.max_projects {
            return Err(PError::Quota(
                "the application would pass project_max_projects",
            ));
        }
        let meta = ProjectMeta {
            id: ids::new(Kind::Project),
            app: app.to_string(),
            name: name.trim().to_string(),
            owner: who.owner(),
            created: at,
            updated: at,
            rev: 1,
            deleted: false,
            files: BTreeMap::new(),
            lock: None,
        };
        self.commit(meta.clone())?;
        Ok(meta)
    }

    /// The precondition of a change of the project itself or of a file: the tag it was read with.
    fn precondition(&self, given: Option<&str>, current: &str) -> Result<(), PError> {
        match given {
            None if self.limits.require_if_match => Err(PError::PreconditionRequired),
            Some(tag) if !tag_matches(tag, current) => Err(PError::PreconditionFailed),
            _ => Ok(()),
        }
    }

    pub fn rename(
        &mut self,
        app: &str,
        id: &str,
        who: &Identity,
        name: &str,
        if_match: Option<&str>,
        at: u64,
    ) -> Result<ProjectMeta, PError> {
        let mut work = self.visible_copy(app, id, who)?;
        if !crate::apps::valid_label(name, 80) {
            return Err(PError::BadName);
        }
        check_lock(&work, who, at)?;
        self.precondition(if_match, &work.rev.to_string())?;
        work.name = name.trim().to_string();
        work.rev += 1;
        work.updated = at;
        self.commit(work.clone())?;
        Ok(work)
    }

    /// Whether the caller may delete the project: its owner and the administrators.
    pub fn is_manager(m: &ProjectMeta, who: &Identity) -> bool {
        who.role == Role::Admin || m.owner == who.owner()
    }

    pub fn soft_delete(
        &mut self,
        app: &str,
        id: &str,
        who: &Identity,
        at: u64,
    ) -> Result<(), PError> {
        let mut work = self.visible_copy(app, id, who)?;
        if !Self::is_manager(&work, who) {
            return Err(PError::Forbidden);
        }
        check_lock(&work, who, at)?;
        work.deleted = true;
        work.lock = None;
        work.rev += 1;
        work.updated = at;
        self.commit(work)
    }

    /// Removes a hidden project and its bytes for good.
    pub fn purge(&mut self, app: &str, id: &str, who: &Identity) -> Result<(), PError> {
        let work = self.visible_copy(app, id, who)?;
        if !work.deleted {
            return Err(PError::Unknown);
        }
        let _ = std::fs::remove_dir_all(project_dir(&self.root, &work));
        self.projects.remove(id);
        Ok(())
    }

    pub fn take_lock(
        &mut self,
        app: &str,
        id: &str,
        who: &Identity,
        ttl: Option<u64>,
        at: u64,
    ) -> Result<Lock, PError> {
        let mut work = self.visible_copy(app, id, who)?;
        check_lock(&work, who, at)?;
        let ttl = ttl
            .unwrap_or(self.limits.lock_ttl)
            .clamp(1, self.limits.lock_ttl);
        let since = work.live_lock(at).map_or(at, |l| l.since);
        let lock = Lock {
            holder: who.owner(),
            holder_name: who.name.clone(),
            since,
            expires: at + ttl,
        };
        work.lock = Some(lock.clone());
        self.commit(work)?;
        Ok(lock)
    }

    /// Lets the lock go. The holder may; an administrator may take it away from somebody else with `force`.
    pub fn release_lock(
        &mut self,
        app: &str,
        id: &str,
        who: &Identity,
        force: bool,
        at: u64,
    ) -> Result<(), PError> {
        let mut work = self.visible_copy(app, id, who)?;
        let held = work.live_lock(at).cloned();
        match held {
            None => return Err(PError::NotLocked),
            Some(l) if l.holder != who.owner() && !(force && who.role == Role::Admin) => {
                return Err(PError::Locked(l.holder_name))
            }
            Some(_) => {}
        }
        work.lock = None;
        self.commit(work)
    }

    /// Writes a file as a new version. A file that exists needs `if_match`, the hash of the version it was made from, unless the settings say otherwise.
    #[allow(clippy::too_many_arguments)]
    pub fn put_file(
        &mut self,
        app: &str,
        id: &str,
        who: &Identity,
        name: &str,
        bytes: &[u8],
        if_match: Option<&str>,
        comment: &str,
        at: u64,
    ) -> Result<PutOutcome, PError> {
        let mut work = self.visible_copy(app, id, who)?;
        check_lock(&work, who, at)?;
        let current = work.files.get(name).and_then(|f| f.current()).cloned();
        if let Some(cur) = &current {
            self.precondition(if_match, &cur.sha256)?;
        } else if if_match.is_some() {
            return Err(PError::PreconditionFailed);
        }
        let outcome = apply_put(
            &self.root,
            &self.limits,
            &mut work,
            who,
            name,
            bytes,
            comment,
            at,
        )?;
        if outcome.created {
            self.commit(work)?;
        }
        Ok(outcome)
    }

    /// Removes a file from the list; its versions stay.
    pub fn delete_file(
        &mut self,
        app: &str,
        id: &str,
        who: &Identity,
        name: &str,
        if_match: Option<&str>,
        at: u64,
    ) -> Result<(), PError> {
        let mut work = self.visible_copy(app, id, who)?;
        check_lock(&work, who, at)?;
        let cur = work
            .files
            .get(name)
            .and_then(|f| f.current())
            .cloned()
            .ok_or(PError::UnknownFile)?;
        self.precondition(if_match, &cur.sha256)?;
        if let Some(entry) = work.files.get_mut(name) {
            entry.deleted = true;
        }
        work.rev += 1;
        work.updated = at;
        self.commit(work)
    }

    /// Writes many files at once, each as a new version (the ones that did not change are left). Everything or nothing.
    pub fn import(
        &mut self,
        app: &str,
        id: &str,
        who: &Identity,
        entries: Vec<(String, Vec<u8>)>,
        at: u64,
    ) -> Result<Vec<(String, Version, bool)>, PError> {
        let mut work = self.visible_copy(app, id, who)?;
        check_lock(&work, who, at)?;
        let mut done = Vec::new();
        for (name, bytes) in &entries {
            match apply_put(
                &self.root,
                &self.limits,
                &mut work,
                who,
                name,
                bytes,
                "import",
                at,
            ) {
                Ok(o) => done.push((name.clone(), o.version, o.created)),
                Err(e) => {
                    if let Some(kept) = self.projects.get(id) {
                        sweep(&self.root, kept);
                    }
                    return Err(e);
                }
            }
        }
        if done.iter().any(|(_, _, created)| *created) {
            self.commit(work)?;
        }
        Ok(done)
    }

    /// Where the bytes of a version are: the current one, or the one asked for (also of a file that was removed).
    pub fn locate(
        &self,
        app: &str,
        id: &str,
        who: &Identity,
        name: &str,
        version: Option<u64>,
    ) -> Result<(Version, PathBuf), PError> {
        let m = self.visible(app, id, who)?;
        let entry = m.files.get(name).ok_or(PError::UnknownFile)?;
        let found = match version {
            None => entry.current().cloned().ok_or(PError::UnknownFile)?,
            Some(n) => entry
                .versions
                .iter()
                .find(|v| v.n == n)
                .cloned()
                .ok_or(PError::UnknownVersion)?,
        };
        let path = blob_path(&self.root, m, &found.sha256);
        Ok((found, path))
    }

    pub fn versions(
        &self,
        app: &str,
        id: &str,
        who: &Identity,
        name: &str,
    ) -> Result<Vec<Version>, PError> {
        let m = self.visible(app, id, who)?;
        let entry = m.files.get(name).ok_or(PError::UnknownFile)?;
        Ok(entry.versions.iter().rev().cloned().collect())
    }

    /// The current files with the path of their bytes, for the export.
    pub fn snapshot(
        &self,
        app: &str,
        id: &str,
        who: &Identity,
    ) -> Result<Vec<(String, Version, PathBuf)>, PError> {
        let m = self.visible(app, id, who)?;
        Ok(m.files
            .iter()
            .filter_map(|(name, f)| {
                f.current()
                    .map(|v| (name.clone(), v.clone(), blob_path(&self.root, m, &v.sha256)))
            })
            .collect())
    }
}

/// The entries of an `.eh3dproj` container, read with the bounds of the container crate and checked against its integrity record. The package is tried as 1.0 and as 1.1.
pub fn read_container(
    bytes: &[u8],
    max_entry: u64,
    max_total: u64,
) -> Result<Vec<(String, Vec<u8>)>, PError> {
    let mut last = String::new();
    for spec in [ContainerSpec::eh3dproj_v1(), ContainerSpec::eh3dproj_v1_1()] {
        match read_with(bytes, &spec, max_entry, max_total) {
            Ok(entries) => return Ok(entries),
            Err(why) => last = why,
        }
    }
    Err(PError::BadContainer(last))
}

fn read_with(
    bytes: &[u8],
    spec: &ContainerSpec,
    max_entry: u64,
    max_total: u64,
) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut container =
        Container::open(std::io::Cursor::new(bytes), spec).map_err(|e| e.to_string())?;
    container.check_layout().map_err(|e| e.to_string())?;
    container.verify_integrity().map_err(|e| e.to_string())?;
    let names: Vec<String> = container.entries().map(|e| e.name.clone()).collect();
    let mut out = Vec::new();
    let mut total: u64 = 0;
    for name in names {
        if name == INTEGRITY_ENTRY {
            continue;
        }
        let data = container
            .read_limited(&name, max_entry)
            .map_err(|e| e.to_string())?;
        total += data.len() as u64;
        if total > max_total {
            return Err("the container is bigger than a project may be".into());
        }
        out.push((name, data));
    }
    Ok(out)
}

/// The container of the files given, as `.eh3dproj` bytes. `spec` is `1.0` or `1.1`; without it the files are tried as 1.0 first. The project has to hold the
/// `project.json` of the container (the application that saved it writes it).
pub fn write_container(
    entries: &[(String, Vec<u8>)],
    spec: Option<&str>,
    scratch: &Path,
) -> Result<Vec<u8>, PError> {
    if !entries.iter().any(|(name, _)| name == PROJECT_ENTRY) {
        return Err(PError::BadContainer(format!(
            "the project has no {PROJECT_ENTRY}"
        )));
    }
    let specs: Vec<ContainerSpec> = match spec {
        Some("1.0") => vec![ContainerSpec::eh3dproj_v1()],
        Some("1.1") => vec![ContainerSpec::eh3dproj_v1_1()],
        _ => vec![ContainerSpec::eh3dproj_v1(), ContainerSpec::eh3dproj_v1_1()],
    };
    std::fs::create_dir_all(scratch).map_err(|_| PError::Storage)?;
    let mut last = String::new();
    for spec in &specs {
        match write_with(entries, spec, scratch) {
            Ok(bytes) => return Ok(bytes),
            Err(why) => last = why,
        }
    }
    Err(PError::BadContainer(last))
}

fn write_with(
    entries: &[(String, Vec<u8>)],
    spec: &ContainerSpec,
    scratch: &Path,
) -> Result<Vec<u8>, String> {
    let mut writer = ContainerWriter::new(spec, scratch);
    for (name, bytes) in entries {
        writer.add(name, bytes).map_err(|e| e.to_string())?;
    }
    let out = scratch.join(format!("{}.eh3dproj", ids::new(Kind::Upload)));
    writer
        .write_to_path(&out, &AtomicOptions::default())
        .map_err(|e| e.to_string())?;
    let bytes = std::fs::read(&out).map_err(|e| e.to_string());
    let _ = std::fs::remove_file(&out);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("eh3d-server-pstore-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn person(name: &str, role: Role) -> Identity {
        Identity {
            name: name.into(),
            role,
            is_token: false,
            token_id: None,
            scopes: None,
            template: None,
        }
    }

    fn book(name: &str, edit: impl FnOnce(&mut ServerConfig)) -> (PBook, PathBuf) {
        let d = dir(name);
        let mut cfg = ServerConfig {
            data_dir: d.clone(),
            ..ServerConfig::default()
        };
        edit(&mut cfg);
        (PBook::open(&cfg).unwrap(), d)
    }

    #[test]
    fn the_names_of_files_are_safe() {
        for good in [
            "project.json",
            "layers/top.gbr",
            "vision/a b/c.json",
            "é.nc",
        ] {
            assert!(check_file_name(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "/abs",
            "../x",
            "a/../b",
            "a//b",
            "a\\b",
            "c:x",
            "con",
            "layers/NUL.txt",
            "dir/",
            "name.",
            "a\nb",
            "integrity.json",
        ] {
            assert!(
                matches!(check_file_name(bad), Err(PError::BadFilename(_))),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn if_match_takes_quotes_weak_tags_and_the_star() {
        assert!(tag_matches("\"abc\"", "abc"));
        assert!(tag_matches("W/\"abc\"", "abc"));
        assert!(tag_matches("abc", "abc"));
        assert!(tag_matches("*", "abc"));
        assert!(!tag_matches("\"abd\"", "abc"));
    }

    #[test]
    fn a_write_is_a_new_version_and_an_old_one_can_be_read() {
        let (mut b, d) = book("versions", |_| {});
        let ana = person("ana", Role::Operator);
        let p = b.create("studio", &ana, "front panel", 100).unwrap();
        let first = b
            .put_file(
                "studio",
                &p.id,
                &ana,
                "layers/top.gbr",
                b"one",
                None,
                "",
                101,
            )
            .unwrap();
        assert!(first.created);
        assert_eq!(first.version.n, 1);
        // the same bytes are not a new version
        let same = b
            .put_file(
                "studio",
                &p.id,
                &ana,
                "layers/top.gbr",
                b"one",
                Some(&first.version.sha256),
                "",
                102,
            )
            .unwrap();
        assert!(!same.created);
        // an existing file needs the tag, and the right one
        assert_eq!(
            b.put_file(
                "studio",
                &p.id,
                &ana,
                "layers/top.gbr",
                b"two",
                None,
                "",
                103
            )
            .unwrap_err(),
            PError::PreconditionRequired
        );
        assert_eq!(
            b.put_file(
                "studio",
                &p.id,
                &ana,
                "layers/top.gbr",
                b"two",
                Some("nope"),
                "",
                103
            )
            .unwrap_err(),
            PError::PreconditionFailed
        );
        let second = b
            .put_file(
                "studio",
                &p.id,
                &ana,
                "layers/top.gbr",
                b"two",
                Some(&first.version.sha256),
                "second",
                104,
            )
            .unwrap();
        assert_eq!(second.version.n, 2);
        let (old, path) = b
            .locate("studio", &p.id, &ana, "layers/top.gbr", Some(1))
            .unwrap();
        assert_eq!(read_verified(&path, &old).unwrap(), b"one");
        let list = b.versions("studio", &p.id, &ana, "layers/top.gbr").unwrap();
        assert_eq!(list.iter().map(|v| v.n).collect::<Vec<_>>(), vec![2, 1]);
        assert_eq!(
            b.locate("studio", &p.id, &ana, "layers/top.gbr", Some(9))
                .unwrap_err(),
            PError::UnknownVersion
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn bytes_that_changed_on_the_disk_are_not_given_back() {
        let (mut b, d) = book("damage", |_| {});
        let ana = person("ana", Role::Operator);
        let p = b.create("studio", &ana, "board", 1).unwrap();
        b.put_file("studio", &p.id, &ana, "a.txt", b"hello", None, "", 2)
            .unwrap();
        let (v, path) = b.locate("studio", &p.id, &ana, "a.txt", None).unwrap();
        assert_eq!(read_verified(&path, &v).unwrap(), b"hello");
        std::fs::write(&path, b"hullo").unwrap();
        assert_eq!(read_verified(&path, &v).unwrap_err(), PError::Damaged);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(read_verified(&path, &v).unwrap_err(), PError::Damaged);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_limits_of_the_store_are_the_settings() {
        let (mut b, d) = book("limits", |c| {
            c.project_max_file_mb = 1;
            c.project_max_mb = 2;
            c.project_max_files = 2;
            c.project_versions_kept = 2;
        });
        let ana = person("ana", Role::Operator);
        let p = b.create("studio", &ana, "board", 1).unwrap();
        let big = vec![7u8; 1024 * 1024 + 1];
        assert_eq!(
            b.put_file("studio", &p.id, &ana, "big.bin", &big, None, "", 2)
                .unwrap_err(),
            PError::FileTooBig
        );
        b.put_file(
            "studio",
            &p.id,
            &ana,
            "a.bin",
            &vec![1u8; 1024 * 1024],
            None,
            "",
            3,
        )
        .unwrap();
        b.put_file(
            "studio",
            &p.id,
            &ana,
            "b.bin",
            &vec![2u8; 1024 * 1024],
            None,
            "",
            4,
        )
        .unwrap();
        // two files only
        assert!(matches!(
            b.put_file("studio", &p.id, &ana, "c.bin", b"x", None, "", 5),
            Err(PError::Quota(_))
        ));
        // the project holds two megabytes: a third distinct megabyte does not fit
        let first = b.locate("studio", &p.id, &ana, "a.bin", None).unwrap().0;
        assert!(matches!(
            b.put_file(
                "studio",
                &p.id,
                &ana,
                "a.bin",
                &vec![3u8; 1024 * 1024],
                Some(&first.sha256),
                "",
                6
            ),
            Err(PError::Quota(_))
        ));
        // only the last two versions are kept
        let (mut b2, d2) = book("kept", |c| c.project_versions_kept = 2);
        let p2 = b2.create("studio", &ana, "board", 1).unwrap();
        let mut tag: Option<String> = None;
        for n in 0..4u8 {
            let out = b2
                .put_file(
                    "studio",
                    &p2.id,
                    &ana,
                    "a.txt",
                    &[n],
                    tag.as_deref(),
                    "",
                    2 + u64::from(n),
                )
                .unwrap();
            tag = Some(out.version.sha256);
        }
        let kept: Vec<u64> = b2
            .versions("studio", &p2.id, &ana, "a.txt")
            .unwrap()
            .iter()
            .map(|v| v.n)
            .collect();
        assert_eq!(kept, vec![4, 3]);
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&d2);
    }

    #[test]
    fn a_lock_keeps_the_others_out_until_it_expires() {
        let (mut b, d) = book("lock", |c| c.project_shared = true);
        let (ana, bob, root) = (
            person("ana", Role::Operator),
            person("bob", Role::Operator),
            person("root", Role::Admin),
        );
        let p = b.create("studio", &ana, "board", 10).unwrap();
        let lock = b.take_lock("studio", &p.id, &ana, Some(60), 10).unwrap();
        assert_eq!(lock.expires, 70);
        assert!(matches!(
            b.take_lock("studio", &p.id, &bob, None, 11),
            Err(PError::Locked(_))
        ));
        assert!(matches!(
            b.put_file("studio", &p.id, &bob, "a.txt", b"x", None, "", 12),
            Err(PError::Locked(_))
        ));
        assert!(matches!(
            b.release_lock("studio", &p.id, &bob, false, 12),
            Err(PError::Locked(_))
        ));
        // the holder renews it and keeps the start
        let again = b.take_lock("studio", &p.id, &ana, None, 20).unwrap();
        assert_eq!(again.since, 10);
        // an administrator may take it away, with force
        assert!(matches!(
            b.release_lock("studio", &p.id, &root, false, 21),
            Err(PError::Locked(_))
        ));
        b.release_lock("studio", &p.id, &root, true, 21).unwrap();
        assert_eq!(
            b.release_lock("studio", &p.id, &root, true, 22)
                .unwrap_err(),
            PError::NotLocked
        );
        // an expired lock does not hold anybody
        b.take_lock("studio", &p.id, &ana, Some(5), 30).unwrap();
        assert!(b
            .put_file("studio", &p.id, &bob, "a.txt", b"x", None, "", 36)
            .is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_project_is_its_owners_unless_the_store_is_shared() {
        let (mut b, d) = book("owner", |_| {});
        let (ana, bob, root) = (
            person("ana", Role::Operator),
            person("bob", Role::Operator),
            person("root", Role::Admin),
        );
        let p = b.create("studio", &ana, "board", 1).unwrap();
        assert!(b.visible("studio", &p.id, &ana).is_ok());
        assert!(b.visible("studio", &p.id, &root).is_ok());
        assert_eq!(
            b.visible("studio", &p.id, &bob).unwrap_err(),
            PError::Unknown
        );
        // another application does not see it either
        assert_eq!(
            b.visible("android", &p.id, &ana).unwrap_err(),
            PError::Unknown
        );
        assert_eq!(b.list("studio", &bob, "", 0, 10).0, 0);
        assert_eq!(b.list("studio", &ana, "", 0, 10).0, 1);
        assert_eq!(b.list("studio", &ana, "PANEL", 0, 10).0, 0);
        // hidden, then gone for good
        assert_eq!(
            b.purge("studio", &p.id, &root).unwrap_err(),
            PError::Unknown
        );
        assert_eq!(
            b.soft_delete("studio", &p.id, &bob, 2).unwrap_err(),
            PError::Unknown
        );
        b.soft_delete("studio", &p.id, &ana, 2).unwrap();
        assert_eq!(
            b.visible("studio", &p.id, &ana).unwrap_err(),
            PError::Unknown
        );
        assert!(b.visible("studio", &p.id, &root).is_ok());
        b.purge("studio", &p.id, &root).unwrap();
        assert_eq!(
            b.visible("studio", &p.id, &root).unwrap_err(),
            PError::Unknown
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_store_is_read_again_from_the_disk() {
        let (mut b, d) = book("reopen", |_| {});
        let ana = person("ana", Role::Operator);
        let p = b.create("pcb", &ana, "board", 1).unwrap();
        b.put_file("pcb", &p.id, &ana, "a.txt", b"hello", None, "", 2)
            .unwrap();
        let cfg = ServerConfig {
            data_dir: d.clone(),
            ..ServerConfig::default()
        };
        let again = PBook::open(&cfg).unwrap();
        let (v, path) = again.locate("pcb", &p.id, &ana, "a.txt", None).unwrap();
        assert_eq!(read_verified(&path, &v).unwrap(), b"hello");
        let _ = std::fs::remove_dir_all(&d);
    }
}
