//! Agent- and framework-neutral domain contracts for NEMESIS.

pub mod config;
pub mod error;
pub mod evidence;
pub mod model;
pub mod store;

pub use error::{CoreError, Result};
pub use evidence::*;
pub use model::*;
