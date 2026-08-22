# Changelog

All notable changes to `cdp-browser-lite` are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.2] - 2026-08-22

### Added
- `test-support` cargo feature: re-exports the in-process DevTools HTTP/WebSocket
  mocks under `cdp_browser_lite::test_support::mock_devtools` so downstream
  crates can drive the library without re-implementing them. Pulls in
  `tokio-tungstenite` and `futures-util` as transitive dependencies only when the
  feature is enabled, so the default build stays slim.
- `MockBehavior`, `MockChrome`, `MockDevTools`, `MockWsBehavior` are now the
  documented public test-support API. The crate's own integration tests have
  been migrated to the new path (their `[[test]]` entries are gated by
  `required-features = ["test-support"]`).
- Orphan sweep for ephemeral profiles: on every ephemeral `Profile::prepare`,
  directories under the system temp dir matching the `cdp-browser-lite-`
  prefix that are older than 24 h and whose `SingletonLock` owner PID is dead
  (Unix) are removed best-effort. Prevents unbounded accumulation of profile
  dirs when Chrome is killed abruptly (SIGKILL) without cleanup.
- `--hide-crash-restore-bubble` added to the base launch flags, alongside the
  existing `--disable-session-crashed-bubble`, to suppress the "Chrome didn't
  shut down correctly" restore bubble on all Chrome versions.

### Fixed
- `patch_preferences` wrote `exit_type`/`exited_cleanly` at the root of
  `Default/Preferences`, where Chrome never reads them (the real exit state
  lives under `profile.exit_type`). It now patches `profile.exit_type` to
  `Normal`, creating or replacing the `profile` object as needed, so
  persistent profiles no longer trigger the restore bubble after an unclean
  shutdown. Root-level writes were removed as dead.

## [0.3.1] - 2026-08-22

### Fixed
- The `given_predicate_rejecting_port_when_reserving_then_skips_it` test in
  `ports` asserted the exact `base + 1` port, which was flaky on Windows and
  macOS CI because the kernel can briefly hold the next ephemeral port after
  `pick_ephemeral_port` releases its own. The test now asserts the actual
  contract: the predicate-rejected port is skipped, and the returned port lies
  within the search range. No production-code behaviour changed.

## [0.3.0] - 2026-08-22

### Added
- `Browser::browser_client()`: a cached, liveness-checked connection to the
  browser-level endpoint (`/json/version`), the foundation for multi-tab driving.
- Five tab delegations on `Browser` — `new_tab`, `attach_tab`, `attach_to_all_tabs`,
  `list_tabs`, `close_tab` — all routed over the shared browser-level connection.
- Re-exports of `Tab`, `TargetInfo` and `BrowserClient` for convenient tab work.
- Full multi-tab support in the devtools mock, plus fidelity and lifecycle test
  coverage (including a real-Chrome E2E proving per-session routing).

### Changed
- Internal connection management refactored around `ensure_process_ready` /
  `relaunch`; both the page-level and browser-level caches are invalidated on
  stop, restart and relaunch.

### Notes
- `Browser::client()` is unchanged and fully backward compatible.
- Calling both `client()` and `browser_client()` opens two WebSocket connections
  (page-level vs browser-level endpoints). This is intentional.

## [0.2.4] - 2026-08-19

### Fixed
- Reverted the `PortAllocator` reservation set to **per-instance** (undoing the 0.2.3
  process-wide change).  The shared registry made the `ports` unit tests collide under
  parallel execution on macOS/Windows — `pick_ephemeral_port()` hands out nearly-contiguous
  bases there (e.g. 49180–49187), and with one global reservation set concurrent tests could
  exhaust each other's small search ranges, failing with `PortConflict`.  Per-instance sets
  restore the original, platform-proven test behaviour.
- The `BrowserPool` ephemeral-open retry introduced in 0.2.3 is **retained**: a bounded
  retry with a fresh reservation absorbs the residual port race (cross-pool double-reservation
  and direct `bind(0)` binders) without a shared registry.  Pool flakiness remains resolved.

## [0.2.3] - 2026-08-19

### Fixed
- `ProfileMode::managed_lock_exists` now uses `std::fs::symlink_metadata` instead of
  `Path::exists` to detect `SingletonLock`.  On Chrome >= 151 `SingletonLock` is written
  as a **dangling symlink** (the target `<hostname>-<pid>` is never created); `Path::exists`
  follows the symlink and returned `false`, so `LaunchMode::Auto` never recognised a live
  managed instance and fell through to `AttachAt` instead of `LaunchAt`.  This was the root
  cause of the B3 failure in `chrome-debug-mcp`.  `symlink_metadata` does not follow the
  symlink, so it returns `true` for both plain files (fake Chrome / Chrome < 151) and
  dangling symlinks (Chrome >= 151).
