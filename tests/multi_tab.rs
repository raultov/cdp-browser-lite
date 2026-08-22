mod support;

use std::time::Duration;

use cdp_browser_lite::BrowserPool;
use cdp_browser_lite::browser::{Browser, BrowserState};
use cdp_browser_lite::config::{BrowserConfig, LaunchMode};
use cdp_browser_lite::error::BrowserError;
use support::mock_devtools::{MockDevTools, MockWsBehavior};

fn attach_only_config(mock: &MockDevTools) -> BrowserConfig {
    BrowserConfig::builder()
        .mode(LaunchMode::AttachOnly)
        .host("127.0.0.1")
        .port(mock.http_port)
        .connect_timeout(Duration::from_secs(3))
        .build()
}

#[tokio::test]
async fn given_attached_browser_when_browser_client_then_connects() {
    let mock = MockDevTools::start(MockWsBehavior::StayOpen).await;
    let browser = Browser::ensure(attach_only_config(&mock))
        .await
        .expect("ensure must succeed");

    let browser_client = browser
        .browser_client()
        .await
        .expect("browser_client must connect to the mock");

    let targets = browser_client
        .list_targets()
        .await
        .expect("browser-level command must work");
    assert!(!targets.is_empty(), "mock must report seeded targets");
}

#[tokio::test]
async fn given_browser_client_called_twice_then_reuses_single_connection() {
    let mock = MockDevTools::start(MockWsBehavior::StayOpen).await;
    let browser = Browser::ensure(attach_only_config(&mock))
        .await
        .expect("ensure must succeed");

    let _first = browser.browser_client().await.expect("first client");
    let _second = browser.browser_client().await.expect("second client");

    tokio::time::sleep(Duration::from_millis(300)).await;

    let ws_conns = mock.connection_count();
    assert_eq!(
        ws_conns, 1,
        "expected exactly 1 active WS connection (reused), got {ws_conns}"
    );
}

#[tokio::test]
async fn given_stopped_browser_when_browser_client_then_stopped_error() {
    let mock = MockDevTools::start(MockWsBehavior::StayOpen).await;
    let browser = Browser::ensure(attach_only_config(&mock))
        .await
        .expect("ensure must succeed");
    browser.stop().await.expect("stop must succeed");

    let result = browser.browser_client().await;
    match result {
        Err(BrowserError::Stopped) => {}
        Err(e) => panic!("expected Stopped after browser stopped, got other error: {e}"),
        Ok(_) => panic!("expected Stopped after browser stopped, got Ok"),
    }
}

#[tokio::test]
async fn given_relaunch_when_browser_client_then_targets_new_process() {
    // First-generation mock claims the configured port, then dies. The
    // relaunch probe inside browser_client() must therefore land on the
    // second-generation mock serving the same port.
    let first = MockDevTools::start(MockWsBehavior::StayOpen).await;
    let port = first.http_port;
    drop(first);

    let _second = MockDevTools::start_on(port).await;

    let cfg = BrowserConfig::builder()
        .mode(LaunchMode::AttachOnly)
        .host("127.0.0.1")
        .port(port)
        .auto_relaunch(true)
        .connect_timeout(Duration::from_secs(3))
        .build();

    // Managed with no live process: the first accessor call must relaunch.
    let state = BrowserState::new(None, None, port, true);
    let browser = Browser::test_from_state(cfg, state);

    let browser_client = browser
        .browser_client()
        .await
        .expect("relaunch and connect must succeed");

    // `T-new` only exists in the second-generation mock, proving the client
    // targets the new process rather than a stale pre-relaunch endpoint.
    let targets = browser_client.list_targets().await.expect("list_targets");
    assert!(
        targets.iter().any(|t| t.target_id == "T-new"),
        "browser_client after relaunch must see the new process's targets"
    );
}

#[tokio::test]
async fn given_browser_when_new_tab_then_returns_tab_with_ids() {
    let mock = MockDevTools::start(MockWsBehavior::StayOpen).await;
    let browser = Browser::ensure(attach_only_config(&mock))
        .await
        .expect("ensure must succeed");

    let tab = browser
        .new_tab("https://example.test/")
        .await
        .expect("new_tab must succeed");

    assert!(
        tab.target_id().starts_with("T-"),
        "tab must carry a target id, got {}",
        tab.target_id()
    );
    assert!(
        tab.session_id().starts_with("S-"),
        "tab must carry a session id, got {}",
        tab.session_id()
    );
}

#[tokio::test]
async fn given_two_new_tabs_then_target_ids_differ() {
    let mock = MockDevTools::start(MockWsBehavior::StayOpen).await;
    let browser = Browser::ensure(attach_only_config(&mock))
        .await
        .expect("ensure must succeed");

    let first = browser.new_tab("about:blank").await.expect("first tab");
    let second = browser.new_tab("about:blank").await.expect("second tab");

    assert_ne!(
        first.target_id(),
        second.target_id(),
        "each new tab must be its own target"
    );
}

