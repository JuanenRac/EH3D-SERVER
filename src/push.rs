//! Notices on a phone through Firebase Cloud Messaging (HTTP v1), off unless `push_enabled` is on.
//!
//! A message carries no text and no name: only which of three things happened (`waiting`, `failed`, `finished`). The app words the notice in its own language and
//! asks this server for the rest, so nothing about a machine or a file passes through Google. The server needs the service account file of the owner's own Firebase
//! project (`push_service_account_file`); that file stays on this computer and its contents are never logged or answered.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use base64::Engine;
use ring::signature::RsaKeyPair;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::auth::Role;
use crate::api::Shared;
use crate::store;
use crate::sync::Locked;

/// The phones the server keeps.
pub const MAX_DEVICES: usize = 100;
/// The longest registration token that is taken.
pub const MAX_TOKEN: usize = 4096;
const SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";
const FCM: &str = "https://fcm.googleapis.com";

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Device {
    pub token: String,
    /// The person who registered the phone: it gets notices while that account may run jobs.
    pub user: String,
    pub added: u64,
}

/// Whether a text can be a registration token.
pub fn token_ok(token: &str) -> bool {
    !token.is_empty() && token.len() <= MAX_TOKEN && token.bytes().all(|b| b.is_ascii_graphic())
}

/// The registered phones, kept in `push_devices.json` of the data folder.
pub struct Devices {
    path: PathBuf,
    list: Mutex<Vec<Device>>,
}

impl Devices {
    pub fn open(data_dir: &Path) -> Devices {
        let path = data_dir.join("push_devices.json");
        // a damaged or missing list is an empty one: a phone registers again each time its app starts
        let list = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Devices {
            path,
            list: Mutex::new(list),
        }
    }

    /// Adds a phone, or moves it to the person who registers it now. False when the list is full.
    pub fn add(&self, token: &str, user: &str, now: u64) -> std::io::Result<bool> {
        let mut list = self.list.locked();
        list.retain(|d| d.token != token);
        if list.len() >= MAX_DEVICES {
            return Ok(false);
        }
        list.push(Device {
            token: token.to_string(),
            user: user.to_string(),
            added: now,
        });
        store::write_json(&self.path, &*list)?;
        Ok(true)
    }

    /// Removes a phone; false when it was not there.
    pub fn remove(&self, token: &str) -> std::io::Result<bool> {
        let mut list = self.list.locked();
        let before = list.len();
        list.retain(|d| d.token != token);
        if list.len() == before {
            return Ok(false);
        }
        store::write_json(&self.path, &*list)?;
        Ok(true)
    }

    pub fn all(&self) -> Vec<Device> {
        self.list.locked().clone()
    }
}

/// The notice a change of state of a job asks for, from the state seen before.
pub fn notice_for(before: Option<&str>, after: &str) -> Option<&'static str> {
    if before == Some(after) {
        return None;
    }
    let was_active = matches!(before, Some("connecting" | "running" | "waiting_operator"));
    match after {
        "waiting_operator" => Some("waiting"),
        "failed" if was_active || before == Some("ready") => Some("failed"),
        "done" | "cancelled" if was_active => Some("finished"),
        _ => None,
    }
}

/// What is needed of the service account file.
#[derive(Deserialize, Clone)]
pub struct Account {
    pub project_id: String,
    pub client_email: String,
    private_key: String,
    pub token_uri: String,
}

impl Account {
    pub fn parse(text: &str) -> Result<Account, String> {
        // the error never quotes the file: it holds a private key
        let a: Account = serde_json::from_str(text)
            .map_err(|_| "it is not a service account file".to_string())?;
        let project_ok = !a.project_id.is_empty()
            && a.project_id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !project_ok || a.client_email.is_empty() {
            return Err("its project or account name is not valid".to_string());
        }
        if !(a.token_uri.starts_with("https://") || a.token_uri.starts_with("http://127.0.0.1:")) {
            return Err("its token_uri is not an https address".to_string());
        }
        Ok(a)
    }