- `BrowserPool::open` with an ephemeral (`port == 0`) config now retries with a fresh
  reservation (bounded, `EPHEMERAL_OPEN_RETRIES = 5`) when the reserved port is lost to a
  concurrent binder between the allocator's probe and Chrome's bind.  This absorbs both the
  cross-pool double-reservation and the direct `bind(0)` race that caused intermittent
  `PortConflict` failures (a `LaunchMode::Auto`/`BrowserPool` flake on 0.2.2).  Fixed-port
  opens still fail fast with `PortConflict` as before.
- **Reverted in 0.2.4:** 0.2.3 briefly made the `PortAllocator` reservation set process-wide;
  that caused the `ports` unit tests to collide under parallel execution on macOS/Windows
  (they pick contiguous ephemeral bases), so it was reverted to per-instance sets.

### Tested
- Added `serve_singleton_symlink` mode to `src/bin/fake_chrome_helper.rs`: creates
  `SingletonLock` as a dangling symlink (target `nonexistent-target` never exists),
  faithfully replicating Chrome >= 151 behaviour.
- Added `FakeMode::ServeSingletonSymlink` to `tests/support/fake_chrome.rs`.
- Added three unit tests in `src/config.rs` for `managed_lock_exists`:
  - `given_symlink_lock_when_managed_lock_exists_then_true` (RED on 0.2.2, green after fix)
  - `given_plain_file_lock_when_managed_lock_exists_then_true` (regression guard)
  - `given_no_lock_when_managed_lock_exists_then_false`
- Added three integration tests in `tests/profile_per_port.rs` mirroring the B3 scenarios
  with `ServeSingletonSymlink`: different port, different profile dir, lock preserved.
- Added `given_real_chrome_on_configured_port_when_second_manager_ensures_then_uses_different_port_and_profile`
  E2E test (`#[ignore]`) in `tests/e2e_real_chrome.rs` covering the full B3 scenario
  on a machine with real Chrome >= 151.
- Introduced a `tokio::sync::Mutex` guard (`ENV_LOCK`) in `tests/profile_per_port.rs` to
  serialize tests that mutate `FAKE_CHROME_MODE` in the process environment, preventing
  spurious failures from concurrent test execution.

## [0.2.2] - 2026-08-19

### Fixed
- `probe::is_chrome_cdp` now sends its `/json/version` readiness probe over
  **HTTP/1.1** instead of HTTP/1.0.  Chrome >= 151 silently drops HTTP/1.0
  requests (returns an empty response), causing `is_chrome_cdp` to always
  return `false` even for a healthy DevTools endpoint.  The fix restores
  correct attach-detection on Chrome >= 151 (`LaunchMode::Auto` B2/B3 paths).

### Tested
- Added `MockBehavior::IgnoresHttp10` to `tests/support/mock_devtools.rs`:
  the mock drops the connection with no response when it receives an HTTP/1.0
  request, faithfully replicating Chrome 151 behaviour.
- Added `given_chrome_ignores_http10_when_probing_then_true` regression test
  in `tests/probe_tests.rs` (RED on 0.2.1, GREEN after the HTTP/1.1 fix).

## [0.2.1] - 2026-08-19

### Fixed
- Fixed port race condition on the `LaunchMode::Auto` path by making the `PortAllocator` a process-wide `OnceLock` singleton and holding `PortReservation` across the Chrome spawn.
- Replaced non-compliant `#[allow(...)]` attributes with `#[expect(..., reason = "...")]` according to new repository conventions.
- Translated the Spanish doc comment on `probe::is_chrome_cdp` to English.

### Changed
- Refined `BrowserPool` termination wording in documentation to indicate best-effort fallback on drop, promoting `close_all().await` as the deterministic path.

### Tested
- Added full integration test coverage for `PersistentPerPort` profiles and `BrowserPool` management.
- Implemented `serve_singleton` mode in `fake_chrome_helper` to properly test Chrome's single-instance delegation behaviors.

## [0.2.0] - 2026-08-19

### Added
- `BrowserPool` for managing multiple Chrome processes concurrently, with automatic port tracking. Managed instances are terminated deterministically via `close_all().await`, with a best-effort fallback upon drop.
- `PortAllocator` replaces the previous port search mechanism, using deterministic reservations to eliminate cross-process bind races.
- `ProfileMode::PersistentPerPort` dynamically derives the profile directory from the resolved port, enabling multi-instance `LaunchMode::Auto`.
- Added `Browser::profile_dir` async accessor to retrieve the resolved profile directory path.

