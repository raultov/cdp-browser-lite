#![allow(missing_docs)]

//! # cdp-browser-lite
//!
//! Total lifecycle control for Chrome instances, seamlessly integrating with `cdp-lite`.
//!
//! Provides the ability to spawn, attach, and manage Google Chrome or Chromium instances,
//! ensuring correct cleanup, avoiding zombie processes, and offering a reliable interface
//! for Headless Chrome automation.
//!
//! ## Example
//!
//! ```no_run
//! use std::time::Duration;
//! use cdp_browser_lite::browser::Browser;
//! use cdp_browser_lite::config::{BrowserConfig, LaunchMode};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let config = BrowserConfig::builder()
//!         .mode(LaunchMode::LaunchNew)
//!         .port(0) // Ephemeral port
//!         .headless(true)
//!         .build();
//!
//!     // Launch the browser
//!     let browser = Browser::ensure(config).await?;
//!     let (host, port) = browser.debug_address();
//!     println!("Chrome launched at {host}:{port}");
//!
//!     // Obtain a CDP client directly from the browser
//!     let mut client = browser.client().await?;
//!     
//!     // Stop the browser cleanly
//!     browser.stop().await?;
//!     Ok(())
//! }
//! ```

pub mod browser;
pub mod config;
pub mod discovery;
pub mod error;
pub mod ports;
pub mod probe;
pub mod process;
pub mod profile;

// Re-exports
pub use browser::Browser;
pub use cdp_lite::client::CdpClient;
pub use cdp_lite::error::{CdpError, CdpResult};
pub use cdp_lite::event_filter::EventFilter;
pub use cdp_lite::protocol::{NoParams, WsCommand, WsResponse};
pub use config::{BrowserConfig, LaunchMode, ProfileMode};
pub use error::BrowserError;
