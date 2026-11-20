//! The `printer3d` plugin: 3D printers behind the queue of the server, each with the limits of temperature of its model, a print that a person starts, an emergency stop that
//! latches and, optionally, answers shaped like Moonraker's or OctoPrint's. The work is in `src/printer3d.rs`; the printers themselves (the simulator, Klipper through
//! Moonraker, OctoPrint) are in `src/printer_drivers.rs`. This file hands the engine the machines that were made with `POST /api/v1/work/machines` for the plugin `printer3d`.

use std::sync::Arc;

use serde_json::{json, Map, Value};

use crate::plugins::{
    work_machine_ids, work_machine_settings, MachineInfo, MachinePlugin, MachineRoute, PluginError,
    PluginResult, PluginStatus, Prepared,
};
use crate::printer3d::{
    BambuLanPolicy, Printer3dEngine, PrinterConfig, MAX_COMMANDS, PROFILES, SETTINGS_HELP,
};
use crate::state::AppState;

/// The routes the plugin adds under `/api/v1/work/machines/{id}/`.
const ROUTES: &[MachineRoute] = &[
    MachineRoute::new("GET", "temperatures", "viewer", "machines:view"),
    MachineRoute::new("PUT", "temperatures", "operator", "jobs:start"),
    MachineRoute::new("GET", "layer", "viewer", "machines:view"),
    MachineRoute::new("POST", "emergency-stop", "operator", "jobs:cancel"),
    MachineRoute::new("POST", "release", "admin", "machines:manage"),
    MachineRoute::new("GET", "compat/moonraker", "viewer", "machines:view"),
    MachineRoute::new("GET", "compat/octoprint", "viewer", "machines:view"),
];

/// The machines of the kind `printer3d`.
#[derive(Default)]
pub struct Printer3dPlugin {
    engine: Printer3dEngine,
}

impl Printer3dPlugin {
    fn config(st: &Arc<AppState>, machine: &str) -> PluginResult<PrinterConfig> {
        let settings =
            work_machine_settings(st, machine, "printer3d").ok_or(PluginError::UnknownMachine)?;
        let mut cfg = PrinterConfig::from_settings(&settings).map_err(PluginError::Bad)?;
        // what the server allows for a real Bambu Lab printer: the switch (changed while the server runs) and the numbers of server.cfg
        cfg.bambu_lan = BambuLanPolicy {
            enabled: st.policy.get().bambu_lan_enabled,
            port: st.cfg.bambu_lan_port,
            timeout_s: st.cfg.bambu_lan_timeout_s,
        };
        Ok(cfg)
    }
}

impl MachinePlugin for Printer3dPlugin {
    fn kind(&self) -> &'static str {
        "printer3d"
    }

    fn title(&self) -> &'static str {
        "3D printers: prints that a person starts, limits of temperature by model, watched heaters and an emergency stop that latches (simulator, Moonraker, OctoPrint)"
    }

    fn capabilities(&self) -> Value {
        let mut settings = Map::new();
        for (key, help) in SETTINGS_HELP {
            settings.insert((*key).to_string(), json!(help));
        }
        let models: Vec<Value> = PROFILES
            .iter()
            .map(|p| json!({"model": p.0, "hotend_max_c": p.1, "bed_max_c": p.2, "chamber_max_c": p.3, "what": p.4}))
            .collect();
        json!({
            "requests": ["print", "preheat", "gcode"],
            "pause": "print",
            "confirm": true,
            "confirm_is": "a person starts every job; the queue only starts by itself when the machine has auto_start_queue",
            "stop": true,
            "stop_is": "the print is cancelled and the heaters go off; the emergency stop (POST .../emergency-stop) also latches the machine until an administrator releases it",
            "progress": "percent",
            "settings": settings,
            "models": models,
            "connections": ["demo", "moonraker", "octoprint"],
            "limits": { "commands": MAX_COMMANDS },
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
        PrinterConfig::from_settings(settings)
            .map(|_| ())
            .map_err(PluginError::Bad)
    }

    fn machines(&self, st: &Arc<AppState>) -> Vec<MachineInfo> {
        work_machine_ids(st, "printer3d")
            .into_iter()
            .map(|(id, name)| {
                let state = if self.engine.busy(&id) || self.engine.latched(&id).is_some() {
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

    fn auto_start(&self, st: &Arc<AppState>, machine: &str) -> bool {
        Self::config(st, machine).is_ok_and(|c| c.auto_start_queue)
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
        self.engine.start(machine, &cfg, native_id)
    }

    fn pause(&self, st: &Arc<AppState>, machine: &str, native_id: &str) -> PluginResult<()> {
        let cfg = Self::config(st, machine)?;
        self.engine.pause(machine, &cfg, native_id)
    }

    fn resume(&self, st: &Arc<AppState>, machine: &str, native_id: &str) -> PluginResult<()> {
        let cfg = Self::config(st, machine)?;
        self.engine.resume(machine, &cfg, native_id)
    }

    fn stop(
        &self,
        st: &Arc<AppState>,
        machine: &str,
        native_id: &str,
        _by: &str,
    ) -> PluginResult<()> {
        let cfg = Self::config(st, machine)?;
        self.engine.stop(machine, &cfg, native_id)
    }

    fn status(
        &self,
        st: &Arc<AppState>,
        machine: &str,
        native_id: &str,
    ) -> PluginResult<PluginStatus> {
        let cfg = Self::config(st, machine)?;
        self.engine.status(machine, &cfg, native_id)
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
            "temperatures" if body.is_null() => self.engine.temperatures(machine, &cfg),
            "temperatures" => self.engine.set_temperature(machine, &cfg, body),
            "layer" => self.engine.layer(machine, &cfg),
            "emergency-stop" => self.engine.emergency_stop(machine, &cfg),
            "release" => Ok(self.engine.release(machine, &cfg)),
            "compat/moonraker" => self.engine.compat(machine, &cfg, "moonraker"),
            "compat/octoprint" => self.engine.compat(machine, &cfg, "octoprint"),
            _ => Err(PluginError::NotSupported(
                "the machine has no such operation",
            )),
        }
    }
}
