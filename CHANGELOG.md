# Changelog

All notable changes to `cdp-browser-lite` are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

[0.1.1]: https://github.com/raultov/cdp-browser-lite/releases/tag/v0.1.1
[0.1.0]: https://github.com/raultov/cdp-browser-lite/releases/tag/v0.1.0
