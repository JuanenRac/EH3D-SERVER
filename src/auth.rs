//! Who may use the server: users with a role, the sessions they log in with and the long-lived tokens that programs use.
//!
//! Three roles: a **viewer** reads, an **operator** also prepares, starts, stops and confirms jobs, an **administrator** also manages users,
//! tokens and machines. Passwords are kept as Argon2 hashes; session and program tokens are random and only their SHA-256 is kept, so
//! a copy of the data folder does not log anybody in. The first administrator is made with a **set-up code** that the server prints on
//! its console while there is no user, as the nodes do.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::store;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Viewer,
    Operator,
    Admin,
}

impl Role {
    pub fn name(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Operator => "operator",
            Role::Admin => "admin",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct User {
    name: String,
    role: Role,
    /// The Argon2 hash of the password, in PHC text.
    hash: String,
    #[serde(default)]
    disabled: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ApiToken {
    pub id: String,
    pub name: String,
    pub role: Role,
    /// SHA-256 of the token, in hexadecimal.
    hash: String,
    pub created: u64,
    /// When the token was last used (seconds since 1970); `None` while it never was. Kept in memory at every use and written to the disk at most once an hour, so a
    /// busy program does not wear out the SD card.
    #[serde(default)]
    pub last_used: Option<u64>,
    /// When the token stops working (seconds since 1970); `None` for a token that does not expire.
    #[serde(default)]
    pub expires: Option<u64>,
    /// The kind of program the token was made for (`phoenix`, `cam`, `client` or `custom`); `None` for a token made before templates existed, which behaves as `custom`.
    #[serde(default)]
    pub template: Option<String>,
    /// What the token may do besides what its role allows; `None`: whatever the role allows (and, for the `phoenix` and `cam` templates, the scopes those programs may hold).
    #[serde(default)]
    pub scopes: Option<Vec<String>>,
}

/// Every scope a token can carry. A scope narrows a role, never widens it: what a token may do is the intersection of the two.
pub const SCOPES: [&str; 15] = [
    "projects:read",
    "projects:write",
    "catalog:read",
    "catalog:write",
    "artifacts:read",
    "artifacts:write",
    "tasks:read",
    "tasks:run",
    "machines:view",
    "jobs:prepare",
    "jobs:authorize",
    "jobs:start",
    "jobs:confirm",
    "jobs:cancel",
    "machines:manage",
];

/// The scopes that move or release a machine: a person's, never a design program's.
pub const MACHINE_SCOPES: [&str; 5] = [
    "jobs:authorize",
    "jobs:start",
    "jobs:confirm",
    "jobs:cancel",
    "machines:manage",
];

/// The templates a token is made from.
pub const TEMPLATES: [&str; 4] = ["phoenix", "cam", "client", "custom"];

/// Whether a template is one of the programs that must never carry a scope of [`MACHINE_SCOPES`].
pub fn is_program_template(template: Option<&str>) -> bool {
    matches!(template, Some("phoenix" | "cam"))
}

/// What a token may do, as the scopes it really has: for the programs of the design suite the scopes it was given (or all they may hold) without the scopes of the
/// machines, whatever the file says; for any other token the scopes it was given, `None` for no restriction.
pub fn effective_scopes(template: Option<&str>, scopes: Option<&[String]>) -> Option<Vec<String>> {
    if is_program_template(template) {
        let base: Vec<String> = match scopes {
            Some(s) => s.to_vec(),
            None => SCOPES.iter().map(|s| s.to_string()).collect(),
        };
        return Some(
            base.into_iter()
                .filter(|s| !MACHINE_SCOPES.contains(&s.as_str()))
                .collect(),
        );
    }
    scopes.map(<[String]>::to_vec)
}

/// A login as it is kept on the disk: only the SHA-256 of its token, never the token.
#[derive(Serialize, Deserialize, Clone, Debug)]
struct SavedSession {
    hash: String,
    name: String,
    expires: u64,
    #[serde(default)]
    id: String,
    #[serde(default)]
    app: String,
    #[serde(default)]
    created: u64,
}

/// The logins kept for the restart that an update of the server makes: only the SHA-256 of each token, and until when they may be taken up.
#[derive(Serialize, Deserialize, Debug)]
struct RestartKeep {
    valid_until: u64,
    sessions: Vec<SavedSession>,
}

/// The file of the logins kept for an update, in the data folder. It is used once and removed whatever happens.
const RESTART_KEEP_FILE: &str = "sessions-update.json";

/// How long the disk may be behind the memory about when a token was last used, in seconds.
pub const LAST_USED_SAVE_SECS: u64 = 3600;
/// The longest life a token can be given, in days.
pub const MAX_TOKEN_DAYS: u32 = 3650;

/// Who a request comes from.
#[derive(Clone, Debug, PartialEq)]
pub struct Identity {
    pub name: String,
    pub role: Role,
    /// True for a program's token, false for a person logged in.
    pub is_token: bool,
    /// The id of the program's token; `None` for a person.
    pub token_id: Option<String>,
    /// What the token is limited to; `None` for a person and for a token without limits beyond its role.
    pub scopes: Option<Vec<String>>,
    /// The template the token was made from.
    pub template: Option<String>,
}

impl Identity {
    /// Whether the scopes of the caller allow `scope` (a person and a token without scopes: yes; the role is checked apart).
    pub fn allows(&self, scope: &str) -> bool {
        self.scopes
            .as_ref()
            .is_none_or(|list| list.iter().any(|s| s == scope))
    }

    /// The key that owns what this caller makes: the name of the user, or `token:<id>` for a program's token (a user name can never have a colon, so the two never
    /// meet, and two tokens with the same name stay apart).
    pub fn owner(&self) -> String {
        match &self.token_id {
            Some(id) => format!("token:{id}"),
            None => self.name.clone(),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum AuthError {
    /// The set-up code is wrong, or the server is already set up.
    BadSetup,
    BadName,
    WeakPassword,
    BadCredentials,
    /// Too many wrong passwords: try again later.
    Throttled,
    UnknownUser,
    UserExists,
    /// The last administrator cannot be deleted, disabled or demoted.
    LastAdmin,
    UnknownToken,
    /// There is no login with this id (or it is not the person's).
    UnknownSession,
    /// The days a token should last are not a number from 1 to 3650.
    BadDays,
    /// The template is not one of `phoenix`, `cam`, `client` or `custom`.
    BadTemplate,
    /// A scope is not one the server knows.
    BadScope,
    /// A scope of the machines asked for a token of a design program.
    ForbiddenScope,
    Storage,
}

impl AuthError {
    /// The code the API answers with.
    pub fn code(self) -> &'static str {
        match self {
            AuthError::BadSetup => "bad_setup",
            AuthError::BadName => "bad_name",
            AuthError::WeakPassword => "weak_password",
            AuthError::BadCredentials => "bad_credentials",
            AuthError::Throttled => "throttled",
            AuthError::UnknownUser => "unknown_user",
            AuthError::UserExists => "user_exists",
            AuthError::LastAdmin => "last_admin",
            AuthError::UnknownToken => "unknown_token",
            AuthError::UnknownSession => "unknown_session",
            AuthError::BadDays => "bad_days",
            AuthError::BadTemplate => "bad_template",
            AuthError::BadScope => "bad_scope",
            AuthError::ForbiddenScope => "forbidden_scope",
            AuthError::Storage => "storage",
        }
    }
}

struct Session {
    who: Identity,
    expires: u64,
    /// The id the person sees in the list of their sessions (the token itself is never shown or kept).
    id: String,
    /// The application the login was made for (`studio`, `android`...); empty for a login made with `POST /login`.
    app: String,
    created: u64,
    last_seen: u64,
    /// The address of the socket the login came from, as text; empty when it was not known.
    ip: String,
    /// The order the logins were made in, which the seconds of `created` cannot tell (not kept on the disk).
    seq: u64,
}

/// A login as the person sees it in the list of their sessions.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionInfo {
    pub id: String,
    pub name: String,
    pub role: Role,
    pub app: String,
    pub created: u64,
    pub last_seen: u64,
    pub expires: u64,
    pub ip: String,
}

/// What a password has to be, from `server.cfg` (`password_min_len`, `password_classes`, `password_deny_name`).
#[derive(Clone, Debug, PartialEq)]
pub struct PasswordPolicy {
    pub min_len: usize,
    /// How many of the four classes (lower case, upper case, digits, anything else) it needs.
    pub classes: usize,
    /// It may not contain the name of the person (a name of four characters or more), in any letter case.
    pub deny_name: bool,
}

impl Default for PasswordPolicy {
    fn default() -> Self {
        PasswordPolicy {
            min_len: 8,
            classes: 1,
            deny_name: false,
        }
    }
}

impl PasswordPolicy {
    /// Whether `password` is good for the person called `name`: from `min_len` to 128 characters, enough classes, and not the name inside.
    pub fn allows(&self, name: &str, password: &str) -> bool {
        let count = password.chars().count();
        if !(self.min_len..=128).contains(&count) {
            return false;
        }
        let classes = [
            password.chars().any(|c| c.is_lowercase()),
            password.chars().any(|c| c.is_uppercase()),
            password.chars().any(|c| c.is_ascii_digit()),
            password.chars().any(|c| !c.is_alphanumeric()),
        ]
        .iter()
        .filter(|present| **present)
        .count();
        if classes < self.classes {
            return false;
        }
        if self.deny_name && name.chars().count() >= 4 {
            return !password.to_lowercase().contains(&name.to_lowercase());
        }
        true
    }
}

pub struct Auth {
    dir: PathBuf,
    users: Vec<User>,
    tokens: Vec<ApiToken>,
    sessions: HashMap<String, Session>,
    setup_code: Option<String>,
    /// Wrong passwords per name: how many in a row and until when the name is blocked.
    failures: HashMap<String, (u32, u64)>,
    /// Wrong setup codes in a row and until when the setup is blocked.
    setup_failures: (u32, u64),
    /// Wrong passwords per address of the socket (any name): how many in a row and until when the address is blocked.
    ip_failures: HashMap<std::net::IpAddr, (u32, u64)>,
    /// How many wrong passwords in a row block an address; 0 turns the limit by address off.
    ip_limit: u32,
    session_secs: u64,
    /// Whether the logins are kept in `sessions.json` (their hashes), so that they outlive a restart of the server.
    persist_sessions: bool,
    /// Until when (seconds since the epoch) the logins are also written to the file for the restart of an update; 0 when no update of the server was asked for.
    restart_keep_until: u64,
    /// When the tokens were last written because of a use (see [`ApiToken::last_used`]).
    tokens_saved_at: u64,
    tokens_dirty: bool,
    /// How many wrong passwords in a row block a name, and the first block in seconds (it doubles while they go on).
    max_failures: u32,
    lockout_base: u64,
    policy: PasswordPolicy,
    /// The longest a login can be renewed in all, in seconds from the moment it was made.
    refresh_max_secs: u64,
    next_seq: u64,
}

pub const TOKEN_PREFIX: &str = "eh3d_";
const MAX_FAILURES: u32 = 5;
const MAX_TRACKED_NAMES: usize = 1024;
const MAX_TRACKED_ADDRESSES: usize = 4096;

fn sha(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

pub fn valid_password(password: &str) -> bool {
    (8..=128).contains(&password.chars().count())
}

fn hash_password(password: &str) -> Option<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .ok()
        .map(|h| h.to_string())
}

fn password_ok(hash: &str, password: &str) -> bool {
    PasswordHash::new(hash)
        .map(|parsed| {
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        })
        .unwrap_or(false)
}

/// Compares two texts without stopping at the first difference.
fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = (a.len() != b.len()) as u8;
    for i in 0..a.len().max(b.len()) {
        diff |= a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0);
    }
    diff == 0
}

impl Auth {
    /// Opens the users and tokens kept in `dir`. With no user yet it makes a set-up code.
    pub fn open(dir: &Path, session_hours: u32) -> io::Result<Auth> {
        let users: Vec<User> = store::read_json(&dir.join("users.json"))?.unwrap_or_default();
        let tokens: Vec<ApiToken> = store::read_json(&dir.join("tokens.json"))?.unwrap_or_default();
        let setup_code = if users.is_empty() {
            Some(format!("{:08}", rand::random::<u32>() % 100_000_000))
        } else {
            None
        };
        Ok(Auth {
            dir: dir.to_path_buf(),
            users,
            tokens,
            sessions: HashMap::new(),
            setup_code,
            failures: HashMap::new(),
            setup_failures: (0, 0),
            ip_failures: HashMap::new(),
            ip_limit: 10,
            session_secs: u64::from(session_hours) * 3600,
            persist_sessions: false,
            restart_keep_until: 0,
            tokens_saved_at: 0,
            tokens_dirty: false,
            max_failures: MAX_FAILURES,
            lockout_base: 30,
            policy: PasswordPolicy::default(),
            refresh_max_secs: 168 * 3600,
            next_seq: 1,
        })
    }

    /// Turns on (or off) the keeping of the logins on the disk. On, the logins of the file that are still good and belong to a user that still counts are taken up now;
    /// off, the file is removed, so that no hash of a login stays behind.
    pub fn set_persist_sessions(&mut self, on: bool) {
        self.persist_sessions = on;
        let path = self.dir.join("sessions.json");
        if !on {
            let _ = std::fs::remove_file(&path);
            return;
        }
        let now = crate::jobs::now();
        let saved: Vec<SavedSession> = match store::read_json(&path) {
            Ok(Some(list)) => list,
            Ok(None) => return,
            Err(e) => {
                // a damaged file only costs the logins: the people sign in again
                eprintln!(
                    "eh3d-server: the saved logins are not read ({e}); everybody signs in again"
                );
                return;
            }
        };
        self.adopt_saved(saved, now);
    }

    /// Takes up the saved logins that are still good and belong to a user that still counts.
    fn adopt_saved(&mut self, saved: Vec<SavedSession>, now: u64) {
        for s in saved {
            let Some(user) = self.users.iter().find(|u| u.name == s.name && !u.disabled) else {
                continue;
            };
            if s.expires > now {
                self.sessions.insert(
                    s.hash,
                    Session {
                        who: Identity {
                            name: user.name.clone(),
                            role: user.role,
                            is_token: false,
                            token_id: None,
                            scopes: None,
                            template: None,
                        },
                        expires: s.expires,
                        id: if s.id.is_empty() { random_hex(8) } else { s.id },
                        app: s.app,
                        created: if s.created == 0 { now } else { s.created },
                        last_seen: now,
                        ip: String::new(),
                        seq: self.next_seq,
                    },
                );
                self.next_seq += 1;
            }
        }
    }

    /// Writes the logins that are still good (nothing happens when the keeping is off). A failure to write is told on the console and costs nothing else: the logins
    /// are still good in memory.
    fn save_sessions(&self, now: u64) {
        // a login that ends (or is closed) while an update waits to restart the server must not come back with it
        if self.restart_keep_until > now {
            self.write_restart_keep(now);
        } else if self.restart_keep_until != 0 {
            // the time is up and the server did not restart: the hashes do not stay on the disk
            let _ = std::fs::remove_file(self.dir.join(RESTART_KEEP_FILE));
        }
        if !self.persist_sessions {
            return;
        }
        let list = self.saved_list(now);
        if let Err(e) = store::write_json(&self.dir.join("sessions.json"), &list) {
            eprintln!("eh3d-server: the logins could not be saved: {e}");
        }
    }

    /// The logins that are still good, as they are kept on the disk (never the token).
    fn saved_list(&self, now: u64) -> Vec<SavedSession> {
        self.sessions
            .iter()
            .filter(|(_, s)| s.expires > now)
            .map(|(hash, s)| SavedSession {
                hash: hash.clone(),
                name: s.who.name.clone(),
                expires: s.expires,
                id: s.id.clone(),
                app: s.app.clone(),
                created: s.created,
            })
            .collect()
    }

    fn write_restart_keep(&self, now: u64) {
        let keep = RestartKeep {
            valid_until: self.restart_keep_until,
            sessions: self.saved_list(now),
        };
        let path = self.dir.join(RESTART_KEEP_FILE);
        // no `.bak`: a copy of the hashes must not stay behind when the file is removed
        let written = serde_json::to_vec(&keep)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
            .and_then(|bytes| store::write_durable(&path, &bytes, false));
        if let Err(e) = written {
            eprintln!("eh3d-server: the logins could not be kept for the update: {e}");
        }
    }

    /// An update of the server was asked for: the logins are kept in a file of the data folder for `secs` seconds, so that the server that starts after the update takes them
    /// up (see [`Auth::restore_after_restart`]). Only the SHA-256 of each token is written. Every change of the logins until then rewrites the file.
    pub fn keep_for_restart(&mut self, secs: u64, now: u64) {
        self.restart_keep_until = now + secs;
        self.write_restart_keep(now);
    }

    /// At start: takes up the logins kept for an update, when `allowed` (the switch `update_keep_logins`) and the file was written no longer ago than it said. The file is
    /// removed in every case, so a login is never taken up twice. How many logins came back.
    pub fn restore_after_restart(&mut self, allowed: bool, now: u64) -> usize {
        let path = self.dir.join(RESTART_KEEP_FILE);
        let found: io::Result<Option<RestartKeep>> = store::read_json(&path);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("tmp"));
        if !allowed {
            return 0;
        }
        let keep = match found {
            Ok(Some(keep)) if now <= keep.valid_until => keep,
            _ => return 0,
        };
        let before = self.sessions.len();
        self.adopt_saved(keep.sessions, now);
        self.sessions.len().saturating_sub(before)
    }

    pub fn needs_setup(&self) -> bool {
        self.users.is_empty()
    }

    /// The set-up code to show on the console (only while there is no user).
    pub fn setup_code(&self) -> Option<&str> {
        self.setup_code.as_deref()
    }

    fn save_users(&self) -> Result<(), AuthError> {
        store::write_json(&self.dir.join("users.json"), &self.users).map_err(|_| AuthError::Storage)
    }

    fn save_tokens(&self) -> Result<(), AuthError> {
        store::write_json(&self.dir.join("tokens.json"), &self.tokens)
            .map_err(|_| AuthError::Storage)
    }

    /// Makes the first administrator.
    pub fn setup(&mut self, code: &str, name: &str, password: &str) -> Result<(), AuthError> {
        let at = crate::jobs::now();
        if self.setup_failures.1 > at {
            return Err(AuthError::Throttled);
        }
        match &self.setup_code {
            Some(expected) if self.users.is_empty() && same(expected, code.trim()) => {}
            _ => {
                // the code has 8 digits: guessing it is slowed down the same way as the passwords
                self.setup_failures.0 += 1;
                if self.setup_failures.0 >= MAX_FAILURES {
                    self.setup_failures.1 =
                        at + (30u64 << (self.setup_failures.0 - MAX_FAILURES).min(6));
                }
                return Err(AuthError::BadSetup);
            }
        }
        if !valid_name(name) {
            return Err(AuthError::BadName);
        }
        if !self.policy.allows(name, password) {
            return Err(AuthError::WeakPassword);
        }
        let hash = hash_password(password).ok_or(AuthError::Storage)?;
        self.users.push(User {
            name: name.into(),
            role: Role::Admin,
            hash,
            disabled: false,
        });
        self.save_users()?;
        self.setup_code = None;
        Ok(())
    }

    /// Logs a person in; returns the session token, the role and how long it lasts in seconds.
    pub fn login(
        &mut self,
        name: &str,
        password: &str,
        now: u64,
    ) -> Result<(String, Role, u64), AuthError> {
        self.login_from(name, password, now, None)
    }

    /// How many wrong passwords in a row from one address block it (0: no limit by address).
    pub fn set_ip_limit(&mut self, limit: u32) {
        self.ip_limit = limit;
    }

    /// How many wrong passwords in a row block a name, and for how long the first block lasts (`auth_max_failures`, `auth_lockout_s`).
    pub fn set_lockout(&mut self, max_failures: u32, base_secs: u64) {
        self.max_failures = max_failures.max(1);
        self.lockout_base = base_secs.max(1);
    }

    pub fn set_password_policy(&mut self, policy: PasswordPolicy) {
        self.policy = policy;
    }

    pub fn password_policy(&self) -> &PasswordPolicy {
        &self.policy
    }

    /// The longest a login can be renewed in all, in hours from the moment it was made.
    pub fn set_refresh_max_hours(&mut self, hours: u64) {
        self.refresh_max_secs = hours.max(1) * 3600;
    }

    /// Like `login`, knowing the address the request came from (the address of the socket, never a header): a person who tries many names from one
    /// address is slowed down without blocking the name of somebody else.
    pub fn login_from(
        &mut self,
        name: &str,
        password: &str,
        now: u64,
        ip: Option<std::net::IpAddr>,
    ) -> Result<(String, Role, u64), AuthError> {
        self.login_with(name, password, now, ip, "", 0)
            .map(|(token, role, secs, _)| (token, role, secs))
    }

    /// The login of an application: the same checks, and the session remembers the application and the address. With `max_sessions` above 0 the oldest login of the
    /// person is closed when they already have that many. Gives the token, the role, how long it lasts in seconds and the id of the session.
    pub fn login_with(
        &mut self,
        name: &str,
        password: &str,
        now: u64,
        ip: Option<std::net::IpAddr>,
        app: &str,
        max_sessions: usize,
    ) -> Result<(String, Role, u64, String), AuthError> {
        let shown_ip = ip.map(|a| a.to_string()).unwrap_or_default();
        let ip = ip.filter(|_| self.ip_limit > 0);
        if let Some(a) = ip {
            if matches!(self.ip_failures.get(&a), Some(&(_, until)) if until > now) {
                return Err(AuthError::Throttled);
            }
        }
        if let Some(&(_, until)) = self.failures.get(name) {
            if until > now {
                return Err(AuthError::Throttled);
            }
        }
        let user = self.users.iter().find(|u| u.name == name && !u.disabled);
        // the hash of a user that does not exist is checked anyway, so the time does not tell which names exist
        let good = match user {
            Some(u) => password_ok(&u.hash, password),
            None => {
                let _ = password_ok("$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHRzb21lc2FsdA$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", password);
                false
            }
        };
        if !good {
            if let Some(a) = ip {
                if self.ip_failures.len() >= MAX_TRACKED_ADDRESSES {
                    self.ip_failures.retain(|_, &mut (_, until)| until > now);
                }
                if self.ip_failures.len() < MAX_TRACKED_ADDRESSES
                    || self.ip_failures.contains_key(&a)
                {
                    let e = self.ip_failures.entry(a).or_insert((0, 0));
                    e.0 += 1;
                    if e.0 >= self.ip_limit {
                        e.1 = now + (self.lockout_base << (e.0 - self.ip_limit).min(6));
                    }
                }
            }
            // names that were never real must not make the map grow for ever: the blocks that ended are forgotten
            if self.failures.len() >= MAX_TRACKED_NAMES {
                self.failures.retain(|_, &mut (_, until)| until > now);
            }
            if self.failures.len() >= MAX_TRACKED_NAMES && !self.failures.contains_key(name) {
                return Err(AuthError::BadCredentials);
            }
            let entry = self.failures.entry(name.to_string()).or_insert((0, 0));
            entry.0 += 1;
            if entry.0 >= self.max_failures {
                // 30 s, then 60 s, 120 s... for as long as the wrong passwords go on
                entry.1 = now + (self.lockout_base << (entry.0 - self.max_failures).min(6));
            }
            return Err(AuthError::BadCredentials);
        }
        self.failures.remove(name);
        if let Some(a) = ip {
            self.ip_failures.remove(&a);
        }
        let role = user.map(|u| u.role).unwrap_or(Role::Viewer);
        let token = random_hex(32);
        // the person already has as many logins as they may: the oldest is closed
        if max_sessions > 0 {
            let mut mine: Vec<(u64, String)> = self
                .sessions
                .iter()
                .filter(|(_, s)| s.who.name == name && s.expires > now)
                .map(|(hash, s)| (s.seq, hash.clone()))
                .collect();
            mine.sort();
            while mine.len() >= max_sessions {
                let (_, hash) = mine.remove(0);
                self.sessions.remove(&hash);
            }
        }
        let id = random_hex(8);
        self.sessions.insert(
            sha(&token),
            Session {
                who: Identity {
                    name: name.into(),
                    role,
                    is_token: false,
                    token_id: None,
                    scopes: None,
                    template: None,
                },
                expires: now + self.session_secs,
                id: id.clone(),
                app: app.to_string(),
                created: now,
                last_seen: now,
                ip: shown_ip,
                seq: self.next_seq,
            },
        );
        self.next_seq += 1;
        self.save_sessions(now);
        Ok((token, role, self.session_secs, id))
    }

    fn info_of(session: &Session) -> SessionInfo {
        SessionInfo {
            id: session.id.clone(),
            name: session.who.name.clone(),
            role: session.who.role,
            app: session.app.clone(),
            created: session.created,
            last_seen: session.last_seen,
            expires: session.expires,
            ip: session.ip.clone(),
        }
    }

    /// The session a token belongs to (a login of a person; a program's token has none).
    pub fn session_of(&self, token: &str) -> Option<SessionInfo> {
        self.sessions.get(&sha(token)).map(Self::info_of)
    }

    /// The application of the login a token belongs to; `None` for a program's token and for a login made without one.
    pub fn app_of(&self, token: &str) -> Option<String> {
        self.session_of(token)
            .map(|s| s.app)
            .filter(|a| !a.is_empty())
    }

    /// The logins that are still good, the newest first: those of one person, or all of them.
    pub fn sessions(&self, name: Option<&str>, now: u64) -> Vec<SessionInfo> {
        let mut list: Vec<(u64, SessionInfo)> = self
            .sessions
            .values()
            .filter(|s| s.expires > now)
            .filter(|s| name.is_none_or(|n| s.who.name == n))
            .map(|s| (s.seq, Self::info_of(s)))
            .collect();
        list.sort_by(|a, b| b.1.created.cmp(&a.1.created).then_with(|| b.0.cmp(&a.0)));
        list.into_iter().map(|(_, info)| info).collect()
    }

    /// Closes the login with this id; with `owner` only if it is that person's. The login that was closed.
    pub fn close_session(
        &mut self,
        id: &str,
        owner: Option<&str>,
    ) -> Result<SessionInfo, AuthError> {
        let hash = self
            .sessions
            .iter()
            .find(|(_, s)| s.id == id && owner.is_none_or(|o| s.who.name == o))
            .map(|(hash, _)| hash.clone())
            .ok_or(AuthError::UnknownSession)?;
        let closed = self
            .sessions
            .remove(&hash)
            .map(|s| Self::info_of(&s))
            .ok_or(AuthError::UnknownSession)?;
        self.save_sessions(crate::jobs::now());
        Ok(closed)
    }

    /// Closes every login of a person. How many there were.
    pub fn close_sessions_of(&mut self, name: &str) -> usize {
        let before = self.sessions.len();
        self.sessions.retain(|_, s| s.who.name != name);
        let closed = before - self.sessions.len();
        if closed > 0 {
            self.save_sessions(crate::jobs::now());
        }
        closed
    }

    /// Renews the login of a token: it lasts again as long as a login does, but never past `auth_refresh_max_hours` from the moment it was made. Gives how many seconds
    /// it has left.
    pub fn refresh(&mut self, token: &str, now: u64) -> Result<u64, AuthError> {
        let limit = self.refresh_max_secs;
        let secs = self.session_secs;
        let session = self
            .sessions
            .get_mut(&sha(token))
            .filter(|s| s.expires > now)
            .ok_or(AuthError::UnknownSession)?;
        let longest = session.created + limit;
        let renewed = (now + secs).min(longest);
        if renewed > session.expires {
            session.expires = renewed;
        }
        let left = session.expires.saturating_sub(now);
        self.save_sessions(now);
        Ok(left)
    }

    pub fn logout(&mut self, token: &str) {
        if self.sessions.remove(&sha(token)).is_some() {
            self.save_sessions(crate::jobs::now());
        }
    }

    /// Who a bearer token belongs to: a session of a person, or a program's token.
    pub fn identify(&mut self, bearer: &str, now: u64) -> Option<Identity> {
        let hash = sha(bearer);
        if bearer.starts_with(TOKEN_PREFIX) {
            let token = self.tokens.iter_mut().find(|t| same(&t.hash, &hash))?;
            // a token that has expired is as good as one that was never made
            if token.expires.is_some_and(|e| e <= now) {
                return None;
            }
            token.last_used = Some(now);
            let who = Identity {
                name: token.name.clone(),
                role: token.role,
                is_token: true,
                token_id: Some(token.id.clone()),
                scopes: effective_scopes(token.template.as_deref(), token.scopes.as_deref()),
                template: token.template.clone(),
            };
            self.tokens_dirty = true;
            if now.saturating_sub(self.tokens_saved_at) >= LAST_USED_SAVE_SECS {
                self.tokens_saved_at = now;
                self.tokens_dirty = false;
                if let Err(e) = self.save_tokens() {
                    eprintln!("eh3d-server: the last use of the tokens could not be saved ({e:?})");
                }
            }
            return Some(who);
        }
        self.sessions.retain(|_, s| s.expires > now);
        let session = self.sessions.get_mut(&hash)?;
        session.last_seen = now;
        let session_name = session.who.name.clone();
        // a user that was deleted or disabled since the login no longer counts
        let user = self
            .users
            .iter()
            .find(|u| u.name == session_name && !u.disabled)?;
        Some(Identity {
            name: user.name.clone(),
            role: user.role,
            is_token: false,
            token_id: None,
            scopes: None,
            template: None,
        })
    }

    // ---- users

    pub fn users(&self) -> Vec<(String, Role, bool)> {
        self.users
            .iter()
            .map(|u| (u.name.clone(), u.role, u.disabled))
            .collect()
    }

    fn admins(&self) -> usize {
        self.users
            .iter()
            .filter(|u| u.role == Role::Admin && !u.disabled)
            .count()
    }

    pub fn add_user(&mut self, name: &str, role: Role, password: &str) -> Result<(), AuthError> {
        if !valid_name(name) {
            return Err(AuthError::BadName);
        }
        if !self.policy.allows(name, password) {
            return Err(AuthError::WeakPassword);
        }
        if self.users.iter().any(|u| u.name == name) {
            return Err(AuthError::UserExists);
        }
        let hash = hash_password(password).ok_or(AuthError::Storage)?;
        self.users.push(User {
            name: name.into(),
            role,
            hash,
            disabled: false,
        });
        self.save_users()
    }

    /// Changes the role, the password or the disabled state of a user; what is `None` stays.
    pub fn update_user(
        &mut self,
        name: &str,
        role: Option<Role>,
        password: Option<&str>,
        disabled: Option<bool>,
    ) -> Result<(), AuthError> {
        let index = self
            .users
            .iter()
            .position(|u| u.name == name)
            .ok_or(AuthError::UnknownUser)?;
        let demotes = self.users[index].role == Role::Admin
            && !self.users[index].disabled
            && (role.is_some_and(|r| r != Role::Admin) || disabled == Some(true));
        if demotes && self.admins() <= 1 {
            return Err(AuthError::LastAdmin);
        }
        if let Some(p) = password {
            if !self.policy.allows(name, p) {
                return Err(AuthError::WeakPassword);
            }
            self.users[index].hash = hash_password(p).ok_or(AuthError::Storage)?;
        }
        if let Some(r) = role {
            self.users[index].role = r;
        }
        if let Some(d) = disabled {
            self.users[index].disabled = d;
        }
        // whatever changed, the sessions already open must meet the new state
        self.sessions.retain(|_, s| s.who.name != name);
        self.save_sessions(crate::jobs::now());
        self.save_users()
    }

    pub fn delete_user(&mut self, name: &str) -> Result<(), AuthError> {
        let index = self
            .users
            .iter()
            .position(|u| u.name == name)
            .ok_or(AuthError::UnknownUser)?;
        if self.users[index].role == Role::Admin
            && !self.users[index].disabled
            && self.admins() <= 1
        {
            return Err(AuthError::LastAdmin);
        }
        self.users.remove(index);
        self.sessions.retain(|_, s| s.who.name != name);
        self.save_sessions(crate::jobs::now());
        self.save_users()
    }

    /// A person changes their own password.
    pub fn change_password(&mut self, name: &str, old: &str, new: &str) -> Result<(), AuthError> {
        let user = self
            .users
            .iter()
            .find(|u| u.name == name)
            .ok_or(AuthError::UnknownUser)?;
        if !password_ok(&user.hash, old) {
            return Err(AuthError::BadCredentials);
        }
        self.update_user(name, None, Some(new), None)
    }

    // ---- the tokens of programs

    pub fn tokens(&self) -> &[ApiToken] {
        &self.tokens
    }

    /// Makes a token for a program; the secret is returned this once and never kept. With `days` the token stops working after them; without, it does not expire.
    pub fn create_token(
        &mut self,
        name: &str,
        role: Role,
        now: u64,
        days: Option<u32>,
    ) -> Result<(String, String), AuthError> {
        self.create_token_with(name, role, now, days, None, None)
    }

    /// The same with a template and scopes. The scopes must be known; a token of the `phoenix` or `cam` template is refused any scope of the machines (it is not
    /// stripped silently: the request is wrong).
    pub fn create_token_with(
        &mut self,
        name: &str,
        role: Role,
        now: u64,
        days: Option<u32>,
        template: Option<&str>,
        scopes: Option<&[String]>,
    ) -> Result<(String, String), AuthError> {
        if name.is_empty() || name.len() > 64 {
            return Err(AuthError::BadName);
        }
        if days.is_some_and(|d| d == 0 || d > MAX_TOKEN_DAYS) {
            return Err(AuthError::BadDays);
        }
        if template.is_some_and(|t| !TEMPLATES.contains(&t)) {
            return Err(AuthError::BadTemplate);
        }
        let mut kept: Option<Vec<String>> = None;
        if let Some(list) = scopes {
            let mut out: Vec<String> = Vec::new();
            for s in list {
                if !SCOPES.contains(&s.as_str()) {
                    return Err(AuthError::BadScope);
                }
                if is_program_template(template) && MACHINE_SCOPES.contains(&s.as_str()) {
                    return Err(AuthError::ForbiddenScope);
                }
                if !out.contains(s) {
                    out.push(s.clone());
                }
            }
            kept = Some(out);
        }
        let secret = format!("{TOKEN_PREFIX}{}", random_hex(24));
        let id = random_hex(4);
        self.tokens.push(ApiToken {
            id: id.clone(),
            name: name.into(),
            role,
            hash: sha(&secret),
            created: now,
            last_used: None,
            expires: days.map(|d| now + u64::from(d) * 86_400),
            template: template.map(str::to_string),
            scopes: kept,
        });
        self.save_tokens()?;
        Ok((id, secret))
    }

    /// Gives a token a new secret and takes the old one out of use at once; the id, the scopes and the expiry stay. The new secret is returned this once.
    pub fn rotate_token(&mut self, id: &str) -> Result<String, AuthError> {
        let secret = format!("{TOKEN_PREFIX}{}", random_hex(24));
        let token = self
            .tokens
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or(AuthError::UnknownToken)?;
        token.hash = sha(&secret);
        token.last_used = None;
        self.save_tokens()?;
        Ok(secret)
    }

    pub fn delete_token(&mut self, id: &str) -> Result<(), AuthError> {
        let index = self
            .tokens
            .iter()
            .position(|t| t.id == id)
            .ok_or(AuthError::UnknownToken)?;
        self.tokens.remove(index);
        self.save_tokens()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("eh3d-server-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn set_up(name: &str) -> (Auth, PathBuf) {
        let d = dir(name);
        let mut a = Auth::open(&d, 12).unwrap();
        let code = a.setup_code().unwrap().to_string();
        a.setup(&code, "juan", "una-clave-larga").unwrap();
        (a, d)
    }

    #[test]
    fn many_names_from_one_address_are_slowed_down_without_blocking_the_names() {
        let d = dir("auth-ip");
        let mut a = Auth::open(&d, 12).unwrap();
        let code = a.setup_code().unwrap().to_string();
        a.setup(&code, "juan", "una-clave-larga").unwrap();
        a.set_ip_limit(3);
        let ip: std::net::IpAddr = "10.0.0.7".parse().unwrap();
        for n in 0..3 {
            let name = format!("who{n}");
            assert_eq!(
                a.login_from(&name, "mala-clave-larga", 1000, Some(ip)),
                Err(AuthError::BadCredentials)
            );
        }
        // the address is blocked, even for the right password; another address and the plain login are not
        assert_eq!(
            a.login_from("juan", "una-clave-larga", 1001, Some(ip)),
            Err(AuthError::Throttled)
        );
        let other: std::net::IpAddr = "10.0.0.8".parse().unwrap();
        assert!(a
            .login_from("juan", "una-clave-larga", 1001, Some(other))
            .is_ok());
        // a limit of 0 turns it off
        a.set_ip_limit(0);
        assert!(a
            .login_from("juan", "una-clave-larga", 1001, Some(ip))
            .is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn guessing_the_setup_code_is_slowed_down() {
        let d = dir("auth-setup-guess");
        let mut a = Auth::open(&d, 12).unwrap();
        let code = a.setup_code().unwrap().to_string();
        for _ in 0..MAX_FAILURES {
            assert_eq!(
                a.setup("00000000x", "juan", "una-clave-larga"),
                Err(AuthError::BadSetup)
            );
        }
        // even the right code waits
        assert_eq!(
            a.setup(&code, "juan", "una-clave-larga"),
            Err(AuthError::Throttled)
        );
        assert!(a.needs_setup());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_first_administrator_needs_the_setup_code_once() {
        let d = dir("auth-a");
        let mut a = Auth::open(&d, 12).unwrap();
        assert!(a.needs_setup());
        let code = a.setup_code().unwrap().to_string();
        assert_eq!(code.len(), 8);
        assert_eq!(
            a.setup("00000000x", "juan", "una-clave-larga"),
            Err(AuthError::BadSetup)
        );
        assert_eq!(
            a.setup(&code, "ju an", "una-clave-larga"),
            Err(AuthError::BadName)
        );
        assert_eq!(
            a.setup(&code, "juan", "short"),
            Err(AuthError::WeakPassword)
        );
        assert_eq!(a.setup(&code, "juan", "una-clave-larga"), Ok(()));
        assert!(!a.needs_setup() && a.setup_code().is_none());
        assert_eq!(
            a.setup(&code, "otro", "una-clave-larga"),
            Err(AuthError::BadSetup)
        );
        // after a restart the user is still there and there is no code
        let b = Auth::open(&d, 12).unwrap();
        assert!(!b.needs_setup() && b.setup_code().is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_login_gives_a_session_that_expires_and_logout_ends() {
        let (mut a, d) = set_up("auth-b");
        let (token, role, secs) = a.login("juan", "una-clave-larga", 1000).unwrap();
        assert_eq!((role, secs), (Role::Admin, 12 * 3600));
        let who = a.identify(&token, 1001).unwrap();
        assert_eq!(
            (who.name.as_str(), who.role, who.is_token),
            ("juan", Role::Admin, false)
        );
        assert!(a.identify(&token, 1000 + 12 * 3600 + 1).is_none());
        let (token, _, _) = a.login("juan", "una-clave-larga", 2000).unwrap();
        a.logout(&token);
        assert!(a.identify(&token, 2001).is_none());
        assert!(a.identify("nonsense", 2001).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn wrong_passwords_are_slowed_down_and_do_not_say_which_part_was_wrong() {
        let (mut a, d) = set_up("auth-c");
        for _ in 0..4 {
            assert_eq!(
                a.login("juan", "mala", 100).unwrap_err(),
                AuthError::BadCredentials
            );
        }
        assert_eq!(
            a.login("nadie", "una-clave-larga", 100).unwrap_err(),
            AuthError::BadCredentials
        );
        assert_eq!(
            a.login("juan", "mala", 100).unwrap_err(),
            AuthError::BadCredentials
        );
        // the fifth wrong one blocked the name
        assert_eq!(
            a.login("juan", "una-clave-larga", 110).unwrap_err(),
            AuthError::Throttled
        );
        assert!(a.login("juan", "una-clave-larga", 200).is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn users_have_roles_and_the_last_administrator_stays() {
        let (mut a, d) = set_up("auth-d");
        a.add_user("ana", Role::Operator, "otra-clave-larga")
            .unwrap();
        assert_eq!(
            a.add_user("ana", Role::Viewer, "otra-clave-larga"),
            Err(AuthError::UserExists)
        );
        assert_eq!(
            a.add_user("x", Role::Viewer, "corta"),
            Err(AuthError::WeakPassword)
        );
        let (token, role, _) = a.login("ana", "otra-clave-larga", 10).unwrap();
        assert_eq!(role, Role::Operator);
        assert!(Role::Viewer < Role::Operator && Role::Operator < Role::Admin);
        // the last administrator cannot go
        assert_eq!(a.delete_user("juan"), Err(AuthError::LastAdmin));
        assert_eq!(
            a.update_user("juan", Some(Role::Viewer), None, None),
            Err(AuthError::LastAdmin)
        );
        assert_eq!(
            a.update_user("juan", None, None, Some(true)),
            Err(AuthError::LastAdmin)
        );
        // a second one lets the first go
        a.update_user("ana", Some(Role::Admin), None, None).unwrap();
        assert!(
            a.identify(&token, 11).is_none(),
            "her open sessions are closed when her role changes"
        );
        a.delete_user("juan").unwrap();
        assert_eq!(a.users().len(), 1);
        // a disabled user cannot log in
        a.add_user("bob", Role::Viewer, "clave-de-bob-1").unwrap();
        a.update_user("bob", None, None, Some(true)).unwrap();
        assert_eq!(
            a.login("bob", "clave-de-bob-1", 20).unwrap_err(),
            AuthError::BadCredentials
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_person_changes_their_own_password_knowing_the_old_one() {
        let (mut a, d) = set_up("auth-e");
        assert_eq!(
            a.change_password("juan", "mala", "nueva-clave-larga"),
            Err(AuthError::BadCredentials)
        );
        assert_eq!(
            a.change_password("juan", "una-clave-larga", "corta"),
            Err(AuthError::WeakPassword)
        );
        a.change_password("juan", "una-clave-larga", "nueva-clave-larga")
            .unwrap();
        assert!(a.login("juan", "una-clave-larga", 5).is_err());
        assert!(a.login("juan", "nueva-clave-larga", 5).is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_programs_token_is_shown_once_and_only_its_hash_is_kept() {
        let (mut a, d) = set_up("auth-f");
        let (id, secret) = a
            .create_token("drillpro-pc", Role::Operator, 99, None)
            .unwrap();
        assert!(secret.starts_with(TOKEN_PREFIX));
        let who = a.identify(&secret, 100).unwrap();
        assert_eq!(
            (who.name.as_str(), who.role, who.is_token),
            ("drillpro-pc", Role::Operator, true)
        );
        let on_disk = std::fs::read_to_string(d.join("tokens.json")).unwrap();
        assert!(!on_disk.contains(&secret));
        let b = Auth::open(&d, 12).unwrap();
        assert_eq!(b.tokens().len(), 1);
        a.delete_token(&id).unwrap();
        assert!(a.identify(&secret, 101).is_none());
        assert_eq!(a.delete_token(&id), Err(AuthError::UnknownToken));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_token_can_expire_and_it_says_when_it_was_last_used() {
        let (mut a, d) = set_up("auth-token-days");
        assert_eq!(
            a.create_token("x", Role::Viewer, 0, Some(0)),
            Err(AuthError::BadDays)
        );
        assert_eq!(
            a.create_token("x", Role::Viewer, 0, Some(MAX_TOKEN_DAYS + 1)),
            Err(AuthError::BadDays)
        );
        let (_, short) = a
            .create_token("laptop", Role::Operator, 1_000, Some(2))
            .unwrap();
        let (_, forever) = a.create_token("fixed", Role::Viewer, 1_000, None).unwrap();
        assert_eq!(a.tokens()[0].expires, Some(1_000 + 2 * 86_400));
        assert_eq!(a.tokens()[1].expires, None);
        assert_eq!(a.tokens()[0].last_used, None);
        assert!(a.identify(&short, 5_000).is_some());
        assert_eq!(a.tokens()[0].last_used, Some(5_000));
        // the last day of its life is still good, the next moment is not
        assert!(a.identify(&short, 1_000 + 2 * 86_400 - 1).is_some());
        assert!(a.identify(&short, 1_000 + 2 * 86_400).is_none());
        assert_eq!(a.tokens()[0].last_used, Some(1_000 + 2 * 86_400 - 1));
        assert!(a.identify(&forever, 1_000 + 400 * 86_400).is_some());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_last_use_of_a_token_reaches_the_disk_once_an_hour_not_at_every_request() {
        let (mut a, d) = set_up("auth-token-saves");
        let (_, secret) = a.create_token("laptop", Role::Operator, 10, None).unwrap();
        let on_disk = |d: &PathBuf| -> Option<u64> {
            let text = std::fs::read_to_string(d.join("tokens.json")).unwrap();
            let rows: Vec<ApiToken> = serde_json::from_str(&text).unwrap();
            rows[0].last_used
        };
        assert_eq!(on_disk(&d), None);
        a.identify(&secret, 10_000).unwrap();
        assert_eq!(
            on_disk(&d),
            Some(10_000),
            "the first use is written at once"
        );
        a.identify(&secret, 10_500).unwrap();
        a.identify(&secret, 13_599).unwrap();
        assert_eq!(
            on_disk(&d),
            Some(10_000),
            "within the hour the disk is left alone"
        );
        assert_eq!(a.tokens()[0].last_used, Some(13_599), "the memory is exact");
        a.identify(&secret, 13_600).unwrap();
        assert_eq!(on_disk(&d), Some(13_600));
        // a file of a version that had neither member still opens
        std::fs::write(
            d.join("tokens.json"),
            r#"[{"id":"ab","name":"old","role":"viewer","hash":"00","created":1}]"#,
        )
        .unwrap();
        let b = Auth::open(&d, 12).unwrap();
        assert_eq!(
            (b.tokens()[0].last_used, b.tokens()[0].expires),
            (None, None)
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_logins_survive_a_restart_when_they_are_kept_and_only_as_hashes() {
        let (mut a, d) = set_up("auth-sessions");
        a.set_persist_sessions(true);
        let now = crate::jobs::now();
        let (token, _, _) = a.login("juan", "una-clave-larga", now).unwrap();
        a.add_user("ana", Role::Operator, "otra-clave-larga")
            .unwrap();
        let (ana, _, _) = a.login("ana", "otra-clave-larga", now).unwrap();
        let on_disk = std::fs::read_to_string(d.join("sessions.json")).unwrap();
        assert!(!on_disk.contains(&token), "the token itself is not kept");
        assert!(on_disk.contains(&sha(&token)));
        // the server starts again
        let mut b = Auth::open(&d, 12).unwrap();
        assert!(
            b.identify(&token, now + 1).is_none(),
            "not before the file is asked for"
        );
        b.set_persist_sessions(true);
        let who = b.identify(&token, now + 1).unwrap();
        assert_eq!((who.name.as_str(), who.role), ("juan", Role::Admin));
        assert!(b.identify(&ana, now + 1).is_some());
        // a logout and a deleted user are forgotten for good
        b.logout(&token);
        b.delete_user("ana").unwrap();
        let mut c = Auth::open(&d, 12).unwrap();
        c.set_persist_sessions(true);
        assert!(c.identify(&token, now + 2).is_none());
        assert!(c.identify(&ana, now + 2).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_logins_kept_for_an_update_come_back_once_only_within_the_time_and_only_if_allowed() {
        let (mut a, d) = set_up("auth-keep-update");
        let now = crate::jobs::now();
        let (token, _, _) = a.login("juan", "una-clave-larga", now).unwrap();
        // nothing is written before an update asks for it
        assert!(!d.join(RESTART_KEEP_FILE).exists());
        a.keep_for_restart(300, now);
        let on_disk = std::fs::read_to_string(d.join(RESTART_KEEP_FILE)).unwrap();
        assert!(!on_disk.contains(&token), "the token itself is not kept");
        assert!(on_disk.contains(&sha(&token)));
        // a login made after the request is in the file too, and one that is closed is out of it
        let (late, _, _) = a.login("juan", "una-clave-larga", now + 1).unwrap();
        a.logout(&token);
        let on_disk = std::fs::read_to_string(d.join(RESTART_KEEP_FILE)).unwrap();
        assert!(on_disk.contains(&sha(&late)) && !on_disk.contains(&sha(&token)));
        // the server that starts after the update takes it up, once
        let mut b = Auth::open(&d, 12).unwrap();
        assert!(
            b.identify(&late, now + 2).is_none(),
            "not before it is asked for"
        );
        assert_eq!(b.restore_after_restart(true, now + 5), 1);
        assert!(b.identify(&late, now + 6).is_some());
        assert!(b.identify(&token, now + 6).is_none());
        assert!(!d.join(RESTART_KEEP_FILE).exists(), "used once");
        assert!(
            !d.join("sessions.json").exists(),
            "persist_sessions stays off: nothing else is written"
        );
        let mut again = Auth::open(&d, 12).unwrap();
        assert_eq!(again.restore_after_restart(true, now + 7), 0);
        // too late: the time is up, and the file goes all the same
        a.keep_for_restart(300, now);
        let mut c = Auth::open(&d, 12).unwrap();
        assert_eq!(c.restore_after_restart(true, now + 301), 0);
        assert!(!d.join(RESTART_KEEP_FILE).exists());
        // the switch is off: nothing comes back and the file goes
        a.keep_for_restart(300, now);
        let mut e = Auth::open(&d, 12).unwrap();
        assert_eq!(e.restore_after_restart(false, now + 5), 0);
        assert!(!d.join(RESTART_KEEP_FILE).exists());
        assert!(e.identify(&late, now + 6).is_none());
        // a damaged file costs the logins only
        std::fs::write(d.join(RESTART_KEEP_FILE), "{ not json").unwrap();
        let mut f = Auth::open(&d, 12).unwrap();
        assert_eq!(f.restore_after_restart(true, now + 5), 0);
        assert!(!d.join(RESTART_KEEP_FILE).exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn expired_logins_and_logins_of_disabled_users_are_not_taken_up_and_off_removes_the_file() {
        let (mut a, d) = set_up("auth-sessions-old");
        a.set_persist_sessions(true);
        let now = crate::jobs::now();
        // a login that ended a day ago
        let (old, _, _) = a
            .login("juan", "una-clave-larga", now - 2 * 86_400)
            .unwrap();
        a.add_user("bob", Role::Viewer, "clave-de-bob-1").unwrap();
        let (bob, _, _) = a.login("bob", "clave-de-bob-1", now).unwrap();
        let mut b = Auth::open(&d, 12).unwrap();
        b.users
            .iter_mut()
            .find(|u| u.name == "bob")
            .unwrap()
            .disabled = true;
        b.set_persist_sessions(true);
        assert!(b.identify(&old, now).is_none());
        assert!(b.identify(&bob, now).is_none());
        b.set_persist_sessions(false);
        assert!(!d.join("sessions.json").exists());
        let mut c = Auth::open(&d, 12).unwrap();
        c.set_persist_sessions(false);
        let (t, _, _) = c.login("juan", "una-clave-larga", now).unwrap();
        assert!(!d.join("sessions.json").exists(), "off: nothing is written");
        assert!(c.identify(&t, now).is_some());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn names_and_passwords_are_checked() {
        assert!(
            valid_name("juan.cano-2")
                && !valid_name("")
                && !valid_name("a b")
                && !valid_name(&"x".repeat(33))
        );
        assert!(
            valid_password("12345678")
                && !valid_password("1234567")
                && !valid_password(&"x".repeat(129))
        );
        assert!(same("abc", "abc") && !same("abc", "abd") && !same("abc", "ab"));
    }

    #[test]
    fn the_scopes_of_a_design_program_never_include_those_of_the_machines() {
        let all: Vec<String> = SCOPES.iter().map(|s| s.to_string()).collect();
        // without scopes a phoenix or cam token gets all it may hold, which is not the machines
        for template in ["phoenix", "cam"] {
            let got = effective_scopes(Some(template), None).unwrap();
            assert!(
                got.contains(&"tasks:run".to_string()) && got.contains(&"jobs:prepare".to_string())
            );
            for forbidden in MACHINE_SCOPES {
                assert!(
                    !got.contains(&forbidden.to_string()),
                    "{template} {forbidden}"
                );
            }
            // and a file that was edited by hand to give one is not believed
            let edited = effective_scopes(Some(template), Some(&all)).unwrap();
            assert_eq!(edited, got);
        }
        // the others keep what they were given, and none given means no limit
        assert_eq!(effective_scopes(Some("client"), Some(&all)).unwrap(), all);
        assert_eq!(effective_scopes(Some("custom"), None), None);
        assert_eq!(effective_scopes(None, None), None);
        assert_eq!(
            effective_scopes(None, Some(&["tasks:read".to_string()])),
            Some(vec!["tasks:read".to_string()])
        );
    }

    #[test]
    fn a_token_is_made_with_checked_scopes_and_a_known_template() {
        let (mut a, d) = set_up("scopes");
        let give = |scopes: &[&str]| scopes.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(a
            .create_token_with(
                "p",
                Role::Operator,
                1,
                None,
                Some("phoenix"),
                Some(&give(&["tasks:run", "tasks:run"]))
            )
            .is_ok());
        assert_eq!(
            a.tokens().last().unwrap().scopes,
            Some(give(&["tasks:run"])),
            "repeats are dropped"
        );
        for (template, scopes, error) in [
            (
                Some("phoenix"),
                vec!["jobs:start"],
                AuthError::ForbiddenScope,
            ),
            (
                Some("cam"),
                vec!["jobs:authorize"],
                AuthError::ForbiddenScope,
            ),
            (Some("custom"), vec!["whatever"], AuthError::BadScope),
            (Some("robot"), vec![], AuthError::BadTemplate),
        ] {
            let got =
                a.create_token_with("q", Role::Operator, 1, None, template, Some(&give(&scopes)));
            assert_eq!(got.unwrap_err(), error, "{template:?} {scopes:?}");
        }
        let before = a.tokens().len();
        assert!(a
            .create_token_with(
                "q",
                Role::Operator,
                1,
                None,
                Some("custom"),
                Some(&give(&["jobs:start"]))
            )
            .is_ok());
        assert_eq!(a.tokens().len(), before + 1, "the refusals made nothing");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_rotated_token_keeps_its_id_and_its_limits_and_loses_its_old_secret() {
        let (mut a, d) = set_up("rotate");
        let (id, old) = a
            .create_token_with(
                "p",
                Role::Operator,
                1,
                None,
                Some("custom"),
                Some(&["tasks:read".to_string()]),
            )
            .unwrap();
        assert!(a.identify(&old, 2).is_some());
        let new = a.rotate_token(&id).unwrap();
        assert_ne!(new, old);
        assert!(
            a.identify(&old, 3).is_none(),
            "the old secret stops at once"
        );
        let who = a.identify(&new, 3).unwrap();
        assert_eq!(who.token_id.as_deref(), Some(id.as_str()));
        assert_eq!(who.scopes, Some(vec!["tasks:read".to_string()]));
        assert!(who.allows("tasks:read") && !who.allows("tasks:run"));
        assert_eq!(a.rotate_token("nope"), Err(AuthError::UnknownToken));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn the_policy_of_the_passwords_is_the_settings() {
        let strict = PasswordPolicy {
            min_len: 12,
            classes: 3,
            deny_name: true,
        };
        assert!(strict.allows("juan", "Clave-larga-77"));
        // too short, too few classes, the name inside (in any letter case)
        assert!(!strict.allows("juan", "Cl-77"));
        assert!(!strict.allows("juan", "clavelargaclave"));
        assert!(!strict.allows("juan", "xxJUAN-clave-77"));
        // a name shorter than four characters is not looked for
        assert!(strict.allows("ana", "Ana-clave-larga-77"));
        assert!(!strict.allows("juan", &"Aa1-".repeat(33)));
        let plain = PasswordPolicy::default();
        assert!(plain.allows("juan", "12345678") && !plain.allows("juan", "1234567"));
        let (mut a, d) = set_up("policy");
        a.set_password_policy(strict);
        assert_eq!(
            a.add_user("ana2", Role::Viewer, "corta"),
            Err(AuthError::WeakPassword)
        );
        assert_eq!(
            a.add_user("ana2", Role::Viewer, "Ana2-clave-larga"),
            Err(AuthError::WeakPassword)
        );
        assert!(a.add_user("ana2", Role::Viewer, "Clave-larga-77").is_ok());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_login_of_an_application_is_listed_closed_and_renewed() {
        let (mut a, d) = set_up("app-sessions");
        let ip: std::net::IpAddr = "10.0.0.5".parse().unwrap();
        let (phone, _, secs, phone_id) = a
            .login_with("juan", "una-clave-larga", 100, Some(ip), "android", 0)
            .unwrap();
        let (studio, _, _, studio_id) = a
            .login_with("juan", "una-clave-larga", 200, None, "studio", 0)
            .unwrap();
        assert_ne!(phone_id, studio_id);
        assert_eq!(a.app_of(&phone).as_deref(), Some("android"));
        assert_eq!(a.app_of("nope"), None);
        // a login made without an application has none
        let (plain, _, _) = a.login("juan", "una-clave-larga", 300).unwrap();
        assert_eq!(a.app_of(&plain), None);
        let list = a.sessions(Some("juan"), 400);
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].created, 300);
        let first = list.iter().find(|s| s.id == phone_id).unwrap();
        assert_eq!(
            (first.app.as_str(), first.ip.as_str()),
            ("android", "10.0.0.5")
        );
        assert!(a.sessions(Some("nobody"), 400).is_empty());
        // seen when it is used
        a.identify(&phone, 350).unwrap();
        assert_eq!(a.session_of(&phone).unwrap().last_seen, 350);
        // renewed up to the limit counted from the moment it was made
        a.set_refresh_max_hours(24);
        assert_eq!(a.refresh(&phone, 100 + secs - 10).unwrap(), secs);
        // never past the limit counted from the moment it was made (100 + 24 hours)
        assert_eq!(a.refresh(&phone, 86_400).unwrap(), 100);
        assert_eq!(a.refresh("nope", 500), Err(AuthError::UnknownSession));
        // closed by id: only the person's own when asked so
        assert_eq!(
            a.close_session(&studio_id, Some("ana")),
            Err(AuthError::UnknownSession)
        );
        assert_eq!(
            a.close_session(&studio_id, Some("juan")).unwrap().app,
            "studio"
        );
        assert!(a.identify(&studio, 500).is_none());
        assert_eq!(
            a.close_session(&studio_id, None),
            Err(AuthError::UnknownSession)
        );
        assert_eq!(a.close_sessions_of("juan"), 2);
        assert!(a.identify(&phone, 500).is_none());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn the_most_logins_of_a_person_close_the_oldest_and_the_lockout_is_the_settings() {
        let (mut a, d) = set_up("app-limits");
        let (first, _, _, _) = a
            .login_with("juan", "una-clave-larga", 10, None, "android", 2)
            .unwrap();
        let (second, _, _, _) = a
            .login_with("juan", "una-clave-larga", 20, None, "ios", 2)
            .unwrap();
        let (third, _, _, _) = a
            .login_with("juan", "una-clave-larga", 30, None, "studio", 2)
            .unwrap();
        assert!(a.identify(&first, 40).is_none(), "the oldest was closed");
        assert!(a.identify(&second, 40).is_some() && a.identify(&third, 40).is_some());
        // three wrong passwords block the name for 7 seconds
        a.set_lockout(3, 7);
        for _ in 0..3 {
            assert_eq!(
                a.login_with("juan", "mala-clave-larga", 100, None, "studio", 0),
                Err(AuthError::BadCredentials)
            );
        }
        assert_eq!(
            a.login_with("juan", "una-clave-larga", 101, None, "studio", 0),
            Err(AuthError::Throttled)
        );
        assert!(a
            .login_with("juan", "una-clave-larga", 107, None, "studio", 0)
            .is_ok());
        let _ = std::fs::remove_dir_all(d);
    }
}