    pub fn read(path: &Path) -> Result<Account, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("it cannot be read ({})", e.kind()))?;
        Account::parse(&text)
    }

    fn key_der(&self) -> Result<Vec<u8>, String> {
        let body: String = self
            .private_key
            .lines()
            .filter(|line| !line.starts_with("-----"))
            .map(str::trim)
            .collect();
        base64::engine::general_purpose::STANDARD
            .decode(body)
            .map_err(|_| "its private key is not readable".to_string())
    }

    /// The RS256 signature of `message` with the key of the account.
    pub fn sign(&self, message: &[u8]) -> Result<Vec<u8>, String> {
        let key = RsaKeyPair::from_pkcs8(&self.key_der()?)
            .map_err(|_| "its private key is not an RSA key in PKCS#8".to_string())?;
        let mut signature = vec![0; key.public().modulus_len()];
        key.sign(
            &ring::signature::RSA_PKCS1_SHA256,
            &ring::rand::SystemRandom::new(),
            message,
            &mut signature,
        )
        .map_err(|_| "the signature failed".to_string())?;
        Ok(signature)
    }
}

type Signer = Box<dyn Fn(&Account, &[u8]) -> Result<Vec<u8>, String> + Send>;

/// The assertion the account signs to ask for an access token.
pub fn assertion(account: &Account, now: u64, signer: &Signer) -> Result<String, String> {
    let b64 = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let header = b64(br#"{"alg":"RS256","typ":"JWT"}"#);
    let claims = json!({
        "iss": account.client_email, "scope": SCOPE, "aud": account.token_uri, "iat": now, "exp": now + 3600,
    });
    let unsigned = format!("{header}.{}", b64(claims.to_string().as_bytes()));
    let signature = signer(account, unsigned.as_bytes())?;
    Ok(format!("{unsigned}.{}", b64(&signature)))
}

/// What happened to one message.
#[derive(Debug, PartialEq, Eq)]
pub enum Sent {
    Ok,
    /// Firebase no longer knows the phone: it is forgotten.
    Gone,
}

/// The link to Firebase: it keeps the access token until shortly before it expires.
pub struct Sender {
    account: Account,
    base: String,
    signer: Signer,
    agent: ureq::Agent,
    access: Option<(String, u64)>,
}

impl Sender {
    pub fn new(account: Account) -> Sender {
        Sender::with(account, FCM.to_string(), Box::new(Account::sign))
    }

    /// The same with another address for Firebase and another signer (the tests).
    pub fn with(account: Account, base: String, signer: Signer) -> Sender {
        Sender {
            account,
            base,
            signer,
            agent: ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(10))
                .redirects(0)
                .build(),
            access: None,
        }
    }

    fn access_token(&mut self, now: u64) -> Result<String, String> {
        if let Some((token, until)) = &self.access {
            if now < *until {
                return Ok(token.clone());
            }
        }
        let jwt = assertion(&self.account, now, &self.signer)?;
        let answer = self
            .agent
            .post(&self.account.token_uri)
            .send_form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", &jwt),
            ])
            .map_err(|e| format!("no access token: {}", short(&e)))?;
        let body: Value = answer
            .into_string()
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .ok_or("no access token: the answer is not JSON")?;
        let token = body["access_token"]
            .as_str()
            .ok_or("no access token in the answer")?
            .to_string();
        let life = body["expires_in"].as_u64().unwrap_or(3600).min(3600);
        self.access = Some((token.clone(), now + life.saturating_sub(60)));
        Ok(token)
    }

    pub fn send(&mut self, device: &str, event: &str, now: u64) -> Result<Sent, String> {
        let access = self.access_token(now)?;
        let url = format!(
            "{}/v1/projects/{}/messages:send",
            self.base, self.account.project_id
        );
        let message = json!({"message": {"token": device, "data": {"eh3d": event}, "android": {"priority": "high"}}});
        match self
            .agent
            .post(&url)
            .set("Authorization", &format!("Bearer {access}"))
            .set("Content-Type", "application/json")
            .send_string(&message.to_string())
        {
            Ok(_) => Ok(Sent::Ok),
            Err(ureq::Error::Status(404, _)) => Ok(Sent::Gone),
            Err(ureq::Error::Status(401, _)) => {
                self.access = None;
                Err("Firebase refused the access token".to_string())
            }
            Err(e) => Err(short(&e)),
        }
    }
}

