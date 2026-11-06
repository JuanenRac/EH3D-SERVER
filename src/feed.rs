//! The feed of events of the common services: what happens to the projects, the machines and the jobs of every application, as it happens.
//!
//! Every event has a number that goes up by one, a kind (`project.file`, `job.state`...), the machine, the project and the job it is about (so a client can listen to
//! only what it cares for), and who may hear it. The last `events_queue` events are kept, so a client that lost its connection asks again with `Last-Event-ID` and
//! gets what it missed; a client that falls behind the live flow by more than that is told to ask for the state again. The stream itself is `GET /api/v1/events/stream`
//! (server-sent events); the websocket of the first version (`GET /api/v1/events`) is not touched.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::sync::broadcast;

use crate::auth::{Identity, Role};
use crate::pstore::{ProjectMeta, Version};
use crate::state::AppState;
use crate::sync::Locked;

/// Who may hear an event.
#[derive(Clone, Debug, PartialEq)]
pub enum Visibility {
    /// Everybody who is logged in (the state of the machines and of the jobs, as `GET /jobs` shows it).
    Everyone,
    /// The person with this owner key and the administrators.
    Owner(String),
}

#[derive(Clone, Debug)]
pub struct FeedEvent {
    pub id: u64,
    pub kind: String,
    pub machine: Option<String>,
    pub project: Option<String>,
    pub job: Option<String>,
    pub app: Option<String>,
    pub visibility: Visibility,
    pub data: Value,
}

impl FeedEvent {
    pub fn new(kind: &str, visibility: Visibility, data: Value) -> FeedEvent {
        FeedEvent {
            id: 0,
            kind: kind.to_string(),
            machine: None,
            project: None,
            job: None,
            app: None,
            visibility,
            data,
        }
    }

    pub fn machine(mut self, id: &str) -> FeedEvent {
        self.machine = Some(id.to_string());
        self
    }

    pub fn project(mut self, id: &str) -> FeedEvent {
        self.project = Some(id.to_string());
        self
    }

    pub fn job(mut self, id: &str) -> FeedEvent {
        self.job = Some(id.to_string());
        self
    }

    pub fn app(mut self, name: &str) -> FeedEvent {
        self.app = Some(name.to_string());
        self
    }

    /// The scope a token needs to hear this event (a token without scopes hears all it is allowed to see).
    pub fn scope(&self) -> Option<&'static str> {
        let family = self.kind.split('.').next().unwrap_or("");
        match family {
            "project" => Some("projects:read"),
            "job" | "machine" | "notification" => Some("machines:view"),
            "catalog" => Some("catalog:read"),
            _ => None,
        }
    }

    pub fn visible_to(&self, who: &Identity) -> bool {
        match &self.visibility {
            Visibility::Everyone => true,
            Visibility::Owner(owner) => who.role == Role::Admin || *owner == who.owner(),
        }
    }

    /// The event as the stream sends it in the `data:` line.
    pub fn text(&self) -> String {
        json!({
            "id": self.id, "kind": self.kind, "machine": self.machine, "project": self.project,
            "job": self.job, "app": self.app, "data": self.data,
        })
        .to_string()
    }
}

/// What a client listens to: every field given has to match (a field not given matches everything).
#[derive(Clone, Debug, Default)]
pub struct Filter {
    pub machine: Option<String>,
    pub project: Option<String>,
    pub job: Option<String>,
    pub app: Option<String>,
    /// Kinds, or the start of a kind written with a dot (`job` matches `job.state`).
    pub kinds: Vec<String>,
}

impl Filter {
    pub fn passes(&self, e: &FeedEvent) -> bool {
        let same = |want: &Option<String>, have: &Option<String>| match want {
            Some(w) => have.as_deref() == Some(w.as_str()),
            None => true,
        };
        if !(same(&self.machine, &e.machine)
            && same(&self.project, &e.project)
            && same(&self.job, &e.job)
            && same(&self.app, &e.app))
        {
            return false;
        }
        self.kinds.is_empty()
            || self
                .kinds
                .iter()
                .any(|k| e.kind == *k || e.kind.starts_with(&format!("{k}.")))
    }
}