#[tokio::test]
async fn given_two_new_tabs_then_session_ids_differ() {
    let mock = MockDevTools::start(MockWsBehavior::StayOpen).await;
    let browser = Browser::ensure(attach_only_config(&mock))
        .await
        .expect("ensure must succeed");

    let first = browser.new_tab("about:blank").await.expect("first tab");
    let second = browser.new_tab("about:blank").await.expect("second tab");

    assert_ne!(
        first.session_id(),
        second.session_id(),
        "each attached tab must have its own session"
    );
}

#[tokio::test]
async fn given_new_tab_when_close_tab_then_disappears_from_list_tabs() {
    let mock = MockDevTools::start(MockWsBehavior::StayOpen).await;
    let browser = Browser::ensure(attach_only_config(&mock))
        .await
        .expect("ensure must succeed");

    let tab = browser.new_tab("about:blank").await.expect("new tab");
    let target_id = tab.target_id().to_string();

    browser
        .close_tab(&target_id)
        .await
        .expect("close_tab must succeed");

    let tabs = browser.list_tabs().await.expect("list_tabs");
    assert!(
        !tabs.iter().any(|t| t.target_id == target_id),
        "closed tab must disappear from list_tabs"
    );
}

#[tokio::test]
async fn given_seeded_targets_when_list_tabs_then_only_real_pages() {
    let mock = MockDevTools::start(MockWsBehavior::StayOpen).await;
    let browser = Browser::ensure(attach_only_config(&mock))
        .await
        .expect("ensure must succeed");

    let tabs = browser.list_tabs().await.expect("list_tabs");
    let ids: Vec<&str> = tabs.iter().map(|t| t.target_id.as_str()).collect();

    assert_eq!(
        ids,
        ["T-page-1"],
        "list_tabs must drop devtools pages and workers"
    );
}

#[tokio::test]
async fn given_multiple_tabs_when_attach_to_all_tabs_then_one_tab_per_page() {
    let mock = MockDevTools::start(MockWsBehavior::StayOpen).await;
    let browser = Browser::ensure(attach_only_config(&mock))
        .await
        .expect("ensure must succeed");

    let _extra = browser.new_tab("about:blank").await.expect("extra tab");

    let tabs = browser
        .attach_to_all_tabs()
        .await
        .expect("attach_to_all_tabs must succeed");

    let ids: Vec<&str> = tabs.iter().map(|t| t.target_id()).collect();
    assert_eq!(ids.len(), 2, "expected one tab per page, got {ids:?}");
    assert!(ids.contains(&"T-page-1"), "seed page must be attached");
}

#[tokio::test]
async fn given_target_id_when_attach_tab_then_returns_matching_tab() {
    let mock = MockDevTools::start(MockWsBehavior::StayOpen).await;
    let browser = Browser::ensure(attach_only_config(&mock))
        .await
        .expect("ensure must succeed");

    let tab = browser
        .attach_tab("T-page-1")
        .await
        .expect("attach_tab must succeed");

    assert_eq!(tab.target_id(), "T-page-1");
    assert!(
        tab.session_id().starts_with("S-"),
        "attached tab must carry a session id"
    );
}

#[tokio::test]
async fn given_stopped_browser_when_new_tab_then_stopped_error() {
    let mock = MockDevTools::start(MockWsBehavior::StayOpen).await;
    let browser = Browser::ensure(attach_only_config(&mock))
        .await
        .expect("ensure must succeed");
    browser.stop().await.expect("stop must succeed");

    let result = browser.new_tab("about:blank").await;
    match result {
        Err(BrowserError::Stopped) => {}
        Err(e) => panic!("expected Stopped after browser stopped, got other error: {e}"),
        Ok(_) => panic!("expected Stopped after browser stopped, got Ok"),
    }
}

#[tokio::test]
async fn given_three_tabs_when_counting_ws_connections_then_exactly_one() {
    let mock = MockDevTools::start(MockWsBehavior::StayOpen).await;
    let browser = Browser::ensure(attach_only_config(&mock))
        .await
        .expect("ensure must succeed");

    // Deliberately no client() call here: the page-level accessor opens a
    // second, page-level socket and would make this count 2.
    let tabs = [
        browser.new_tab("about:blank").await.expect("tab 1"),
        browser.new_tab("about:blank").await.expect("tab 2"),
        browser.new_tab("about:blank").await.expect("tab 3"),
    ];

    for tab in &tabs {
        tab.send_raw_command("Target.activateTarget", serde_json::json!({}))
            .await
            .expect("tab command must be routed");
    }

    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        mock.connection_count(),
        1,
        "all tabs must multiplex over exactly one WS connection"
    );
}

