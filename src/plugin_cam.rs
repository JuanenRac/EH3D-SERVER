//! The `cam` plugin: workers that make G-code (drilling, the outline of a board, the isolation of tracks) and check a program that somebody else made. They move nothing:
//! the result is in the `view` of the job. The work is in `src/cam.rs`; this file hands it the workers that were made with `POST /api/v1/work/machines` for the plugin `cam`.

use std::sync::Arc;

use serde_json::{json, Map, Value};

use crate::cam::{CamConfig, CamEngine, SETTINGS_HELP, TOOLS};
use crate::plugins::{
    work_machine_ids, work_machine_settings, MachineInfo, MachinePlugin, MachineRoute, PluginError,
    PluginResult, PluginStatus, Prepared,
};
use crate::state::AppState;

/// The routes the plugin adds under `/api/v1/work/machines/{id}/`.
const ROUTES: &[MachineRoute] = &[MachineRoute::new("GET", "tools", "viewer", "machines:view")];

/// The workers of the kind `cam`.
#[derive(Default)]
pub struct CamPlugin {
    engine: CamEngine,
}

impl CamPlugin {
    fn config(st: &Arc<AppState>, machine: &str) -> PluginResult<CamConfig> {
        let settings =
            work_machine_settings(st, machine, "cam").ok_or(PluginError::UnknownMachine)?;
        CamConfig::from_settings(&settings).map_err(PluginError::Bad)
    }
}

impl MachinePlugin for CamPlugin {
    fn kind(&self) -> &'static str {
        "cam"
    }

    fn title(&self) -> &'static str {
        "CAM workers: G-code for drilling, outlines, isolation, stencils and probing, and the reports of a G-code program; nothing moves"
    }

    fn capabilities(&self) -> Value {
        let mut settings = Map::new();
        for (key, help) in SETTINGS_HELP {
            settings.insert((*key).to_string(), json!(help));
        }
        json!({
            "requests": ["drill", "outline", "isolate", "stencil", "autolevel", "inspect", "verify"],
            "pause": "none",
            "confirm": false,
            "stop": true,
            "stop_is": "the job is cancelled; nothing moves",
            "progress": "steps",
            "settings": settings,
            "moves": false,
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
        CamConfig::from_settings(settings)
            .map(|_| ())
            .map_err(PluginError::Bad)
    }

    fn machines(&self, st: &Arc<AppState>) -> Vec<MachineInfo> {
        work_machine_ids(st, "cam")
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
            "a CAM job cannot be held; it can be cancelled",
        ))
    }

    fn resume(&self, _st: &Arc<AppState>, _machine: &str, _native_id: &str) -> PluginResult<()> {
        Err(PluginError::NotSupported(
            "a CAM job cannot be held; it can be cancelled",
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

    fn operation(
        &self,
        st: &Arc<AppState>,
        machine: &str,
        route: &str,
        _body: &Value,
        _by: &str,
    ) -> PluginResult<Value> {
        Self::config(st, machine)?;
        match route {
            "tools" => {
                let tools: Vec<Value> = TOOLS
                    .iter()
                    .map(|(kind, what)| json!({"kind": kind, "what": what, "available": true, "version": "built in"}))
                    .collect();
                Ok(json!({ "tools": tools }))
            }
            _ => Err(PluginError::NotSupported(
                "the machine has no such operation",
            )),
        }
    }
}
