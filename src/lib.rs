//! The EH3D server: the center of the EH3D programs. DRILLPRO, ROUTEPRO, the Android app, the Studio and the panel next to the machine
//! send it jobs; it plans them, runs them on the machine through its node and tells everybody how they go.
//!
//! The planning and the execution are the libraries of EH3D-DRILLPRO and EH3D-ROUTEPRO, so there is one implementation of them, not one per client.

pub mod api;
pub mod api_artifacts;
pub mod api_auth;
pub mod api_catalogs;
pub mod api_projects;
pub mod api_pstore;
pub mod api_tasks;
pub mod api_work;
pub mod apps;
pub mod artifacts;
pub mod audit;
pub mod auth;
pub mod catalogs;
pub mod config;
pub mod design;
pub mod emergency;
pub mod errors;
pub mod feed;
pub mod gcode;
pub mod gcode_run;
pub mod head;
pub mod ids;
pub mod inspect;
pub mod jobs;
pub mod machines;
pub mod panic;
pub mod plan;
pub mod plugins;
pub mod policy;
pub mod projects;
pub mod pstore;
pub mod spindle;
pub mod state;
pub mod store;
pub mod sync;
pub mod tasks;
pub mod tickets;
pub mod updates;
pub mod work;
