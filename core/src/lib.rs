//! DownloaDan core: the download engine, the queue manager and the browser-extension bridge.
//! No UI code lives here, so everything can be tested without a window.

pub mod api;
pub mod bridge;
pub mod engine;
pub mod manager;
pub mod model;
pub mod theme;
pub mod util;

pub use manager::{AddReq, Event, Manager};
