//! The machines the server knows: for each one how to reach it (a node over the network, or a simulator), the constants of its
//! configuration files (`DRILLPRO.DCF` and `ROUTEPRO.RCF`, as text) and the values tuned on the real machine (a text in the format of the
//! programs' machine file). The planning of a job takes its numbers from here, so the machine's calibration lives in one place for every
//! client that sends jobs.

use std::io;
use std::path::{Path, PathBuf};

use eh3d_drillpro::protocol::connection::{self, ConnectionConfig, Kind};
use serde::{Deserialize, Serialize};

use crate::store;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Connection {
    /// No machine: the job runs on the built-in simulator.
    Demo,
    /// An EH3D-NODE-BUNGARD on the network.
    Node { address: String, token: String },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct MachineRecord {
    pub id: String,
    pub name: String,
    /// What it is, in free text (for example `ccd-mtc`).
    #[serde(default)]
    pub model: String,
    /// The station (the workshop, the room or the bench) the machine belongs to, as a short name like an id; empty when it has none.
    #[serde(default)]
    pub station: String,
    pub connection: Connection,
    /// The text of `DRILLPRO.DCF`: the machine constants of the drilling program.
    #[serde(default)]
    pub dcf: String,
    /// The text of `ROUTEPRO.RCF`.
    #[serde(default)]
    pub rcf: String,
    /// The values tuned on the real machine, in the format of the programs' machine file (`x_search_hz = 2200` ...). The connection keys in it are ignored.
    #[serde(default)]
    pub cfg: String,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum MachineError {
    BadId,
    BadName,
    /// The name of a station is written like an id.
    BadStation,
    Exists,
    Unknown,
    /// A node needs its address.
    BadAddress,
    /// The tuning text does not parse (a typo is an error, never a silent default).
    BadCfg,
    BadDcf,
    Storage,
}

impl MachineError {
    pub fn code(self) -> &'static str {
        match self {
            MachineError::BadId => "bad_id",
            MachineError::BadName => "bad_name",
            MachineError::BadStation => "bad_station",
            MachineError::Exists => "machine_exists",
            MachineError::Unknown => "unknown_machine",
            MachineError::BadAddress => "bad_address",
            MachineError::BadCfg => "bad_cfg",
            MachineError::BadDcf => "bad_dcf",
            MachineError::Storage => "storage",
        }
    }
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

impl MachineRecord {
    /// The connection of the programs' library for this machine: the tuning text, with the way to reach it from here.
    pub fn connection_config(&self) -> Result<ConnectionConfig, MachineError> {
        let mut c = connection::parse(&self.cfg).map_err(|_| MachineError::BadCfg)?;
        match &self.connection {
            Connection::Demo => {
                c.kind = Kind::Demo;
                // the simulator never raises the control unit's home line: searches are sent in full, as the programs' own simulator does
                c.home_signal = false;
            }
            Connection::Node { address, token } => {
                c.kind = Kind::Node;
                c.node_address = address.clone();
                c.node_token = token.clone();
            }
        }
        Ok(c)
    }

    pub fn validate(&self) -> Result<(), MachineError> {
        if !valid_id(&self.id) {
            return Err(MachineError::BadId);
        }
        if self.name.trim().is_empty() || self.name.len() > 64 {
            return Err(MachineError::BadName);
        }
        if !self.station.is_empty() && !valid_id(&self.station) {
            return Err(MachineError::BadStation);
        }
        if let Connection::Node { address, .. } = &self.connection {
            if address.trim().is_empty() || !address.contains(':') {
                return Err(MachineError::BadAddress);
            }
        }
        self.connection_config()?;
        for text in [&self.dcf, &self.rcf] {
            if !text.trim().is_empty()
                && eh3d_drillpro::config::parse(text)
                    .and_then(|c| c.settings())
                    .is_none()
            {
                return Err(MachineError::BadDcf);
            }
        }
        Ok(())
    }

    /// What the API shows: everything but the node's token, with a flag that says whether there is one.
    pub fn public(&self) -> serde_json::Value {
        let (kind, address, token_set) = match &self.connection {
            Connection::Demo => ("demo", String::new(), false),
            Connection::Node { address, token } => ("node", address.clone(), !token.is_empty()),
        };
        serde_json::json!({
            "id": self.id, "name": self.name, "model": self.model, "station": self.station,
            "connection": { "kind": kind, "address": address, "token_set": token_set },
            "dcf": self.dcf, "rcf": self.rcf, "cfg": self.cfg,
        })
    }
}

pub struct Machines {
    path: PathBuf,
    list: Vec<MachineRecord>,
}

impl Machines {
    pub fn open(dir: &Path) -> io::Result<Machines> {
        let path = dir.join("machines.json");
        let list: Vec<MachineRecord> = store::read_json(&path)?.unwrap_or_default();
        Ok(Machines { path, list })
    }

    fn save(&self) -> Result<(), MachineError> {
        store::write_json(&self.path, &self.list).map_err(|_| MachineError::Storage)
    }

    pub fn list(&self) -> &[MachineRecord] {
        &self.list
    }

    pub fn get(&self, id: &str) -> Option<&MachineRecord> {
        self.list.iter().find(|m| m.id == id)
    }

    pub fn add(&mut self, record: MachineRecord) -> Result<(), MachineError> {
        record.validate()?;
        if self.get(&record.id).is_some() {
            return Err(MachineError::Exists);
        }
        self.list.push(record);
        self.save()
    }

    /// Replaces a machine. A node's token that is left empty in `record` stays as it was, so the panel never has to know it.
    pub fn replace(&mut self, mut record: MachineRecord) -> Result<(), MachineError> {
        let index = self
            .list
            .iter()
            .position(|m| m.id == record.id)
            .ok_or(MachineError::Unknown)?;
        if let (Connection::Node { address, token }, Connection::Node { token: old, .. }) =
            (&record.connection, &self.list[index].connection)
        {
            if token.is_empty() {
                record.connection = Connection::Node {
                    address: address.clone(),
                    token: old.clone(),
                };
            }
        }
        record.validate()?;
        self.list[index] = record;
        self.save()
    }

    pub fn remove(&mut self, id: &str) -> Result<(), MachineError> {
        let index = self
            .list
            .iter()
            .position(|m| m.id == id)
            .ok_or(MachineError::Unknown)?;
        self.list.remove(index);
        self.save()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo(id: &str) -> MachineRecord {
        MachineRecord {
            id: id.into(),
            name: "Bungard".into(),
            model: "ccd-mtc".into(),
            station: String::new(),
            connection: Connection::Demo,
            dcf: String::new(),
            rcf: String::new(),
            cfg: String::new(),
        }
    }

    fn dir(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("eh3d-server-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn ids_are_short_lowercase_words() {
        assert!(
            valid_id("bungard-1")
                && !valid_id("")
                && !valid_id("Bungard")
                && !valid_id("a b")
                && !valid_id(&"x".repeat(33))
        );
    }

    #[test]
    fn a_machine_is_added_replaced_and_removed_and_kept_on_disk() {
        let d = dir("machines-a");
        let mut m = Machines::open(&d).unwrap();
        m.add(demo("one")).unwrap();
        assert_eq!(m.add(demo("one")), Err(MachineError::Exists));
        assert_eq!(m.add(demo("Bad Id")), Err(MachineError::BadId));
        let mut changed = demo("one");
        changed.name = "Another".into();
        m.replace(changed).unwrap();
        assert_eq!(
            Machines::open(&d).unwrap().get("one").unwrap().name,
            "Another"
        );
        assert_eq!(m.replace(demo("two")), Err(MachineError::Unknown));
        m.remove("one").unwrap();
        assert!(m.list().is_empty());
        assert_eq!(m.remove("one"), Err(MachineError::Unknown));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_node_needs_an_address_and_its_token_is_never_shown_nor_lost_on_an_update() {
        let d = dir("machines-b");
        let mut m = Machines::open(&d).unwrap();
        let mut node = demo("node");
        node.connection = Connection::Node {
            address: String::new(),
            token: "secret-token".into(),
        };
        assert_eq!(m.add(node.clone()), Err(MachineError::BadAddress));
        node.connection = Connection::Node {
            address: "192.168.0.50:5800".into(),
            token: "secret-token".into(),
        };
        m.add(node.clone()).unwrap();
        let shown = m.get("node").unwrap().public().to_string();
        assert!(!shown.contains("secret-token") && shown.contains("\"token_set\":true"));
        // an update that leaves the token empty keeps the one stored
        node.connection = Connection::Node {
            address: "192.168.0.51:5800".into(),
            token: String::new(),
        };
        m.replace(node).unwrap();
        match &m.get("node").unwrap().connection {
            Connection::Node { address, token } => assert_eq!(
                (address.as_str(), token.as_str()),
                ("192.168.0.51:5800", "secret-token")
            ),
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_tuning_text_is_checked_and_becomes_the_connection_of_the_library() {
        let mut r = demo("tuned");
        r.cfg = "y_search_hz = 2000\nunstick_mm = 8\nspindle = node\n".into();
        r.validate().unwrap();
        let c = r.connection_config().unwrap();
        assert_eq!(
            (
                c.tuning.y_search_hz,
                c.tuning.unstick_mm,
                c.tuning.spindle_node
            ),
            (2000.0, 8.0, true)
        );
        assert_eq!(c.kind, Kind::Demo);
        r.cfg = "y_serch_hz = 2000\n".into();
        assert_eq!(r.validate(), Err(MachineError::BadCfg));
        // whatever the text says about the connection, the record decides
        r.cfg = "connection = usb\nusb_port = COM9\n".into();
        assert_eq!(r.connection_config().unwrap().kind, Kind::Demo);
        r.connection = Connection::Node {
            address: "10.0.0.2:5800".into(),
            token: "t".into(),
        };
        let c = r.connection_config().unwrap();
        assert_eq!(
            (c.kind, c.node_address.as_str(), c.node_token.as_str()),
            (Kind::Node, "10.0.0.2:5800", "t")
        );
    }

    #[test]
    fn a_configuration_file_that_is_not_one_is_refused() {
        let mut r = demo("dcf");
        r.dcf = "this is not a DCF".into();
        assert_eq!(r.validate(), Err(MachineError::BadDcf));
    }
}
