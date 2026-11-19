//! The `gerber` plugin: workers that read Gerber and drill files, list the layers and check a board against a basic set of rules. They move nothing: a job is a report.
//! The work is in `src/gerber.rs`; this file hands it the workers that were made with `POST /api/v1/work/machines` for the plugin `gerber`.

use std::sync::Arc;

use serde_json::{json, Map, Value};

use crate::gerber::{GerberConfig, GerberEngine, MAX_FILES, SETTINGS_HELP};
use crate::plugins::{
    work_machine_ids, work_machine_settings, MachineInfo, MachinePlugin, PluginError, PluginResult,
    PluginStatus, Prepared,
};
use crate::state::AppState;

/// The workers of the kind `gerber`.
#[derive(Default)]
pub struct GerberPlugin {
    engine: GerberEngine,
}

impl GerberPlugin {
    fn config(st: &Arc<AppState>, machine: &str) -> PluginResult<GerberConfig> {
        let settings =
            work_machine_settings(st, machine, "gerber").ok_or(PluginError::UnknownMachine)?;
        GerberConfig::from_settings(&settings).map_err(PluginError::Bad)
    }
}

impl MachinePlugin for GerberPlugin {
    fn kind(&self) -> &'static str {
        "gerber"
    }

    fn title(&self) -> &'static str {
        "Gerber workers: read Gerber and drill files, list the layers and check a board against basic rules; nothing moves"
    }

    fn capabilities(&self) -> Value {
        let mut settings = Map::new();
        for (key, help) in SETTINGS_HELP {
            settings.insert((*key).to_string(), json!(help));
        }
        json!({
            "requests": ["load", "drc", "report"],
            "pause": "none",
            "confirm": false,
            "stop": true,
            "stop_is": "the job is cancelled; nothing moves",
            "progress": "checks",
            "settings": settings,
            "limits": { "files": MAX_FILES },
            "moves": false,
            "evidence": "simulator",
        })
    }

    fn manages_machines(&self) -> bool {
        true
    }

    fn check_settings(&self, settings: &Value) -> PluginResult<()> {
        GerberConfig::from_settings(settings)
            .map(|_| ())
            .map_err(PluginError::Bad)
    }

    fn machines(&self, st: &Arc<AppState>) -> Vec<MachineInfo> {
        work_machine_ids(st, "gerber")
            .into_iter()
            .map(|(id, name)| {
                let state = if self.engine.book().busy(&id) {
                    "busy"
                } else {
                    "idle"
                };
                MachineInfo { id, name, state }
            })
            .collect()
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
        self.engine.book().start(machine, native_id)
    }

    fn pause(&self, _st: &Arc<AppState>, _machine: &str, _native_id: &str) -> PluginResult<()> {
        Err(PluginError::NotSupported(
            "a gerber job cannot be held; it can be cancelled",
        ))
    }

    fn resume(&self, _st: &Arc<AppState>, _machine: &str, _native_id: &str) -> PluginResult<()> {
        Err(PluginError::NotSupported(
            "a gerber job cannot be held; it can be cancelled",
        ))
    }

    fn stop(
        &self,
        _st: &Arc<AppState>,
        _machine: &str,
        native_id: &str,
        _by: &str,
    ) -> PluginResult<()> {
        self.engine.book().stop(native_id, "cancelled").map(|_| ())
    }

    fn status(
        &self,
        _st: &Arc<AppState>,
        _machine: &str,
        native_id: &str,
    ) -> PluginResult<PluginStatus> {
        self.engine.book().status(native_id)
    }

    fn discard(&self, _st: &Arc<AppState>, _machine: &str, native_id: &str, _by: &str) {
        self.engine.book().discard(native_id);
    }
}
