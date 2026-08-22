# cdp-browser-lite

[![CI](https://github.com/raultov/cdp-browser-lite/actions/workflows/ci.yml/badge.svg)](https://github.com/raultov/cdp-browser-lite/actions/workflows/ci.yml)
[![Release](https://github.com/raultov/cdp-browser-lite/actions/workflows/release.yml/badge.svg)](https://github.com/raultov/cdp-browser-lite/actions/workflows/release.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Total lifecycle control for Chrome instances, seamlessly integrating with `cdp-lite`.

## Overview
`cdp-browser-lite` provides the ability to spawn, attach, and manage Google Chrome or Chromium instances, ensuring correct cleanup, avoiding zombie processes, and offering a reliable interface for Headless Chrome automation.

It re-exports the complete interface of `cdp-lite` for convenient access to the Chrome DevTools Protocol.

## Quickstart

```rust
use cdp_browser_lite::browser::Browser;
use cdp_browser_lite::config::{BrowserConfig, LaunchMode};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = BrowserConfig::builder()
        .mode(LaunchMode::LaunchNew)
        .port(0) // Auto ephemeral port
        .headless(true)
        .build();

    let browser = Browser::ensure(config).await?;
    let mut client = browser.client().await?;
    
    let version = client.send_raw_command("Browser.getVersion", serde_json::json!({})).await?;
    println!("Chrome Version: {}", version.result.as_ref().unwrap()["product"]);

    browser.stop().await?;
    Ok(())
}
```

## Multi-tab

A single managed Chrome instance can drive N tabs over **one** browser-level WebSocket
connection, with commands routed per-tab by `sessionId`:

```rust
use cdp_browser_lite::browser::Browser;
use cdp_browser_lite::config::{BrowserConfig, LaunchMode};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let browser = Browser::ensure(
        BrowserConfig::builder()
            .mode(LaunchMode::LaunchNew)
            .port(0)
            .headless(true)
            .build(),
    )
    .await?;

    let docs = browser.new_tab("https://docs.rs").await?;
    let crates = browser.new_tab("https://crates.io").await?;

    // Each command only affects its own tab.
    docs.send_raw_command("Page.enable", cdp_browser_lite::NoParams).await?;
    crates.send_raw_command("Page.reload", serde_json::json!({})).await?;

    for tab in browser.list_tabs().await? {
        println!("{} -> {}", tab.target_id, tab.url);
    }

    browser.stop().await?;
    Ok(())
}
```

Notes:

- `new_tab`, `attach_tab`, `attach_to_all_tabs`, `list_tabs` and `close_tab` all go
  through the shared browser-level connection obtained from `browser_client()`.
- If you call **both** `client()` and `browser_client()` on the same `Browser`, two
  WebSocket connections are opened. This is correct and intentional: `client()` targets
  a page-level endpoint while `browser_client()` targets the browser-level endpoint
  (`/json/version`), which is a different socket.
- Dropping a `Tab` neither closes nor detaches it; call `Tab::close` or `Tab::detach`
  explicitly (upstream `cdp-lite` semantics).

## Launch Modes
- `LaunchNew`: Always spawns a new managed browser process.
- `AttachOnly`: Attaches to an already running instance.
- `Auto`: Tries to attach first, and if unavailable, launches a new instance.

## Profile Modes
- `Ephemeral`: Creates a temporary profile that is cleaned up when dropped.
- `Persistent`: Uses a persistent directory.
- `PersistentPerPort`: Derives the profile directory from the resolved port (`root/{prefix}{port}`),
  enabling independent multi-instance `LaunchMode::Auto` scenarios.
- `UserDefault`: Omits the profile flag, using the system's default Chrome profile.

## Chrome >= 151 compatibility

Chrome >= 151 writes `SingletonLock` as a **dangling symlink** (the symlink target is never
created). `cdp-browser-lite` >= 0.2.3 detects this correctly using `symlink_metadata` so
that `LaunchMode::Auto` / `ProfileMode::PersistentPerPort` can distinguish a live managed
instance from an unoccupied port on Chrome >= 151.

## Test support for downstream crates

`cdp-browser-lite` ships with the in-process DevTools HTTP and WebSocket mocks used by its
own integration tests. They are re-exported under `cdp_browser_lite::test_support` and gated
behind the `test-support` cargo feature so downstream binaries do not pay for the extra
dependencies (`tokio-tungstenite`, `futures-util`) unless they opt in.

Enable the feature in your own project:

```toml
[dev-dependencies]
cdp-browser-lite = { version = "0.3", features = ["test-support"] }
```

Then point the library at the mock just like the crate's own integration tests do:

```rust
use cdp_browser_lite::test_support::mock_devtools::{
    MockBehavior, MockChrome, MockDevTools, MockWsBehavior,
};

#[tokio::test]
async fn my_attach_only_test() {
    // Stands up a fake `/json/version` + CDP WebSocket peer on ephemeral ports.
    let http = MockChrome::start(MockBehavior::KeepAlive).await;
    let devtools = MockDevTools::start(MockWsBehavior::StayOpen).await;
    assert!(devtools.connection_count() >= 0);
    // ...use `devtools.http_port` and `devtools.ws_port` to drive `Browser`...
}
```

The mocks cover the same scenarios as the upstream `tests/support/mock_devtools.rs` did:
HTTP probe behaviour (`KeepAlive`, `CloseAfterResponse`, `KeepAliveThenCloseAfter`,
`SilentPeer`, `NotChrome`, `IgnoresHttp10`), the full `Browser.*` / `Target.*` dispatch
over WebSocket, `connection_count` accounting, and `drop_new_connections` to force a
server-side socket close for reconnect-path tests.

## Troubleshooting
- **CHROME_PATH**: Set `CHROME_PATH` to specify a custom Chrome executable location.
- **Docker/Sandbox**: If running in Docker, you may need to add `.no_sandbox(true)` to `BrowserConfigBuilder`.

## Supported Platforms

Every push and pull request is validated by CI, which lints (`clippy`, `rustfmt`),
builds and runs the full test suite on the three major operating systems, and then
proves the library links into runnable executables by building native binaries for
each target:

| OS      | Target                      |
| ------- | --------------------------- |
| Linux   | `x86_64-unknown-linux-gnu`  |
| macOS   | `aarch64-apple-darwin`      |
| macOS   | `x86_64-apple-darwin`       |
| Windows | `x86_64-pc-windows-msvc`    |

Pushing a version tag (`vX.Y.Z`) triggers the release workflow, which packages the
compiled binaries for each platform and attaches them to the corresponding GitHub
Release.
