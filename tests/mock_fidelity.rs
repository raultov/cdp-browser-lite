mod support;

use std::time::Duration;

use cdp_browser_lite::test_support::mock_devtools::{MockDevTools, MockWsBehavior};
use cdp_lite::browser::BrowserClient;
use serde_json::json;

async fn connect_browser(mock: &MockDevTools) -> BrowserClient {
    BrowserClient::connect(
        &format!("127.0.0.1:{}", mock.http_port),
        Duration::from_secs(3),
    )
    .await
    .expect("BrowserClient must connect to the mock")
}

#[tokio::test]
async fn given_mock_when_browser_client_connects_then_succeeds() {
    let mock = MockDevTools::start(MockWsBehavior::StayOpen).await;
    let browser = connect_browser(&mock).await;

    let targets = browser
        .list_targets()
        .await
        .expect("list_targets must reach the mock");
    assert!(!targets.is_empty(), "mock must report seeded targets");
}

#[tokio::test]
async fn given_mock_when_list_targets_then_returns_seeded_targets() {
    let mock = MockDevTools::start(MockWsBehavior::StayOpen).await;
    let browser = connect_browser(&mock).await;

    let targets = browser.list_targets().await.expect("list_targets");
    let ids: Vec<&str> = targets.iter().map(|t| t.target_id.as_str()).collect();

    assert_eq!(
        ids,
        ["T-page-1", "T-devtools", "T-worker"],
        "list_targets must return the seeded targets in order"
    );
}

#[tokio::test]
async fn given_mock_when_list_tabs_then_excludes_devtools_and_workers() {
    let mock = MockDevTools::start(MockWsBehavior::StayOpen).await;
    let browser = connect_browser(&mock).await;

    let tabs = browser.list_tabs().await.expect("list_tabs");
    let ids: Vec<&str> = tabs.iter().map(|t| t.target_id.as_str()).collect();

    assert_eq!(
        ids,
        ["T-page-1"],
        "list_tabs must drop the devtools page and the service worker"
    );
}

#[tokio::test]
async fn given_mock_when_create_target_then_returns_unique_target_id() {
    let mock = MockDevTools::start(MockWsBehavior::StayOpen).await;
    let browser = connect_browser(&mock).await;

    let response = browser
        .client()
        .send_raw_command(
            "Target.createTarget",
            json!({ "url": "https://example.test/" }),
        )
        .await
        .expect("createTarget must succeed");

    let target_id = response
        .result
        .as_ref()
        .and_then(|r| r.get("targetId"))
        .and_then(|v| v.as_str())
        .expect("createTarget response must carry a targetId");

    assert!(
        target_id.starts_with("T-"),
        "mock-generated target id must use the T- prefix, got {target_id}"
    );

    let targets = browser.list_targets().await.expect("list_targets");
    assert!(
        targets.iter().any(|t| t.target_id == target_id),
        "created target {target_id} must appear in list_targets"
    );
}

#[tokio::test]
async fn given_mock_when_attach_to_target_then_returns_unique_session_id() {
    let mock = MockDevTools::start(MockWsBehavior::StayOpen).await;
    let browser = connect_browser(&mock).await;

    let tab = browser
        .attach("T-page-1")
        .await
        .expect("attach must succeed");

    assert!(
        tab.session_id().starts_with("S-"),
        "mock-generated session id must use the S- prefix, got {}",
        tab.session_id()
    );
}

#[tokio::test]
async fn given_mock_when_close_target_then_target_disappears() {
    let mock = MockDevTools::start(MockWsBehavior::StayOpen).await;
    let browser = connect_browser(&mock).await;

    let tab = browser
        .new_tab("https://example.test/")
        .await
        .expect("new_tab must succeed");
    let target_id = tab.target_id().to_string();

    browser
        .close_tab(&target_id)
        .await
        .expect("close_tab must succeed");

    let targets = browser.list_targets().await.expect("list_targets");
    assert!(
        !targets.iter().any(|t| t.target_id == target_id),
        "closed target must disappear from list_targets"
    );
}

#[tokio::test]
async fn given_mock_when_attaching_twice_then_two_distinct_sessions() {
    let mock = MockDevTools::start(MockWsBehavior::StayOpen).await;
    let browser = connect_browser(&mock).await;

    let first = browser.attach("T-page-1").await.expect("first attach");
    let second = browser.attach("T-page-1").await.expect("second attach");

    assert_ne!(
        first.session_id(),
        second.session_id(),
        "two attaches to the same target must yield two distinct sessions"
    );

    for tab in [&first, &second] {
        tab.send_raw_command("Target.activateTarget", json!({}))
            .await
            .expect("each session must be independently usable");
    }
}
