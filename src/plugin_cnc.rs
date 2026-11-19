//! The `cnc` plugin: routers, mills and engravers driven by G-code, run in simulation until a real machine is connected behind the same operations. The work is in
//! `src/cnc.rs` (the reading of a program against the travel of the machine, the jog, the emergency stop and its latch, the simulated run); this file hands it the machines
//! that were made with `POST /api/v1/work/machines` for the plugin `cnc`.

use std::sync::Arc;

use serde_json::{json, Map, Value};

use crate::cnc::{CncConfig, CncEngine, MAX_LINES, SETTINGS_HELP};
use crate::plugins::{
    work_machine_ids, work_machine_settings, MachineInfo, MachinePlugin, MachineRoute, PluginError,
    PluginResult, PluginStatus, Prepared,
};
use crate::state::AppState;

/// The routes the plugin adds under `/api/v1/work/machines/{id}/`.
const ROUTES: &[MachineRoute] = &[
    MachineRoute::new("GET", "position", "viewer", "machines:view"),
    MachineRoute::new("POST", "jog", "operator", "jobs:start"),
    MachineRoute::new("POST", "emergency-stop", "operator", "jobs:cancel"),
    MachineRoute::new("POST", "release", "admin", "machines:manage"),
];

/// The machines of the kind `cnc`.
#[derive(Default)]
pub struct CncPlugin {
    engine: CncEngine,
}

impl CncPlugin {
    fn config(st: &Arc<AppState>, machine: &str) -> PluginResult<CncConfig> {
        let settings =
            work_machine_settings(st, machine, "cnc").ok_or(PluginError::UnknownMachine)?;
        CncConfig::from_settings(&settings).map_err(PluginError::Bad)
    }
}

impl MachinePlugin for CncPlugin {
    fn kind(&self) -> &'static str {
        "cnc"
    }

    fn title(&self) -> &'static str {
        "CNC machines: G-code jobs held to the travel and the spindle of the machine, a jog with limits and an emergency stop that latches (simulation)"
    }

    fn capabilities(&self) -> Value {
        let mut settings = Map::new();
        for (key, help) in SETTINGS_HELP {
            settings.insert((*key).to_string(), json!(help));
        }
        json!({
            "requests": ["gcode", "file"],
            "pause": "all",
            "confirm": false,
            "stop": true,
            "stop_is": "feed hold and reset: the job ends; the emergency stop of the machine (POST .../emergency-stop) also latches it until an administrator releases it",
            "progress": "lines",
            "settings": settings,
            "limits": { "lines": MAX_LINES },
            "connections": ["demo"],
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
        CncConfig::from_settings(settings)
            .map(|_| ())
            .map_err(PluginError::Bad)
    }

    fn machines(&self, st: &Arc<AppState>) -> Vec<MachineInfo> {
        work_machine_ids(st, "cnc")
            .into_iter()
            .map(|(id, name)| {
                let state = if self.engine.busy(&id) || self.engine.latched(&id) {
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
            "position" => Ok(self.engine.position(machine)),
            "jog" => self.engine.jog(machine, &cfg, body),
            "emergency-stop" => Ok(self.engine.emergency_stop(machine, &cfg)),
            "release" => Ok(self.engine.release(machine)),
            _ => Err(PluginError::NotSupported(
                "the machine has no such operation",
            )),
        }
    }
}