struct Inner {
    next: u64,
    ring: VecDeque<FeedEvent>,
}

pub struct Feed {
    inner: Mutex<Inner>,
    tx: broadcast::Sender<FeedEvent>,
    clients: Arc<AtomicUsize>,
    capacity: usize,
    max_clients: usize,
}

/// A place taken in the limit of clients; it is given back when it is dropped.
pub struct ClientGuard {
    clients: Arc<AtomicUsize>,
}

impl Drop for ClientGuard {
    fn drop(&mut self) {
        self.clients.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Feed {
    pub fn new(capacity: usize, max_clients: usize) -> Feed {
        let (tx, _) = broadcast::channel(capacity.max(16));
        Feed {
            inner: Mutex::new(Inner {
                next: 1,
                ring: VecDeque::new(),
            }),
            tx,
            clients: Arc::new(AtomicUsize::new(0)),
            capacity: capacity.max(16),
            max_clients,
        }
    }

    /// Gives the event its number, keeps it and tells the clients that listen. Returns the number.
    pub fn publish(&self, mut event: FeedEvent) -> u64 {
        let mut inner = self.inner.locked();
        event.id = inner.next;
        inner.next += 1;
        inner.ring.push_back(event.clone());
        while inner.ring.len() > self.capacity {
            inner.ring.pop_front();
        }
        let id = event.id;
        // nobody listening is not a problem
        let _ = self.tx.send(event);
        id
    }

    pub fn subscribe(&self) -> broadcast::Receiver<FeedEvent> {
        self.tx.subscribe()
    }

    /// The events kept after number `last`, oldest first.
    pub fn since(&self, last: u64) -> Vec<FeedEvent> {
        self.inner
            .locked()
            .ring
            .iter()
            .filter(|e| e.id > last)
            .cloned()
            .collect()
    }

    /// The number the last event got (0: none yet).
    pub fn last_id(&self) -> u64 {
        self.inner.locked().next - 1
    }

    /// Takes a place for a client that listens; `None` when `events_clients_max` are listening already.
    pub fn join(&self) -> Option<ClientGuard> {
        let taken = self.clients.fetch_add(1, Ordering::SeqCst);
        if taken >= self.max_clients {
            self.clients.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(ClientGuard {
            clients: self.clients.clone(),
        })
    }

    pub fn clients(&self) -> usize {
        self.clients.load(Ordering::SeqCst)
    }
}

impl AppState {
    fn project_visibility(&self, owner: &str) -> Visibility {
        if self.cfg.project_shared {
            Visibility::Everyone
        } else {
            Visibility::Owner(owner.to_string())
        }
    }

    /// A project of the store was made, changed or removed.
    pub fn announce_project(&self, kind: &str, m: &ProjectMeta) {
        let data = json!({ "name": m.name, "owner": m.owner, "etag": m.etag() });
        self.feed.publish(
            FeedEvent::new(kind, self.project_visibility(&m.owner), data)
                .app(&m.app)
                .project(&m.id),
        );
    }

    /// A file of a project got a new version.
    pub fn announce_file(
        &self,
        kind: &str,
        app: &str,
        project: &str,
        name: &str,
        version: &Version,
        who: &Identity,
    ) {
        let owner = self.owner_of_project(project, who);
        let data =
            json!({ "file": name, "version": version.n, "sha256": version.sha256, "by": who.name });
        self.feed.publish(
            FeedEvent::new(kind, self.project_visibility(&owner), data)
                .app(app)
                .project(project),
        );
    }

    pub fn announce_removed_file(&self, app: &str, project: &str, name: &str, who: &Identity) {
        let owner = self.owner_of_project(project, who);
        let data = json!({ "file": name, "by": who.name });
        self.feed.publish(
            FeedEvent::new(
                "project.file_removed",
                self.project_visibility(&owner),
                data,
            )
            .app(app)
            .project(project),
        );
    }

    /// Somebody took the editing lock of a project or let it go.
    pub fn announce_lock(&self, app: &str, project: &str, locked: bool, who: &Identity) {
        let owner = self.owner_of_project(project, who);
        let kind = if locked {
            "project.locked"
        } else {
            "project.unlocked"
        };
        self.feed.publish(
            FeedEvent::new(
                kind,
                self.project_visibility(&owner),
                json!({ "by": who.name }),
            )
            .app(app)
            .project(project),
        );
    }

    /// The owner of a project of the store (the caller when it is gone, which only an event of its removal can say).
    fn owner_of_project(&self, project: &str, fallback: &Identity) -> String {
        self.pstore
            .book
            .locked()
            .owner_of(project)
            .unwrap_or_else(|| fallback.owner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn event(kind: &str) -> FeedEvent {
        FeedEvent::new(kind, Visibility::Everyone, json!({}))
    }

    #[test]
    fn the_events_are_numbered_and_the_last_ones_are_kept() {
        let feed = Feed::new(16, 2);
        for _ in 0..20 {
            feed.publish(event("job.state"));
        }
        assert_eq!(feed.last_id(), 20);
        let kept = feed.since(0);
        assert_eq!(kept.len(), 16);
        assert_eq!(kept[0].id, 5);
        assert_eq!(feed.since(18).len(), 2);
        assert!(feed.since(20).is_empty());
    }

    #[test]
    fn a_filter_keeps_what_was_asked_for() {
        let e = event("job.state").machine("m1").job("w-1").app("studio");
        assert!(Filter::default().passes(&e));
        let mut f = Filter {
            machine: Some("m1".into()),
            ..Filter::default()
        };
        assert!(f.passes(&e));
        f.machine = Some("m2".into());
        assert!(!f.passes(&e));
        let by_kind = |k: &str| Filter {
            kinds: vec![k.to_string()],
            ..Filter::default()
        };
        assert!(by_kind("job").passes(&e));
        assert!(by_kind("job.state").passes(&e));
        assert!(!by_kind("jo").passes(&e));
        assert!(!by_kind("project").passes(&e));
        // an event without a project does not pass a filter that asks for one
        let p = Filter {
            project: Some("p-1".into()),
            ..Filter::default()
        };
        assert!(!p.passes(&e));
    }

    #[test]
    fn an_event_of_an_owner_is_heard_by_the_owner_and_the_administrators() {
        let e = FeedEvent::new("project.file", Visibility::Owner("ana".into()), json!({}));
        assert!(e.visible_to(&person("ana", Role::Viewer)));
        assert!(e.visible_to(&person("root", Role::Admin)));
        assert!(!e.visible_to(&person("bob", Role::Operator)));
        assert!(event("job.state").visible_to(&person("bob", Role::Viewer)));
    }

    #[test]
    fn the_clients_are_limited_and_give_their_place_back() {
        let feed = Feed::new(16, 2);
        let a = feed.join().unwrap();
        let _b = feed.join().unwrap();
        assert!(feed.join().is_none());
        assert_eq!(feed.clients(), 2);
        drop(a);
        assert_eq!(feed.clients(), 1);
        assert!(feed.join().is_some());
    }

    #[test]
    fn a_listener_receives_what_is_published_after_it_subscribed() {
        let feed = Feed::new(16, 2);
        feed.publish(event("job.state"));
        let mut rx = feed.subscribe();
        feed.publish(event("job.done"));
        let got = rx.try_recv().unwrap();
        assert_eq!(got.kind, "job.done");
        assert_eq!(got.id, 2);
        assert!(rx.try_recv().is_err());
    }
}
