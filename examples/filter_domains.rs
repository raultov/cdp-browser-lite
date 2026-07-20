//! Live network-domain inspector built on top of `cdp-browser-lite`.
//!
//! This is the "complex" sibling of the `cdp-lite` `filter_domains` example.
//! That upstream example assumes a Chrome already listening on
//! `127.0.0.1:9222` and merely merges a couple of CDP event streams. Here we
//! exercise the whole value proposition of `cdp-browser-lite`: we **launch and
//! manage** a headless Chrome, then filter and aggregate its live CDP event
//! traffic to build a report of every web domain the page contacts.
//!
//! What it demonstrates:
//! 1. Launching a managed headless Chrome (ephemeral port + ephemeral profile).
//! 2. Subscribing to per-CDP-domain event streams via `CdpClient::on_domain`
//!    and merging them into one with `StreamExt::merge` (the core trick of the
//!    upstream example).
//! 3. Consuming that merged stream from a background task, filtering the
//!    `Network.*` events and classifying every request by its web host into
//!    first-party vs third-party traffic.
//! 4. Navigating and waiting for `Page.loadEventFired` (delivered through the
//!    very same event stream, not by polling).
//! 5. Printing an aggregated report: hosts contacted, first/third-party split,
//!    HTTP status distribution and a breakdown of CDP events seen.
//! 6. A clean shutdown (no zombie process, ephemeral profile removed).
//!
//! Run it with logs:
//! ```bash
//! RUST_LOG=info cargo run --example filter_domains
//! # add cdp event detail:
//! RUST_LOG=debug cargo run --example filter_domains
//! ```
//!
//! In sandboxed environments (Docker/CI) set `CHROME_NO_SANDBOX=1`, which the
//! launcher honours automatically.

use std::collections::BTreeMap;
use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cdp_browser_lite::config::{BrowserConfig, LaunchMode, ProfileMode};
use cdp_browser_lite::{Browser, CdpClient};
use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio_stream::StreamExt;
use tracing::{debug, info, warn};
use tracing_subscriber::{EnvFilter, fmt};

const TARGET_URL: &str = "https://www.rust-lang.org";

/// Aggregated view of everything observed on the merged CDP event stream.
#[derive(Default)]
struct Capture {
    /// Count of every CDP event method seen (e.g. `Network.requestWillBeSent`).
    methods: BTreeMap<String, u64>,
    /// Number of requests issued towards each web host.
    hosts: BTreeMap<String, u64>,
    /// Distribution of HTTP response status codes.
    statuses: BTreeMap<u64, u64>,
    first_party: u64,
    third_party: u64,
    total_events: u64,
}

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
        .user_agent("cdp-browser-lite/filter_domains")
        .connect_timeout(Duration::from_secs(10))
        .startup_timeout(Duration::from_secs(20))
        .build();

    info!("Launching managed headless Chrome...");
    let browser = Browser::ensure(config).await?;
    let (host, port) = browser.debug_address();
    info!(%host, port, managed = browser.is_managed(), "Chrome is up");

    let client = browser.client().await?;

    // 2. Enable the CDP domains whose events we want. This must happen before
    //    we navigate, otherwise the requests fire before Chrome starts
    //    reporting them.
    client.send_raw_command("Page.enable", json!({})).await?;
    client.send_raw_command("Network.enable", json!({})).await?;

    // 3. Merge the `Network` and `Page` domain streams into one, exactly like
    //    the upstream `filter_domains` example, and drain it from a background
    //    task into a shared `Capture`.
    let capture = Arc::new(Mutex::new(Capture::default()));
    let loaded = Arc::new(Notify::new());
    let base_host = host_of(TARGET_URL).unwrap_or_default();
    let collector = spawn_collector(&client, capture.clone(), loaded.clone(), base_host);

    // 4. Navigate and wait for the load event delivered on the stream itself.
    navigate(&client, TARGET_URL).await?;
    wait_for_load(&loaded, Duration::from_secs(15)).await;
    // Let late sub-resource requests settle before we stop listening.
    tokio::time::sleep(Duration::from_millis(750)).await;

    // 5. Report. We stop the collector first so nothing mutates `capture`
    //    while we read it.
    collector.abort();
    report(&capture.lock().unwrap());

    // 6. Clean shutdown: terminates the process and removes the ephemeral profile.
    info!("Stopping browser...");
    browser.stop().await?;
    info!(alive = browser.is_alive(), "Browser stopped");

    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).init();
}