### Changed
- **Breaking:** `ProfileMode` is now `#[non_exhaustive]` to allow future variants.
- **Breaking:** `Browser::debug_address`, `is_alive`, `is_managed`, and `pid` are now `async` methods to prevent silent failures under lock contention.
- **Breaking:** `Profile::prepare` now requires a `port` argument to resolve dynamic profiles.
- `LaunchMode::Auto` logic has been reworked: it now properly avoids mutating locks of live processes (fixing directory corruption/races) and delays profile creation until the port is finalized (fixing temporal tempdir leaks).
- `ports::find_free_port_near` has been removed in favor of `PortAllocator`.


## [0.1.1] - 2026-07-20

### Fixed
- Windows build under `clippy -D warnings`: the `grace` parameter and the
  `TERMINATE_POLL` constant in `ChromeProcess::terminate` are only used by the
  Unix SIGTERM grace loop, so they are now gated to `cfg(unix)` and no longer
  trip the dead-code lint on non-Unix targets.
- Flaky `ports` unit tests on CI: they no longer assume that ports contiguous to
  an ephemeral one are free. A `reserve_contiguous` helper binds a real
  contiguous block (with `u16` overflow guard), making the search tests
  deterministic across runners. The occupancy-based tests, which depend on POSIX
  `bind` exclusivity that Windows loopback does not guarantee in-process, are
  gated to `cfg(unix)`; the platform-agnostic `tries == 0` case stays universal.

### Changed
- Release workflow now creates the GitHub release once in a dedicated
  `create-release` job that the build matrix `needs`, instead of letting the
  four parallel target jobs race to create the same tag (which failed with
  `already_exists` on `tag_name`). Build jobs only upload their assets.

## [0.1.0] - 2026-07-20

### Added
- `Browser` lifecycle facade with three launch modes: `Auto` (attach-or-launch),
  `LaunchNew` (always spawn), and `AttachOnly` (connect to existing remote
  Chrome).
- `BrowserConfig` builder with flat setters for headless, proxy, window size,
  user agent, timeouts, `keep_alive_on_drop`, `auto_relaunch`, `no_sandbox`, and
  passthrough `arg` / `args`. Built-in validation rejects port 0 for `Auto` /
  `AttachOnly` and rejects remote hosts for `Auto` / `LaunchNew`.
- `ProfileMode` covering ephemeral (auto-cleaned tempdir), persistent (with
  `Preferences` patching to skip the crash bubble), and `UserDefault`.
- Portable Chrome process spawn (`tokio::process` + `kill_on_drop`) and
  graceful termination: SIGTERM → grace → SIGKILL on unix (via `nix`), direct
  kill on Windows. `Drop` is a best-effort safety net.
- Cross-platform Chrome executable discovery (`CHROME_PATH` first, then
  per-OS candidate lists). `is_chrome_cdp` probe tolerates modern Chrome
  keep-alive behaviour. `find_free_port_near` walks a bounded candidate range
  on the blocking pool.
- Typed `BrowserError` model built on `thiserror`, with `From<CdpError>` and
  `From<io::Error>` impls and actionable messages (e.g. mentions `CHROME_PATH`
  on `ExecutableNotFound`).
- Re-export of the full `cdp-lite` public surface (`CdpClient`, `CdpError`,
  `CdpResult`, `EventFilter`, `NoParams`, `WsCommand`, `WsResponse`) from the
  crate root.
- Cached and reusable CDP client: `Browser::client` returns the same client on
  repeat calls, performs a liveness ping, transparently reconnects on
  `Disconnected` / `Timeout`, and optionally auto-relaunches a dead managed
  process when `auto_relaunch(true)` is set.
- Three runnable examples: `simple`, `runtime_usage` (navigate + scrape +
  screenshot), and `filter_domains` (Network.* + Page.* event aggregation).
- Integration test suite covering every BDD scenario from PLAN §5: lifecycle,
  drop semantics, multi-instance concurrency, client access + reconnect,
  probe behaviour, process spawn + termination, and a `--ignored` real-Chrome
  smoke test.
- GitHub Actions CI matrix (fmt, clippy with `-D warnings`, build, test) on
  Linux, macOS and Windows, plus per-target release binaries for every
  supported platform, plus an E2E job that installs Chrome and runs the
  ignored tests.

[0.3.2]: https://github.com/raultov/cdp-browser-lite/releases/tag/v0.3.2
[0.3.1]: https://github.com/raultov/cdp-browser-lite/releases/tag/v0.3.1
[0.3.0]: https://github.com/raultov/cdp-browser-lite/releases/tag/v0.3.0
[0.2.4]: https://github.com/raultov/cdp-browser-lite/releases/tag/v0.2.4
[0.2.3]: https://github.com/raultov/cdp-browser-lite/releases/tag/v0.2.3
[0.2.2]: https://github.com/raultov/cdp-browser-lite/releases/tag/v0.2.2
[0.1.1]: https://github.com/raultov/cdp-browser-lite/releases/tag/v0.1.1
[0.1.0]: https://github.com/raultov/cdp-browser-lite/releases/tag/v0.1.0
