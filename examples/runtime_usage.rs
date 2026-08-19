//! End-to-end runtime example.
//!
//! Unlike the `cdp-lite` `runtime_usage` example — which assumes a Chrome
//! already listening on `127.0.0.1:9222` — this one exercises the whole value
//! proposition of `cdp-browser-lite`: it **launches and manages** a headless
//! Chrome on an ephemeral port, drives it over CDP, and then shuts it down
//! cleanly (no zombie processes, ephemeral profile removed).
//!
//! Flow:
//! 1. Launch a managed headless Chrome (ephemeral port + ephemeral profile).
//! 2. Report the browser version via `Browser.getVersion`.
//! 3. Navigate to a page and wait until `document.readyState === "complete"`.
//! 4. Scrape the DOM synchronously (`Runtime.evaluate`, `returnByValue`).
//! 5. Run an async expression that awaits a `fetch()` promise.
//! 6. Issue a heavier CDP call (`Page.captureScreenshot`) and report its size.
//! 7. Stop the browser and clean up.
//!
//! Run it with logs:
//! ```bash
//! RUST_LOG=info cargo run --example runtime_usage
//! ```
//!
//! In sandboxed environments (Docker/CI) set `CHROME_NO_SANDBOX=1`, which the
//! launcher honours automatically.

use std::error::Error;
use std::time::{Duration, Instant};

use cdp_browser_lite::config::{BrowserConfig, LaunchMode, ProfileMode};
use cdp_browser_lite::{Browser, CdpClient, CdpResult, WsResponse};
use serde_json::json;
use tracing::{info, warn};
use tracing_subscriber::{EnvFilter, fmt};

const TARGET_URL: &str = "https://www.rust-lang.org";

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    init_tracing();

    // 1. Launch a fully managed, headless Chrome. Port 0 => ephemeral; the
    //    library discovers the real port from `DevToolsActivePort`.
    let config = BrowserConfig::builder()
        .mode(LaunchMode::LaunchNew)
        .port(0)
        .headless(true)
        .profile(ProfileMode::Ephemeral)
        .window_size(1280, 800)
        .user_agent("cdp-browser-lite/runtime_usage")
        .connect_timeout(Duration::from_secs(10))
        .startup_timeout(Duration::from_secs(20))
        .build();

    info!("Launching managed headless Chrome...");
    let browser = Browser::ensure(config).await?;
    let (host, port) = browser.debug_address().await;
    info!(%host, port, managed = browser.is_managed().await, "Chrome is up");

    // The client is cached and reused by the browser on subsequent calls.
    let client = browser.client().await?;

    // 2. Browser version.
    report_version(&client).await?;

    // 3. Enable the domains we need, navigate and wait for the load to settle.
    client.send_raw_command("Page.enable", json!({})).await?;
    client.send_raw_command("Runtime.enable", json!({})).await?;
    navigate_and_wait(&client, TARGET_URL, Duration::from_secs(15)).await?;

    // 4. Synchronous DOM scraping.
    scrape_page(&client).await?;

    // 5. Async expression awaiting a promise.
    report_fetch_status(&client).await?;

    // 6. A heavier CDP round-trip.
    report_screenshot(&client).await?;

    // 7. Clean shutdown: terminates the process and removes the ephemeral profile.
    info!("Stopping browser...");
    browser.stop().await?;
    info!(alive = browser.is_alive().await, "Browser stopped");

    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).init();
}

/// Runs `Runtime.evaluate` with `returnByValue`, optionally awaiting a promise.
async fn evaluate(
    client: &CdpClient,
    expression: &str,
    await_promise: bool,
) -> CdpResult<WsResponse> {
    client
        .send_raw_command(
            "Runtime.evaluate",
            json!({
                "expression": expression,
                "returnByValue": true,
                "awaitPromise": await_promise,
            }),
        )
        .await
}

/// Extracts the `result.result.value` payload from a `Runtime.evaluate` response.
fn eval_value(resp: &WsResponse) -> Option<&serde_json::Value> {
    resp.result.as_ref()?.get("result")?.get("value")
}

async fn report_version(client: &CdpClient) -> Result<(), Box<dyn Error>> {
    let resp = client
        .send_raw_command("Browser.getVersion", json!({}))
        .await?;
    let info = resp.result.unwrap_or_default();
    info!(
        product = %info["product"].as_str().unwrap_or("unknown"),
        protocol = %info["protocolVersion"].as_str().unwrap_or("?"),
        "Browser.getVersion"
    );
    Ok(())
}

async fn navigate_and_wait(
    client: &CdpClient,
    url: &str,
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    info!(%url, "Navigating");
    client
        .send_raw_command("Page.navigate", json!({ "url": url }))
        .await?;

    let deadline = Instant::now() + timeout;
    loop {
        let resp = evaluate(client, "document.readyState", false).await?;
        let ready = eval_value(&resp).and_then(|v| v.as_str()).unwrap_or("");
        if ready == "complete" {
            info!("Document reached readyState=complete");
            return Ok(());
        }
        if Instant::now() >= deadline {
            warn!(last_state = %ready, "Load timed out; continuing anyway");
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn scrape_page(client: &CdpClient) -> Result<(), Box<dyn Error>> {
    let expression = r#"({
        title: document.title,
        url: location.href,
        h1: document.querySelector("h1")?.innerText ?? null,
        links: document.querySelectorAll("a").length,
        description: document
            .querySelector('meta[name="description"]')
            ?.getAttribute("content") ?? null
    })"#;

    let resp = evaluate(client, expression, false).await?;
    match eval_value(&resp) {
        Some(value) => info!(page = %value, "Scraped page"),
        None => warn!(raw = ?resp.result, "Unexpected evaluate shape"),
    }
    Ok(())
}

async fn report_fetch_status(client: &CdpClient) -> Result<(), Box<dyn Error>> {
    let expression = r#"(async () => {
        const start = performance.now();
        const r = await fetch(location.href, { method: "GET" });
        return { status: r.status, ok: r.ok, ms: Math.round(performance.now() - start) };
    })()"#;

    let resp = evaluate(client, expression, true).await?;
    match eval_value(&resp) {
        Some(value) => info!(fetch = %value, "Async fetch resolved"),
        None => warn!(raw = ?resp.result, "Unexpected async evaluate shape"),
    }
    Ok(())
}

async fn report_screenshot(client: &CdpClient) -> Result<(), Box<dyn Error>> {
    let resp = client
        .send_raw_command("Page.captureScreenshot", json!({ "format": "png" }))
        .await?;
    let encoded_len = resp
        .result
        .as_ref()
        .and_then(|r| r.get("data"))
        .and_then(|d| d.as_str())
        .map(str::len)
        .unwrap_or(0);
    // base64 expands 3 bytes into 4 chars, so this approximates the PNG size.
    let approx_bytes = encoded_len / 4 * 3;
    info!(
        base64_len = encoded_len,
        approx_png_bytes = approx_bytes,
        "Captured screenshot"
    );
    Ok(())
}
