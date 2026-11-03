//! Projects, their revisions and who they are shared with.
//!
//! A project is a name, an owner and a line of **revisions**; each revision points to an artifact (the file of the design at that moment). The server keeps the file as it is
//! (the `.phoenixproj` or `.eh3dproj` is an opaque artifact to it) and never needs to understand it. A program never needs the server to open, edit and save a local project.
//!
//! **Who sees a project.** Its owner, the administrators and the people it was shared with, explicitly, one by one: with `read` they see the project and the artifacts of its
//! revisions, with `write` they may also add revisions. Nothing is shared by default, and a project that is not visible does not exist for the caller (`404 unknown_project`).
//! The sharing is the owner's (or an administrator's): a person it was shared with cannot share it on.
//!
//! **Two people saving at once.** A revision says which revision it follows (`parent`, 0 for the first); the server accepts it only when that is still the last one
//! (`409 stale_parent` otherwise), so one of two concurrent saves wins and the other is told to look at what changed.
//!
//! **Deleting.** Deleting a project hides it and keeps its revisions until an administrator purges it, which is audited.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::auth::{Identity, Role};
use crate::jobs::now;
use crate::store;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    Read,
    Write,
}

/// One person (a user, or a program's token) the owner shared something with.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Share {
    /// The owner key of the person (`Identity::owner`): a user name, or `token:<id>`.
    pub who: String,
    pub access: Access,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Revision {
    /// 1 for the first, then up by one.
    pub number: u64,
    pub artifact_id: String,
    pub by: String,
    pub at: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ProjectRecord {
    pub id: String,
    pub name: String,
    pub owner: String,
    pub created: u64,
    pub revisions: Vec<Revision>,
    pub shares: Vec<Share>,
    /// Hidden until an administrator purges it.
    #[serde(default)]
    pub deleted: bool,
}

impl ProjectRecord {
    /// What `who` may do with the project, if it may see it at all.
    pub fn access_of(&self, who: &Identity) -> Option<Access> {
        if self.deleted && who.role != Role::Admin {
            return None;
        }
        if who.role == Role::Admin || self.owner == who.owner() {
            return Some(Access::Write);
        }
        self.shares
            .iter()
            .find(|s| s.who == who.owner())
            .map(|s| s.access)
    }

    pub fn head(&self) -> u64 {
        self.revisions.last().map_or(0, |r| r.number)
    }

    /// The project as the API shows it; the list of shares only to those who manage it.
    pub fn view(&self, manager: bool) -> Value {
        let mut v = json!({
            "id": self.id, "name": self.name, "owner": self.owner, "created": self.created,
            "head": self.head(), "revisions": self.revisions, "deleted": self.deleted,
        });
        if manager {
            v["shares"] = json!(self.shares);
        }
        v
    }
}

#[derive(Debug, PartialEq)]
pub enum ProjectError {
    BadName,
    Unknown,
    StaleParent,
    BadShare(&'static str),
    SharesOff,
    UnknownArtifact,
    /// The project is seen by the caller, who may not do this to it.
    ReadOnly,
    Storage,
}

impl ProjectError {
    pub fn code(&self) -> &'static str {
        match self {
            ProjectError::BadName => "bad_name",
            ProjectError::Unknown => "unknown_project",
            ProjectError::StaleParent => "stale_parent",
            ProjectError::BadShare(_) => "bad_share",
            ProjectError::SharesOff => "shares_off",
            ProjectError::UnknownArtifact => "unknown_artifact",
            ProjectError::ReadOnly => "forbidden",
            ProjectError::Storage => "storage",
        }
    }

    pub fn status(&self) -> u16 {
        match self {
            ProjectError::BadName | ProjectError::BadShare(_) => 422,
            ProjectError::Unknown | ProjectError::UnknownArtifact => 404,
            ProjectError::StaleParent => 409,
            ProjectError::SharesOff => 501,
            ProjectError::ReadOnly => 403,
            ProjectError::Storage => 500,
        }
    }

    pub fn detail(&self) -> String {
        match self {
            ProjectError::BadShare(why) => (*why).to_string(),
            _ => String::new(),
        }
    }
}

pub struct ProjectBook {
    path: PathBuf,
    projects: Vec<ProjectRecord>,
}

impl ProjectBook {
    pub fn open(dir: &Path) -> io::Result<ProjectBook> {
        let path = dir.join("projects.json");
        let projects = store::read_json(&path)?.unwrap_or_default();
        Ok(ProjectBook { path, projects })
    }

    fn save(&self) -> Result<(), ProjectError> {
        store::write_json(&self.path, &self.projects).map_err(|_| ProjectError::Storage)
    }

    pub fn list(&self) -> &[ProjectRecord] {
        &self.projects
    }

    pub fn get(&self, id: &str) -> Option<&ProjectRecord> {
        self.projects.iter().find(|p| p.id == id)
    }

    /// The project, if `who` may see it; for anybody else it is as if there were none.
    pub fn visible(&self, id: &str, who: &Identity) -> Result<&ProjectRecord, ProjectError> {
        self.get(id)
            .filter(|p| p.access_of(who).is_some())
            .ok_or(ProjectError::Unknown)
    }

    pub fn create(&mut self, who: &Identity, name: &str) -> Result<ProjectRecord, ProjectError> {
        if name.trim().is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control)
        {
            return Err(ProjectError::BadName);
        }
        let record = ProjectRecord {
            id: crate::ids::new(crate::ids::Kind::Project),
            name: name.to_string(),
            owner: who.owner(),
            created: now(),
            revisions: Vec::new(),
            shares: Vec::new(),
            deleted: false,
        };
        self.projects.push(record.clone());
        if let Err(e) = self.save() {
            self.projects.pop();
            return Err(e);
        }
        Ok(record)
    }

    /// Adds a revision that follows `parent`; the caller needs `write` access, and the artifact is checked by the caller.
    pub fn add_revision(
        &mut self,
        who: &Identity,
        id: &str,
        artifact_id: &str,
        parent: u64,
    ) -> Result<Revision, ProjectError> {
        let project = self
            .projects
            .iter_mut()
            .find(|p| p.id == id)
            .filter(|p| p.access_of(who).is_some())
            .ok_or(ProjectError::Unknown)?;
        // someone who can only read sees the project and may not write to it
        if project.access_of(who) != Some(Access::Write) {
            return Err(ProjectError::ReadOnly);
        }
        if project.head() != parent {
            return Err(ProjectError::StaleParent);
        }
        let revision = Revision {
            number: parent + 1,
            artifact_id: artifact_id.to_string(),
            by: who.owner(),
            at: now(),
        };
        project.revisions.push(revision.clone());
        if let Err(e) = self.save() {
            if let Some(p) = self.projects.iter_mut().find(|p| p.id == id) {
                p.revisions.pop();
            }
            return Err(e);
        }
        Ok(revision)
    }

    /// Shares the project with `target` (an owner key), or changes the access given. Only the owner and the administrators may.
    pub fn share(
        &mut self,
        who: &Identity,
        id: &str,
        target: &str,
        access: Access,
    ) -> Result<(), ProjectError> {
        let project = self
            .projects
            .iter_mut()
            .find(|p| p.id == id)
            .filter(|p| p.access_of(who).is_some())
            .ok_or(ProjectError::Unknown)?;
        if !(who.role == Role::Admin || project.owner == who.owner()) {
            return Err(ProjectError::ReadOnly);
        }
        if target == project.owner {
            return Err(ProjectError::BadShare("the owner has it already"));
        }
        let before = project.shares.clone();
        match project.shares.iter_mut().find(|s| s.who == target) {
            Some(s) => s.access = access,
            None => project.shares.push(Share {
                who: target.to_string(),
                access,
            }),
        }
        if let Err(e) = self.save() {
            if let Some(p) = self.projects.iter_mut().find(|p| p.id == id) {
                p.shares = before;
            }
            return Err(e);
        }
        Ok(())
    }

    pub fn unshare(&mut self, who: &Identity, id: &str, target: &str) -> Result<(), ProjectError> {
        let project = self
            .projects
            .iter_mut()
            .find(|p| p.id == id)
            .filter(|p| p.access_of(who).is_some())
            .ok_or(ProjectError::Unknown)?;
        if !(who.role == Role::Admin || project.owner == who.owner()) {
            return Err(ProjectError::ReadOnly);
        }
        let before = project.shares.clone();
        project.shares.retain(|s| s.who != target);
        if project.shares.len() == before.len() {
            return Err(ProjectError::Unknown);
        }
        if let Err(e) = self.save() {
            if let Some(p) = self.projects.iter_mut().find(|p| p.id == id) {
                p.shares = before;
            }
            return Err(e);
        }
        Ok(())
    }

    /// Hides the project (the owner and the administrators); its revisions stay until it is purged.
    pub fn soft_delete(&mut self, who: &Identity, id: &str) -> Result<(), ProjectError> {
        let project = self
            .projects
            .iter_mut()
            .find(|p| p.id == id)
            .filter(|p| p.access_of(who).is_some())
            .ok_or(ProjectError::Unknown)?;
        if !(who.role == Role::Admin || project.owner == who.owner()) {
            return Err(ProjectError::ReadOnly);
        }
        project.deleted = true;
        self.save()
    }

    /// Removes a hidden project for good (an administrator).
    pub fn purge(&mut self, id: &str) -> Result<(), ProjectError> {
        let index = self
            .projects
            .iter()
            .position(|p| p.id == id && p.deleted)
            .ok_or(ProjectError::Unknown)?;
        let gone = self.projects.remove(index);
        if let Err(e) = self.save() {
            self.projects.insert(index, gone);
            return Err(e);
        }
        Ok(())
    }

    /// Whether `who` sees the artifact because a project they see lists it in a revision (or says it is the origin of the artifact).
    pub fn shows_artifact(
        &self,
        who: &Identity,
        artifact_id: &str,
        origin_project: Option<&str>,
    ) -> bool {
        self.projects.iter().any(|p| {
            p.access_of(who).is_some()
                && !p.deleted
                && (p.revisions.iter().any(|r| r.artifact_id == artifact_id)
                    || origin_project == Some(p.id.as_str()))
        })
    }

    /// The artifacts the revisions of live projects point to.
    pub fn revision_artifacts(&self) -> Vec<String> {
        self.projects
            .iter()
            .flat_map(|p| p.revisions.iter().map(|r| r.artifact_id.clone()))
            .collect()
    }
}

