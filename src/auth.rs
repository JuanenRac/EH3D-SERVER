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
}

/// Who a request comes from.
#[derive(Clone, Debug, PartialEq)]
pub struct Identity {
    pub name: String,
    pub role: Role,
    /// True for a program's token, false for a person logged in.
    pub is_token: bool,
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
            AuthError::Storage => "storage",
        }
    }
}

struct Session {
    who: Identity,
    expires: u64,
}

pub struct Auth {
    dir: PathBuf,
    users: Vec<User>,
    tokens: Vec<ApiToken>,
    sessions: HashMap<String, Session>,
    setup_code: Option<String>,
    /// Wrong passwords per name: how many in a row and until when the name is blocked.
    failures: HashMap<String, (u32, u64)>,
    session_secs: u64,
}

pub const TOKEN_PREFIX: &str = "eh3d_";
const MAX_FAILURES: u32 = 5;

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
            session_secs: u64::from(session_hours) * 3600,
        })
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
        match &self.setup_code {
            Some(expected) if self.users.is_empty() && same(expected, code.trim()) => {}
            _ => return Err(AuthError::BadSetup),
        }
        if !valid_name(name) {
            return Err(AuthError::BadName);
        }
        if !valid_password(password) {
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
            let entry = self.failures.entry(name.to_string()).or_insert((0, 0));
            entry.0 += 1;
            if entry.0 >= MAX_FAILURES {
                // 30 s, then 60 s, 120 s... for as long as the wrong passwords go on
                entry.1 = now + (30u64 << (entry.0 - MAX_FAILURES).min(6));
            }
            return Err(AuthError::BadCredentials);
        }
        self.failures.remove(name);
        let role = user.map(|u| u.role).unwrap_or(Role::Viewer);
        let token = random_hex(32);
        self.sessions.insert(
            sha(&token),
            Session {
                who: Identity {
                    name: name.into(),
                    role,
                    is_token: false,
                },
                expires: now + self.session_secs,
            },
        );
        Ok((token, role, self.session_secs))
    }

    pub fn logout(&mut self, token: &str) {
        self.sessions.remove(&sha(token));
    }

    /// Who a bearer token belongs to: a session of a person, or a program's token.
    pub fn identify(&mut self, bearer: &str, now: u64) -> Option<Identity> {
        let hash = sha(bearer);
        if bearer.starts_with(TOKEN_PREFIX) {
            return self
                .tokens
                .iter()
                .find(|t| same(&t.hash, &hash))
                .map(|t| Identity {
                    name: t.name.clone(),
                    role: t.role,
                    is_token: true,
                });
        }
        self.sessions.retain(|_, s| s.expires > now);
        let session = self.sessions.get(&hash)?;
        // a user that was deleted or disabled since the login no longer counts
        let user = self
            .users
            .iter()
            .find(|u| u.name == session.who.name && !u.disabled)?;
        Some(Identity {
            name: user.name.clone(),
            role: user.role,
            is_token: false,
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
        if !valid_password(password) {
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
            if !valid_password(p) {
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

    /// Makes a token for a program; the secret is returned this once and never kept.
    pub fn create_token(
        &mut self,
        name: &str,
        role: Role,
        now: u64,
    ) -> Result<(String, String), AuthError> {
        if name.is_empty() || name.len() > 64 {
            return Err(AuthError::BadName);
        }
        let secret = format!("{TOKEN_PREFIX}{}", random_hex(24));
        let id = random_hex(4);
        self.tokens.push(ApiToken {
            id: id.clone(),
            name: name.into(),
            role,
            hash: sha(&secret),
            created: now,
        });
        self.save_tokens()?;
        Ok((id, secret))
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
        let (id, secret) = a.create_token("drillpro-pc", Role::Operator, 99).unwrap();
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
}
