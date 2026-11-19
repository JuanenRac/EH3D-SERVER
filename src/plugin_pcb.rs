//! The `pcb` plugin: board stations that make reports and checks and move nothing. The work is in `src/pcb.rs` (the checks of the rules of a board, the reports of drilling,
//! of the list of parts and of an inspection, and the simulated run); this file hands it the stations that were made with `POST /api/v1/work/machines` for the plugin `pcb`.

use std::sync::Arc;

use serde_json::{json, Map, Value};

use crate::pcb::{PcbConfig, PcbEngine, SETTINGS_HELP};
use crate::plugins::{
    work_machine_ids, work_machine_settings, MachineInfo, MachinePlugin, PluginError, PluginResult,
    PluginStatus, Prepared,
};
use crate::state::AppState;

/// The board stations of the server.
#[derive(Default)]
pub struct PcbPlugin {
    engine: PcbEngine,
}

impl PcbPlugin {
    fn config(st: &Arc<AppState>, machine: &str) -> PluginResult<PcbConfig> {
        let settings =
            work_machine_settings(st, machine, "pcb").ok_or(PluginError::UnknownMachine)?;
        PcbConfig::from_settings(&settings).map_err(PluginError::Bad)
    }
}

impl MachinePlugin for PcbPlugin {
    fn kind(&self) -> &'static str {
        "pcb"
    }

    fn title(&self) -> &'static str {
        "Board stations: checks of the rules of a board, drilling, parts list and inspection reports, with no movement (simulation)"
    }

    fn capabilities(&self) -> Value {
        let mut settings = Map::new();
        for (key, help) in SETTINGS_HELP {
            settings.insert((*key).to_string(), json!(help));
        }
        json!({
            "requests": ["rules_check", "drill_report", "bom_report", "inspect"],
            "pause": "all",
            "confirm": false,
            "stop": true,
            "progress": "items",
            "moves": false,
            "settings": settings,
            "evidence": "simulator",
        })
    }

    fn manages_machines(&self) -> bool {
        true
    }

    fn check_settings(&self, settings: &Value) -> PluginResult<()> {
        PcbConfig::from_settings(settings)
            .map(|_| ())
            .map_err(PluginError::Bad)
    }

    fn machines(&self, st: &Arc<AppState>) -> Vec<MachineInfo> {
        work_machine_ids(st, "pcb")
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