/// Everything the server keeps about projects.
pub struct Projects {
    pub book: Mutex<ProjectBook>,
    pub shares_enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("eh3d-server-proj-{name}-{}", std::process::id()));
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

    #[test]
    fn a_project_is_the_owners_and_the_administrators_until_it_is_shared() {
        let d = dir("vis");
        let mut book = ProjectBook::open(&d).unwrap();
        let (ana, bob, root) = (
            person("ana", Role::Operator),
            person("bob", Role::Operator),
            person("root", Role::Admin),
        );
        let p = book.create(&ana, "board one").unwrap();
        assert!(book.visible(&p.id, &ana).is_ok());
        assert!(book.visible(&p.id, &root).is_ok());
        assert_eq!(
            book.visible(&p.id, &bob).unwrap_err(),
            ProjectError::Unknown
        );
        book.share(&ana, &p.id, "bob", Access::Read).unwrap();
        assert_eq!(
            book.visible(&p.id, &bob).unwrap().access_of(&bob),
            Some(Access::Read)
        );
        // read access sees but does not write, and does not share on
        assert_eq!(
            book.add_revision(&bob, &p.id, "a-1", 0),
            Err(ProjectError::ReadOnly)
        );
        assert_eq!(
            book.share(&bob, &p.id, "cy", Access::Read),
            Err(ProjectError::ReadOnly)
        );
        book.share(&ana, &p.id, "bob", Access::Write).unwrap();
        assert!(book.add_revision(&bob, &p.id, "a-1", 0).is_ok());
        // the owner cannot be shared with
        assert!(matches!(
            book.share(&ana, &p.id, "ana", Access::Read),
            Err(ProjectError::BadShare(_))
        ));
        book.unshare(&ana, &p.id, "bob").unwrap();
        assert_eq!(
            book.visible(&p.id, &bob).unwrap_err(),
            ProjectError::Unknown
        );
        assert_eq!(book.unshare(&ana, &p.id, "bob"), Err(ProjectError::Unknown));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn two_saves_on_one_parent_one_wins() {
        let d = dir("cas");
        let mut book = ProjectBook::open(&d).unwrap();
        let ana = person("ana", Role::Operator);
        let p = book.create(&ana, "x").unwrap();
        assert_eq!(book.add_revision(&ana, &p.id, "a-1", 0).unwrap().number, 1);
        // both saved from revision 1
        assert_eq!(book.add_revision(&ana, &p.id, "a-2", 1).unwrap().number, 2);
        assert_eq!(
            book.add_revision(&ana, &p.id, "a-3", 1),
            Err(ProjectError::StaleParent)
        );
        // a parent from the future, or the start again
        assert_eq!(
            book.add_revision(&ana, &p.id, "a-3", 5),
            Err(ProjectError::StaleParent)
        );
        assert_eq!(
            book.add_revision(&ana, &p.id, "a-3", 0),
            Err(ProjectError::StaleParent)
        );
        assert_eq!(book.get(&p.id).unwrap().head(), 2);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_deleted_project_is_hidden_and_purged_only_after() {
        let d = dir("del");
        let mut book = ProjectBook::open(&d).unwrap();
        let (ana, root) = (person("ana", Role::Operator), person("root", Role::Admin));
        let p = book.create(&ana, "x").unwrap();
        assert_eq!(
            book.purge(&p.id),
            Err(ProjectError::Unknown),
            "only a deleted one is purged"
        );
        book.soft_delete(&ana, &p.id).unwrap();
        assert!(book.visible(&p.id, &ana).is_err());
        assert!(
            book.visible(&p.id, &root).is_ok(),
            "the administrator can still see it"
        );
        assert!(!book.shows_artifact(&ana, "a-1", Some(&p.id)));
        book.purge(&p.id).unwrap();
        assert!(book.get(&p.id).is_none());
        // the file keeps what was written
        let again = ProjectBook::open(&d).unwrap();
        assert!(again.list().is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_artifacts_of_a_visible_project_are_shown() {
        let d = dir("show");
        let mut book = ProjectBook::open(&d).unwrap();
        let (ana, bob) = (person("ana", Role::Operator), person("bob", Role::Operator));
        let p = book.create(&ana, "x").unwrap();
        book.add_revision(&ana, &p.id, "a-1", 0).unwrap();
        assert!(!book.shows_artifact(&bob, "a-1", None));
        book.share(&ana, &p.id, "bob", Access::Read).unwrap();
        assert!(book.shows_artifact(&bob, "a-1", None));
        assert!(!book.shows_artifact(&bob, "a-9", None));
        assert!(
            book.shows_artifact(&bob, "a-9", Some(&p.id)),
            "made for the project"
        );
        assert_eq!(book.revision_artifacts(), ["a-1"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn names_are_checked_and_a_token_is_a_different_owner_from_a_user() {
        let d = dir("name");
        let mut book = ProjectBook::open(&d).unwrap();
        let ana = person("ana", Role::Operator);
        for bad in ["", "   ", "x\u{7}y", &"n".repeat(81)] {
            assert_eq!(book.create(&ana, bad).unwrap_err(), ProjectError::BadName);
        }
        let token = Identity {
            name: "ana".into(),
            role: Role::Operator,
            is_token: true,
            token_id: Some("t1".into()),
            scopes: None,
            template: None,
        };
        let p = book.create(&ana, "mine").unwrap();
        assert!(book.visible(&p.id, &token).is_err());
        book.share(&ana, &p.id, "token:t1", Access::Read).unwrap();
        assert!(book.visible(&p.id, &token).is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }
}
