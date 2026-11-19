//! The `laser` plugin: laser machines that the server runs in simulation until a real machine is connected behind the same operations. The work is in `src/laser.rs`
//! (the reading of a G-code job against the limits of the machine, the emergency stop and its latch, the simulated run); this file hands it the machines that were made
//! with `POST /api/v1/work/machines` for the plugin `laser`.

use std::sync::Arc;

use serde_json::{json, Map, Value};

use crate::laser::{LaserConfig, LaserEngine, MAX_LINES, SETTINGS_HELP};
use crate::plugins::{
    work_machine_ids, work_machine_settings, MachineInfo, MachinePlugin, MachineRoute, PluginError,
    PluginResult, PluginStatus, Prepared,
};
use crate::state::AppState;

/// The routes the plugin adds under `/api/v1/work/machines/{id}/`.
const ROUTES: &[MachineRoute] = &[
    MachineRoute::new("POST", "laser/test-fire", "operator", "jobs:start"),
    MachineRoute::new("POST", "laser/release", "admin", "machines:manage"),
];

/// The laser machines of the server.
#[derive(Default)]
pub struct LaserPlugin {
    engine: LaserEngine,
}

impl LaserPlugin {
    fn config(st: &Arc<AppState>, machine: &str) -> PluginResult<LaserConfig> {
        let settings =
            work_machine_settings(st, machine, "laser").ok_or(PluginError::UnknownMachine)?;
        LaserConfig::from_settings(&settings).map_err(PluginError::Bad)
    }
}

impl MachinePlugin for LaserPlugin {
    fn kind(&self) -> &'static str {
        "laser"
    }

    fn title(&self) -> &'static str {
        "Laser machines: G-code jobs held to the limits of power, feed and bed, with an emergency stop that latches (simulation)"
    }

    fn capabilities(&self) -> Value {
        let mut settings = Map::new();
        for (key, help) in SETTINGS_HELP {
            settings.insert((*key).to_string(), json!(help));
        }
        json!({
            "requests": ["gcode", "vector", "test_fire"],
            "pause": "all",
            "confirm": false,
            "stop": true,
            "stop_is": "emergency stop: the job ends at once and the machine is latched until an administrator releases it (POST .../laser/release)",
            "progress": "lines",
            "settings": settings,
            "limits": { "lines": MAX_LINES },
            "evidence": "simulator",
        })
    }

    fn routes(&self) -> &'static [MachineRoute] {
        ROUTES
    }

    fn manages_machines(&self) -> bool {
        true
    }

    fn check_settings(&self, settings: &Value) -> PluginResult<()> {
        LaserConfig::from_settings(settings)
            .map(|_| ())
            .map_err(PluginError::Bad)
    }

    fn machines(&self, st: &Arc<AppState>) -> Vec<MachineInfo> {
        work_machine_ids(st, "laser")
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
        st: &Arc<AppState>,
        machine: &str,
        native_id: &str,
        _by: &str,
    ) -> PluginResult<()> {
        let cfg = Self::config(st, machine)?;
        self.engine.check_start(native_id, &cfg)?;
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
        st: &Arc<AppState>,
        machine: &str,
        native_id: &str,
    ) -> PluginResult<PluginStatus> {
        if let Ok(cfg) = Self::config(st, machine) {
            self.engine.door_opened(native_id, &cfg);
        }
        self.engine.status(native_id)
    }

    fn discard(&self, _st: &Arc<AppState>, _machine: &str, native_id: &str, _by: &str) {
        self.engine.discard(native_id);
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
            "laser/test-fire" => self.engine.test_fire(machine, &cfg, body),
            "laser/release" => Ok(self.engine.release(machine)),
            _ => Err(PluginError::NotSupported(
                "the machine has no such operation",
            )),
        }
    }
}