/// An error without the address or the body, which could carry a token.
fn short(e: &ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, _) => format!("HTTP {code}"),
        ureq::Error::Transport(t) => format!("{}", t.kind()),
    }
}

/// Sends a notice to every phone whose person may still run jobs, and forgets the phones Firebase no longer knows.
pub fn deliver(state: &Shared, sender: &mut Sender, event: &str, now: u64) {
    let operators: Vec<String> = state
        .auth
        .locked()
        .users()
        .into_iter()
        .filter(|(_, role, disabled)| !disabled && *role >= Role::Operator)
        .map(|(name, _, _)| name)
        .collect();
    for device in state.push_devices.all() {
        if !operators.contains(&device.user) {
            continue;
        }
        match sender.send(&device.token, event, now) {
            Ok(Sent::Ok) => {}
            Ok(Sent::Gone) => {
                let _ = state.push_devices.remove(&device.token);
            }
            Err(why) => eprintln!("eh3d-server: a notice was not sent: {why}"),
        }
    }
}

/// Follows the events of the jobs and sends the notices. The switch is read for each one, so turning it off stops them at once.
pub fn spawn(state: Shared) -> std::thread::JoinHandle<()> {
    let mut events = state.events.subscribe();
    std::thread::spawn(move || {
        let mut seen: HashMap<String, String> = HashMap::new();
        let mut sender: Option<Sender> = None;
        let mut complained = false;
        loop {
            let text = match events.blocking_recv() {
                Ok(text) => text,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            let Ok(value) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            let (Some("job"), Some(id), Some(after)) = (
                value["type"].as_str(),
                value["job"]["id"].as_str(),
                value["job"]["state"].as_str(),
            ) else {
                continue;
            };
            let before = seen.insert(id.to_string(), after.to_string());
            let Some(event) = notice_for(before.as_deref(), after) else {
                continue;
            };
            if !state.policy.get().push_enabled || state.push_devices.all().is_empty() {
                continue;
            }
            if sender.is_none() {
                match Account::read(&state.cfg.push_service_account_file) {
                    Ok(account) => sender = Some(Sender::new(account)),
                    Err(why) => {
                        if !complained {
                            eprintln!("eh3d-server: no notices: push_service_account_file: {why}");
                            complained = true;
                        }
                    }
                }
            }
            if let Some(sender) = sender.as_mut() {
                deliver(&state, sender, event, crate::jobs::now());
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCOUNT: &str = r#"{"project_id":"my-project","client_email":"a@b.example","private_key":"x","token_uri":"https://oauth2.example/token"}"#;

    #[test]
    fn a_change_of_state_asks_for_one_of_three_notices() {
        assert_eq!(
            notice_for(Some("running"), "waiting_operator"),
            Some("waiting")
        );
        assert_eq!(
            notice_for(Some("waiting_operator"), "waiting_operator"),
            None
        );
        assert_eq!(notice_for(Some("running"), "failed"), Some("failed"));
        assert_eq!(notice_for(Some("ready"), "failed"), Some("failed"));
        assert_eq!(notice_for(Some("running"), "done"), Some("finished"));
        assert_eq!(
            notice_for(Some("waiting_operator"), "cancelled"),
            Some("finished")
        );
        assert_eq!(notice_for(Some("ready"), "cancelled"), None);
        assert_eq!(notice_for(None, "done"), None);
        assert_eq!(notice_for(Some("done"), "done"), None);
        assert_eq!(notice_for(Some("ready"), "running"), None);
    }

    #[test]
    fn only_a_printable_token_of_a_sane_length_is_taken() {
        assert!(token_ok("abc:DEF-123_x"));
        assert!(!token_ok(""));
        assert!(!token_ok("a b"));
        assert!(!token_ok("a\n"));
        assert!(!token_ok(&"a".repeat(MAX_TOKEN + 1)));
    }

    #[test]
    fn the_account_file_is_checked_and_its_errors_do_not_quote_it() {
        let a = Account::parse(ACCOUNT).expect("an account");
        assert_eq!(a.project_id, "my-project");
        for bad in [
            "nonsense-secret",
            &ACCOUNT.replace("my-project", "../x"),
            &ACCOUNT.replace("https://", "http://"),
        ] {
            let why = Account::parse(bad).err().expect("refused");
            assert!(!why.contains("secret"), "{why}");
        }
        assert!(a.sign(b"message").is_err());
    }

    #[test]
    fn the_assertion_names_the_account_and_carries_the_signature() {
        let a = Account::parse(ACCOUNT).expect("an account");
        let signer: Signer = Box::new(|_, message| Ok(message[..4].to_vec()));
        let jwt = assertion(&a, 1_000, &signer).expect("a JWT");
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(parts[1])
            .unwrap();
        let claims: Value = serde_json::from_slice(&claims).unwrap();
        assert_eq!(claims["iss"], "a@b.example");
        assert_eq!(claims["exp"], 4_600);
        assert_eq!(claims["scope"], SCOPE);
    }

    /// A stand-in for Google on this computer: it answers each request with the next status and body and keeps what it received.
    fn fake(answers: Vec<(u16, &'static str)>) -> (u16, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for (status, body) in answers {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut buf = [0u8; 1024];
                loop {
                    let n = stream.read(&mut buf).unwrap();
                    request.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&request).to_string();
                    let done = text.split_once("\r\n\r\n").is_some_and(|(head, rest)| {
                        let length = head
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        rest.len() >= length
                    });
                    if done || n == 0 {
                        break;
                    }
                }
                seen.push(String::from_utf8_lossy(&request).to_string());
                let answer = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(answer.as_bytes()).unwrap();
            }
            seen
        });
        (port, handle)
    }

    #[test]
    fn a_message_carries_the_event_only_and_a_forgotten_phone_is_said() {
        let (port, google) = fake(vec![
            (200, r#"{"access_token":"T","expires_in":3600}"#),
            (200, "{}"),
            (404, "{}"),
            (500, "{}"),
        ]);
        let account = Account::parse(&ACCOUNT.replace(
            "https://oauth2.example/token",
            &format!("http://127.0.0.1:{port}/token"),
        ))
        .unwrap();
        let mut sender = Sender::with(
            account,
            format!("http://127.0.0.1:{port}"),
            Box::new(|_, _| Ok(vec![1, 2, 3])),
        );
        assert_eq!(sender.send("phone-1", "waiting", 10), Ok(Sent::Ok));
        assert_eq!(sender.send("phone-2", "failed", 11), Ok(Sent::Gone));
        assert_eq!(
            sender.send("phone-3", "finished", 12),
            Err("HTTP 500".to_string())
        );
        let seen = google.join().unwrap();
        assert!(seen[0].starts_with("POST /token "), "{}", seen[0]);
        assert!(
            seen[1].starts_with("POST /v1/projects/my-project/messages:send "),
            "{}",
            seen[1]
        );
        assert!(seen[1].contains("Authorization: Bearer T"), "{}", seen[1]);
        let body = seen[1].split_once("\r\n\r\n").unwrap().1;
        let body: Value = serde_json::from_str(body).unwrap();
        assert_eq!(
            body,
            json!({"message": {"token": "phone-1", "data": {"eh3d": "waiting"}, "android": {"priority": "high"}}})
        );
    }

    #[test]
    fn the_phones_are_kept_once_each_and_survive_a_restart() {
        let dir = std::env::temp_dir().join(format!("eh3d-push-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let devices = Devices::open(&dir);
        assert!(devices.add("one", "ana", 1).unwrap());
        assert!(devices.add("one", "luis", 2).unwrap());
        assert_eq!(devices.all().len(), 1);
        assert_eq!(devices.all()[0].user, "luis");
        for i in 1..MAX_DEVICES {
            assert!(devices.add(&format!("t{i}"), "ana", 3).unwrap());
        }
        assert!(!devices.add("one-more", "ana", 4).unwrap());
        assert_eq!(Devices::open(&dir).all().len(), MAX_DEVICES);
        assert!(devices.remove("one").unwrap());
        assert!(!devices.remove("one").unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
