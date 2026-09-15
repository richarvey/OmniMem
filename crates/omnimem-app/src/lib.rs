//! What the headless server and the desktop app share.
//!
//! Both run the same services (the engine, the enrichment worker, the RSS
//! scheduler and the MCP server) from [`run_services`]. The server runs it on
//! the main thread and stops on a signal; the desktop app runs it on a
//! background thread, because the platform event loop must own the main
//! thread, and stops it from the tray's Quit. This crate has no GUI
//! dependencies, so it builds and tests everywhere.

mod instance;
mod services;

pub use instance::{Instance, InstanceLock, acquire, request_show, take_show_request};
pub use services::{
    ServiceState, data_dir, default_data_dir, feeds_path, load_embedder, local_mcp_url,
    open_engine, run_services,
};