#[tokio::test]
async fn given_both_client_and_browser_client_then_two_ws_connections() {
    let mock = MockDevTools::start(MockWsBehavior::StayOpen).await;
    let browser = Browser::ensure(attach_only_config(&mock))
        .await
        .expect("ensure must succeed");

    let _page = browser.client().await.expect("page-level client");
    let _browser = browser
        .browser_client()
        .await
        .expect("browser-level client");

    tokio::time::sleep(Duration::from_millis(300)).await;

    // Intentional and documented: client() targets a page endpoint while
    // browser_client() targets the browser endpoint - two sockets.
    assert_eq!(
        mock.connection_count(),
        2,
        "page-level and browser-level accessors use distinct connections"
    );
}

#[tokio::test]
async fn given_open_tabs_when_stop_then_browser_client_returns_stopped() {
    let mock = MockDevTools::start(MockWsBehavior::StayOpen).await;
    let browser = Browser::ensure(attach_only_config(&mock))
        .await
        .expect("ensure must succeed");

    let _tab = browser.new_tab("about:blank").await.expect("open tab");
    browser.stop().await.expect("stop must succeed");

    let result = browser.browser_client().await;
    match result {
        Err(BrowserError::Stopped) => {}
        Err(e) => panic!("expected Stopped after stop, got other error: {e}"),
        Ok(_) => panic!("expected Stopped after stop, got Ok"),
    }
}

#[tokio::test]
async fn given_open_tabs_when_restart_then_new_browser_client_works() {
    let mock = MockDevTools::start(MockWsBehavior::StayOpen).await;
    let browser = Browser::ensure(attach_only_config(&mock))
        .await
        .expect("ensure must succeed");

    let _tab = browser.new_tab("about:blank").await.expect("open tab");
    browser.restart().await.expect("restart must succeed");

    let browser_client = browser
        .browser_client()
        .await
        .expect("browser_client must reconnect after restart");

    let targets = browser_client.list_targets().await.expect("list_targets");
    assert!(
        targets.iter().any(|t| t.target_id == "T-page-1"),
        "restarted browser must expose its own targets"
    );
}

#[tokio::test]
async fn given_pooled_browsers_when_each_opens_tabs_then_tabs_are_isolated() {
    let mock_a = MockDevTools::start(MockWsBehavior::StayOpen).await;
    let mock_b = MockDevTools::start(MockWsBehavior::StayOpen).await;

    let pool = BrowserPool::new();
    let id_a = pool
        .open(attach_only_config(&mock_a))
        .await
        .expect("open browser A");
    let id_b = pool
        .open(attach_only_config(&mock_b))
        .await
        .expect("open browser B");

    let browser_a = pool.get(id_a).await.expect("browser A in pool");
    let browser_b = pool.get(id_b).await.expect("browser B in pool");

    // Each mock is an independent browser process: tab registries must not
    // bleed across pooled instances. Note the mocks generate ids from the
    // same deterministic scheme, so isolation is asserted on the registry
    // contents (counts), not on id uniqueness.
    let tabs_a = browser_a.list_tabs().await.expect("list A");
    let tabs_b = browser_b.list_tabs().await.expect("list B");
    assert_eq!(tabs_a.len(), 1, "A starts with its seed page only");
    assert_eq!(tabs_b.len(), 1, "B starts with its seed page only");

    let tab_b = browser_b.new_tab("about:blank").await.expect("tab in B");
    let tabs_a = browser_a.list_tabs().await.expect("list A after B opened");
    assert_eq!(
        tabs_a.len(),
        1,
        "opening a tab in B must not add targets to A"
    );

    let tab_a = browser_a.new_tab("about:blank").await.expect("tab in A");
    let tabs_a = browser_a.list_tabs().await.expect("list A after A opened");
    let tabs_b = browser_b.list_tabs().await.expect("list B after A opened");
    assert_eq!(tabs_a.len(), 2, "A must see its own tab plus the seed page");
    assert_eq!(tabs_b.len(), 2, "B must see its own tab plus the seed page");
    assert!(
        tabs_b.iter().any(|t| t.target_id == tab_b.target_id()),
        "B's tab must be listed by B"
    );

    browser_a
        .close_tab(tab_a.target_id())
        .await
        .expect("close A's tab");

    let tabs_a = browser_a.list_tabs().await.expect("list A after close");
    let tabs_b = browser_b.list_tabs().await.expect("list B after A closed");
    assert_eq!(tabs_a.len(), 1, "A lost exactly its own tab");
    assert_eq!(
        tabs_b.len(),
        2,
        "closing A's tab must leave B's registry intact"
    );

    pool.close_all().await.expect("close pool");
}
