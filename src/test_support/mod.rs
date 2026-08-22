//! Test support utilities for downstream consumers of `cdp-browser-lite`.
//!
//! This module is only compiled when the `test-support` cargo feature is
//! enabled, so downstream binaries do not pay for the extra dependencies
//! (`tokio-tungstenite`, `futures-util`) unless they opt in.
//!
//! Enable it in your own project with:
//!
//! ```toml
//! [dev-dependencies]
//! cdp-browser-lite = { version = "0.3", features = ["test-support"] }
//! ```
//!
//! Then point the library at the mock just like the crate's own integration
//! tests do:
//!
//! ```no_run
//! use cdp_browser_lite::test_support::mock_devtools::{
//!     MockBehavior, MockChrome, MockDevTools, MockWsBehavior,
//! };
//!
//! # async fn run() {
//! let http = MockChrome::start(MockBehavior::KeepAlive).await;
//! let devtools = MockDevTools::start(MockWsBehavior::StayOpen).await;
//! println!("mock HTTP on {} / WS on {}", http.port, devtools.ws_port);
//! # }
//! ```

pub mod mock_devtools;
