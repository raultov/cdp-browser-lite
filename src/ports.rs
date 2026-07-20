use std::net::TcpListener;

use crate::error::BrowserError;

/// Finds a free port in the range `[base, base + tries)`.
///
/// Kept for parity with the `find_new_port` method from chrome-debug-mcp; the
/// `LaunchNew` mode prefers ephemeral ports (`port = 0`) and
/// `find_free_port_near` is only used in `LaunchMode::Auto` when the fixed port
/// is occupied by another managed instance.
pub(crate) async fn find_free_port_near(
    host: &str,
    base: u16,
    tries: u16,
) -> Result<u16, BrowserError> {
    let host_owned = host.to_string();
    tokio::task::spawn_blocking(move || find_free_port_near_blocking(&host_owned, base, tries))
        .await
        .map_err(|e| {
            BrowserError::Io(std::io::Error::other(format!(
                "find_free_port_near task panicked: {e}"
            )))
        })?
}

fn find_free_port_near_blocking(host: &str, base: u16, tries: u16) -> Result<u16, BrowserError> {
    for offset in 0..tries {
        let candidate = base.wrapping_add(offset);
        if let Ok(listener) = TcpListener::bind((host, candidate)) {
            drop(listener);
            return Ok(candidate);
        }
    }
    Err(BrowserError::PortConflict { port: base })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pick_ephemeral_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        port
    }

    #[tokio::test]
    async fn given_base_free_when_searching_then_returns_base() {
        let base = pick_ephemeral_port();
        let result = find_free_port_near("127.0.0.1", base, 5)
            .await
            .expect("base should be free");
        assert_eq!(result, base);
    }

    #[tokio::test]
    async fn given_base_occupied_when_searching_then_returns_next_free() {
        let base = pick_ephemeral_port();
        let _occupy: Vec<TcpListener> = (0..3)
            .map(|i| TcpListener::bind(("127.0.0.1", base + i)).unwrap())
            .collect();

        let result = find_free_port_near("127.0.0.1", base, 10)
            .await
            .expect("a port in range should be free");

        assert!(
            result >= base + 3,
            "must skip occupied base..base+2, got {result} (base={base})"
        );
        assert!(
            result < base + 10,
            "must not exceed the search range, got {result} (base={base}, tries=10)"
        );
        assert_ne!(result, base, "must not return the occupied base");
    }

    #[tokio::test]
    async fn given_no_free_port_in_range_when_searching_then_port_conflict() {
        let base = pick_ephemeral_port();
        let _occupy: Vec<TcpListener> = (0..10)
            .map(|i| TcpListener::bind(("127.0.0.1", base + i)).unwrap())
            .collect();

        let result = find_free_port_near("127.0.0.1", base, 10).await;
        match result {
            Err(BrowserError::PortConflict { port }) => {
                assert_eq!(port, base, "PortConflict must carry the requested base");
            }
            other => panic!("expected PortConflict, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn given_tries_zero_when_searching_then_port_conflict() {
        let base = pick_ephemeral_port();
        let result = find_free_port_near("127.0.0.1", base, 0).await;
        assert!(
            matches!(result, Err(BrowserError::PortConflict { port }) if port == base),
            "tries=0 must yield PortConflict immediately, got {result:?}"
        );
    }
}
