//! The EH3D server: the center of the EH3D programs. DRILLPRO, ROUTEPRO, the Android app, the Studio and the panel next to the machine
//! send it jobs; it plans them, runs them on the machine through its node and tells everybody how they go.
//!
//! The planning and the execution are the libraries of EH3D-DRILLPRO and EH3D-ROUTEPRO, so there is one implementation of them, not one per client.

pub mod api;
pub mod auth;
pub mod config;
pub mod head;
pub mod inspect;
pub mod jobs;
pub mod machines;
pub mod plan;
pub mod state;
pub mod store;
pub mod updates;
