//! The `pnp` plugin: pick-and-place machines that the server runs in simulation until a real machine is connected behind the same operations. The work is in
//! `src/pnp.rs` (the feeders, the placement jobs, the head and the simulated run); this file hands it the machines that were made with `POST /api/v1/work/machines` for the
//! plugin `pnp`.

use std::sync::Arc;

use serde_json::{json, Map, Value};

use crate::plugins::{
    work_machine_ids, work_machine_settings, MachineInfo, MachinePlugin, MachineRoute, PluginError,
    PluginResult, PluginStatus, Prepared,
};
use crate::pnp::{PnpConfig, PnpEngine, MAX_COUNT, MAX_SLOTS, SETTINGS_HELP};
use crate::state::AppState;

/// The routes the plugin adds under `/api/v1/work/machines/{id}/`.
const ROUTES: &[MachineRoute] = &[
    MachineRoute::new("GET", "feeders", "viewer", "machines:view"),
    MachineRoute::new("PUT", "feeders", "operator", "jobs:prepare"),
];

/// The pick-and-place machines of the server.
#[derive(Default)]
pub struct PnpPlugin {
    engine: PnpEngine,
}

impl PnpPlugin {
    fn config(st: &Arc<AppState>, machine: &str) -> PluginResult<PnpConfig> {
        let settings =
            work_machine_settings(st, machine, "pnp").ok_or(PluginError::UnknownMachine)?;
        PnpConfig::from_settings(&settings).map_err(PluginError::Bad)
    }
}

impl MachinePlugin for PnpPlugin {
    fn kind(&self) -> &'static str {
        "pnp"
    }

    fn title(&self) -> &'static str {
        "Pick-and-place machines: feeders, a queue of placement jobs and the state of the head (simulation)"
    }

    fn capabilities(&self) -> Value {
        let mut settings = Map::new();
        for (key, help) in SETTINGS_HELP {
            settings.insert((*key).to_string(), json!(help));
        }
        json!({
            "requests": ["place", "load_feeders", "calibrate"],
            "pause": "all",
            "confirm": false,
            "stop": true,
            "progress": "placements",
            "settings": settings,
            "limits": { "slots": MAX_SLOTS, "count": MAX_COUNT },
            "evidence": "simulator",
        })
    }

    fn manages_machines(&self) -> bool {
        true
    }

    fn routes(&self) -> &'static [MachineRoute] {
        ROUTES
    }

    fn operation(
        &self,
        st: &Arc<AppState>,
        machine: &str,
        route: &str,
        body: &Value,
        _by: &str,
    ) -> PluginResult<Value> {
        let cfg = Self::config(st, machine)?;
        match route {
            "feeders" if body.is_null() => Ok(self.engine.feeders_view(machine, &cfg)),
            "feeders" => self.engine.set_feeder(machine, &cfg, body),
            _ => Err(PluginError::NotSupported(
                "the machine has no such operation",
            )),
        }
    }

    fn check_settings(&self, settings: &Value) -> PluginResult<()> {
        PnpConfig::from_settings(settings)
            .map(|_| ())
            .map_err(PluginError::Bad)
    }

    fn machines(&self, st: &Arc<AppState>) -> Vec<MachineInfo> {
        work_machine_ids(st, "pnp")
            .into_iter()
            .map(|(id, name)| {
                let state = if self.engine.busy(&id) {
                    "busy"
                } else {
                    "idle"
                };
                MachineInfo { id, name, state }
            })
            .collect()
    }

    fn detail(&self, st: &Arc<AppState>, machine: &str) -> Value {
        match Self::config(st, machine) {
            Ok(cfg) => self.engine.detail(machine, &cfg),
            Err(_) => Value::Null,
        }
    }

    fn prepare(
        &self,
        st: &Arc<AppState>,
        machine: &str,
        request: &Value,
        _by: &str,
    ) -> PluginResult<Prepared> {
        let cfg = Self::config(st, machine)?;
        self.engine.prepare(machine, &cfg, request)
    }

    fn start(
        &self,
        _st: &Arc<AppState>,
        machine: &str,
        native_id: &str,
        _by: &str,
    ) -> PluginResult<()> {
        self.engine.start(machine, native_id)
    }

    fn pause(&self, _st: &Arc<AppState>, _machine: &str, native_id: &str) -> PluginResult<()> {
        self.engine.pause(native_id)
    }

    fn resume(&self, _st: &Arc<AppState>, _machine: &str, native_id: &str) -> PluginResult<()> {
        self.engine.resume(native_id)
    }

    fn stop(
        &self,
        _st: &Arc<AppState>,
        _machine: &str,
        native_id: &str,
        _by: &str,
    ) -> PluginResult<()> {
        self.engine.stop(native_id)
    }

    fn status(
        &self,
        _st: &Arc<AppState>,
        _machine: &str,
        native_id: &str,
    ) -> PluginResult<PluginStatus> {
        self.engine.status(native_id)
    }

    fn discard(&self, _st: &Arc<AppState>, _machine: &str, native_id: &str, _by: &str) {
        self.engine.discard(native_id);
    }
}
