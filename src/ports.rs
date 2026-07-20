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

    /// Reserves a contiguous run of `count` ports and returns the base plus the
    /// listeners holding them. Retries until it actually finds a free contiguous
    /// block, so the test never assumes `base+1`, `base+2`, ... happen to be free
    /// on the runner (the source of the earlier macOS flake) and never overflows
    /// `u16`.
    ///
    /// Unix-only: it relies on POSIX `bind` exclusivity (a second bind to a held
    /// port fails). Windows loopback does not reliably enforce that in-process,
    /// so the occupancy-based tests below are gated to `cfg(unix)`.
    #[cfg(unix)]
    fn reserve_contiguous(count: u16) -> (u16, Vec<TcpListener>) {
        for _ in 0..100 {
            let first = TcpListener::bind("127.0.0.1:0").unwrap();
            let base = first.local_addr().unwrap().port();
            if base.checked_add(count).is_none() {
                continue; // too close to u16::MAX for the range; try another base
            }
            let mut held = vec![first];
            for offset in 1..count {
                match TcpListener::bind(("127.0.0.1", base + offset)) {
                    Ok(listener) => held.push(listener),
                    Err(_) => break,
                }
            }
            if held.len() == count as usize {
                return (base, held);
            }
        }
        panic!("could not reserve {count} contiguous free ports after 100 attempts");
    }

    // Relies on POSIX bind semantics (freed ephemeral port is immediately
    // rebindable to the exact same number); Windows loopback does not guarantee
    // this, so keep it Unix-only.
    #[cfg(unix)]
    #[tokio::test]
    async fn given_base_free_when_searching_then_returns_base() {
        let base = pick_ephemeral_port();
        let result = find_free_port_near("127.0.0.1", base, 5)
            .await
            .expect("base should be free");
        assert_eq!(result, base);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn given_base_occupied_when_searching_then_returns_next_free() {
        // Reserve base..base+10 contiguously, then free everything from offset 3
        // onward: base..base+3 stay occupied, base+3..base+10 are free.
        let (base, mut held) = reserve_contiguous(10);
        held.truncate(3);

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

    #[cfg(unix)]
    #[tokio::test]
    async fn given_no_free_port_in_range_when_searching_then_port_conflict() {
        // Keep the whole base..base+10 range occupied so the search must fail.
        let (base, _occupy) = reserve_contiguous(10);

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