/// Spawns the task that owns the merged `Network` + `Page` event stream.
fn spawn_collector(
    client: &CdpClient,
    capture: Arc<Mutex<Capture>>,
    loaded: Arc<Notify>,
    base_host: String,
) -> tokio::task::JoinHandle<()> {
    // `on_domain` yields a `Stream` of events whose method starts with the
    // given CDP domain prefix; merging keeps a single `next()` loop.
    let network = client.on_domain("Network");
    let page = client.on_domain("Page");
    let mut merged = network.merge(page);

    tokio::spawn(async move {
        while let Some(item) = merged.next().await {
            match item {
                Ok(event) => handle_event(&capture, &loaded, &event, &base_host),
                Err(err) => warn!(%err, "event stream error"),
            }
        }
    })
}

/// Routes a single CDP event into the aggregate and signals page load.
fn handle_event(
    capture: &Mutex<Capture>,
    loaded: &Notify,
    event: &cdp_browser_lite::WsResponse,
    base_host: &str,
) {
    let method = match event.method.as_deref() {
        Some(method) => method,
        None => return,
    };

    let mut cap = capture.lock().unwrap();
    cap.total_events += 1;
    *cap.methods.entry(method.to_string()).or_insert(0) += 1;

    match method {
        "Network.requestWillBeSent" => record_request(&mut cap, event.params.as_ref(), base_host),
        "Network.responseReceived" => record_response(&mut cap, event.params.as_ref()),
        "Page.loadEventFired" => loaded.notify_one(),
        _ => {}
    }
}

/// Classifies an outgoing request by host into first- vs third-party.
fn record_request(cap: &mut Capture, params: Option<&Value>, base_host: &str) {
    let url = params
        .and_then(|p| p.get("request"))
        .and_then(|r| r.get("url"))
        .and_then(Value::as_str)
        .unwrap_or_default();

    let host = match host_of(url) {
        Some(host) => host,
        None => return,
    };

    if is_same_site(&host, base_host) {
        cap.first_party += 1;
    } else {
        cap.third_party += 1;
    }
    *cap.hosts.entry(host).or_insert(0) += 1;
}

/// Records the HTTP status code carried by a `Network.responseReceived` event.
fn record_response(cap: &mut Capture, params: Option<&Value>) {
    let status = params
        .and_then(|p| p.get("response"))
        .and_then(|r| r.get("status"))
        .and_then(Value::as_u64);

    if let Some(code) = status {
        *cap.statuses.entry(code).or_insert(0) += 1;
    }
}

async fn navigate(client: &CdpClient, url: &str) -> Result<(), Box<dyn Error>> {
    info!(%url, "Navigating");
    client
        .send_raw_command("Page.navigate", json!({ "url": url }))
        .await?;
    Ok(())
}

/// Waits for the load event (signalled by the collector) or times out.
async fn wait_for_load(loaded: &Notify, timeout: Duration) {
    match tokio::time::timeout(timeout, loaded.notified()).await {
        Ok(()) => info!("Page.loadEventFired received"),
        Err(_) => warn!("Timed out waiting for load event; reporting anyway"),
    }
}

fn report(cap: &Capture) {
    info!(total_events = cap.total_events, "Capture complete");
    info!(
        first_party = cap.first_party,
        third_party = cap.third_party,
        unique_hosts = cap.hosts.len(),
        "Request breakdown"
    );

    for (host, count) in top(&cap.hosts, 8) {
        info!(%host, requests = count, "Contacted host");
    }
    for (code, count) in &cap.statuses {
        info!(status = code, count = count, "HTTP status");
    }
    // Only visible with RUST_LOG=debug: the raw CDP event tally.
    for (method, count) in &cap.methods {
        debug!(%method, count = count, "CDP event");
    }
}

/// Returns the `n` busiest hosts, ordered by request count (desc), host (asc).
fn top(map: &BTreeMap<String, u64>, n: usize) -> Vec<(String, u64)> {
    let mut entries: Vec<(String, u64)> = map.iter().map(|(k, v)| (k.clone(), *v)).collect();
    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    entries.truncate(n);
    entries
}

/// Extracts a lowercase host from an HTTP(S) URL without pulling in a URL
/// crate. Non-web schemes (`chrome://`, `devtools://`, `data:`, ...) yield
/// `None` so internal browser traffic is excluded from the report.
fn host_of(url: &str) -> Option<String> {
    let (scheme, after_scheme) = url.split_once("://")?;
    if !matches!(scheme, "http" | "https") {
        return None;
    }
    let authority = after_scheme.split(['/', '?', '#']).next()?;
    // Strip any `user:pass@` prefix and the `:port` suffix.
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = host.split(':').next()?;
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}

/// Naive "same registrable domain" check (last two labels), enough to split
/// first-party from third-party traffic in a demo.
fn is_same_site(host: &str, base_host: &str) -> bool {
    !base_host.is_empty() && registrable(host) == registrable(base_host)
}

fn registrable(host: &str) -> String {
    let labels: Vec<&str> = host.split('.').collect();
    let len = labels.len();
    if len >= 2 {
        labels[len - 2..].join(".")
    } else {
        host.to_string()
    }
}
