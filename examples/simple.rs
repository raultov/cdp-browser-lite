use cdp_browser_lite::browser::Browser;
use cdp_browser_lite::config::{BrowserConfig, LaunchMode};
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).init();

    let config = BrowserConfig::builder()
        .mode(LaunchMode::LaunchNew)
        .port(0)
        .headless(true)
        .build();

    info!("Launching Chrome...");
    let browser = Browser::ensure(config).await?;
    let (host, port) = browser.debug_address();
    info!(%host, port, "Chrome launched");

    let client = browser.client().await?;

    let version = client
        .send_raw_command("Browser.getVersion", serde_json::json!({}))
        .await?;
    info!(
        product = %version.result.as_ref().unwrap()["product"],
        "Chrome version"
    );

    browser.stop().await?;
    info!("Chrome stopped");

    Ok(())
}
