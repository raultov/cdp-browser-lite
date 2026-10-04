use std::collections::HashSet;
use std::net::TcpListener;
use std::sync::{Arc, Weak};
use tokio::sync::Mutex;

use crate::error::BrowserError;

#[derive(Debug, Default)]
pub struct PortAllocator {
    reserved: Mutex<HashSet<u16>>,
}

/// Search parameters for [`PortAllocator::reserve_near`].
#[derive(Debug, Clone, Copy)]
pub struct PortSearch<'a> {
    pub host: &'a str,
    pub base: u16,
    pub tries: u16,
}

impl<'a> PortSearch<'a> {
    pub fn new(host: &'a str, base: u16, tries: u16) -> Self {
        Self { host, base, tries }
    }
}

#[derive(Debug)]
pub struct PortReservation {
    port: u16,
    allocator: Weak<PortAllocator>,
}

impl Drop for PortReservation {
    fn drop(&mut self) {
        if let Some(alloc) = self.allocator.upgrade() {
            // Drop can run in non-async contexts; use blocking_lock or spawn
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let alloc_clone = alloc.clone();
                let port = self.port;
                handle.spawn(async move {
                    let mut res = alloc_clone.reserved.lock().await;
                    res.remove(&port);
                });
            } else {
                let mut res = alloc.reserved.blocking_lock();
                res.remove(&self.port);
            }
        }
    }
}

impl PortReservation {
    pub fn port(&self) -> u16 {
        self.port
    }
}

impl PortAllocator {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub async fn reserve_near<F>(
        self: &Arc<Self>,
        search: PortSearch<'_>,
        is_acceptable: F,
    ) -> Result<PortReservation, BrowserError>
    where
        F: Fn(u16) -> bool + Send,
    {
        let mut reserved = self.reserved.lock().await;
        for offset in 0..search.tries {
            let candidate = search.base.wrapping_add(offset);

            if reserved.contains(&candidate) {
                continue;
            }

            if !is_acceptable(candidate) {
                continue;
            }

            if let Ok(listener) = TcpListener::bind((search.host, candidate)) {
                drop(listener);
                reserved.insert(candidate);
                return Ok(PortReservation {
                    port: candidate,
                    allocator: Arc::downgrade(self),
                });
            }
        }
        Err(BrowserError::PortConflict { port: search.base })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Returns an ephemeral port that is free to bind at this instant.
    ///
    /// The socket is **not** held: `reserve_near` binds candidates itself, so
    /// the port is released again immediately. That leaves a window in which a
    /// concurrent test, or the OS's own dynamic-port bookkeeping (Windows holds
    /// a just-released ephemeral port briefly), can claim the port before the
    /// allocator gets to it. Rebinding once here at least guarantees the port
    /// was usable when it was handed out, which closes the commonest failure
    /// mode seen on Windows CI.
    ///
    /// Because the window cannot be closed from here, tests must assert on the
    /// allocator's contract (in range, never reused) and never on an exact port
    /// number. This is the same reasoning as commit `672100b`, which relaxed the
    /// predicate-rejection test after it flaked on macOS CI.
    fn pick_ephemeral_port() -> u16 {
        for _ in 0..100 {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            drop(listener);
            if TcpListener::bind(("127.0.0.1", port)).is_ok() {
                return port;
            }
        }
        panic!("could not find a rebindable ephemeral port after 100 attempts");
    }

    /// Whether `port` is one of the `tries` candidates `reserve_near` would
    /// consider, using the same wrapping arithmetic the allocator does.
    fn in_search_range(base: u16, tries: u16, port: u16) -> bool {
        (0..tries).any(|offset| base.wrapping_add(offset) == port)
    }

    #[cfg(unix)]
    fn reserve_contiguous(count: u16) -> (u16, Vec<TcpListener>) {
        for _ in 0..100 {
            let first = TcpListener::bind("127.0.0.1:0").unwrap();
            let base = first.local_addr().unwrap().port();
            if base.checked_add(count).is_none() {
                continue;
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

    #[tokio::test]
    async fn given_free_base_when_reserving_then_returns_base() {
        let alloc = PortAllocator::new();
        let base = pick_ephemeral_port();
        let tried = Arc::new(std::sync::Mutex::new(Vec::new()));
        let tried_in = Arc::clone(&tried);
        let res = alloc
            .reserve_near(PortSearch::new("127.0.0.1", base, 5), move |p| {
                tried_in.lock().unwrap().push(p);
                true
            })
            .await
            .unwrap();

        // The allocator must consider candidates in ascending offset order and
        // stop at the first it can claim, which is what makes it return `base`
        // when `base` is free. Asserting `res.port() == base` exactly is not
        // possible here: `pick_ephemeral_port` cannot hold the socket, so the
        // OS or a concurrent test may claim `base` in the gap and the allocator
        // then correctly moves to the next candidate.
        assert_eq!(
            tried.lock().unwrap().first().copied(),
            Some(base),
            "the base candidate must be tried first"
        );
        assert!(
            in_search_range(base, 5, res.port()),
            "reservation must be a candidate of the search range, got {}",
            res.port()
        );
    }

    #[tokio::test]
    async fn given_reserved_port_when_reserving_again_then_skips_it() {
        let alloc = PortAllocator::new();
        let base = pick_ephemeral_port();
        let res1 = alloc
            .reserve_near(PortSearch::new("127.0.0.1", base, 5), |_| true)
            .await
            .unwrap();
        let res2 = alloc
            .reserve_near(PortSearch::new("127.0.0.1", base, 5), |_| true)
            .await
            .unwrap();

        // The contract: a port the allocator has reserved is never handed out
        // twice. Asserting exact values (`base`, then `base + 1`) is fragile:
        // `pick_ephemeral_port` releases the socket before the allocator binds
        // it, so the OS or a concurrent test can take `base` or `base + 1` and
        // legitimately push the result to the next free slot. That is what broke
        // on Windows CI (55899 returned where 55898 was expected).
        assert_ne!(
            res2.port(),
            res1.port(),
            "a reserved port must never be handed out twice"
        );
        assert!(
            in_search_range(base, 5, res1.port()),
            "first reservation must be a candidate of the search range, got {}",
            res1.port()
        );
        assert!(
            in_search_range(base, 5, res2.port()),
            "second reservation must be a candidate of the search range, got {}",
            res2.port()
        );
    }

    #[tokio::test]
    async fn given_reservation_dropped_when_reserving_then_port_is_reusable() {
        let alloc = PortAllocator::new();
        let base = pick_ephemeral_port();
        let first_port = {
            let res1 = alloc
                .reserve_near(PortSearch::new("127.0.0.1", base, 5), |_| true)
                .await
                .unwrap();
            res1.port()
        };
        // Yield to allow Drop task to run
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Accept nothing but the released port. If Drop had failed to give it
        // back, the allocator would still exclude it and, with no other
        // acceptable candidate, report a PortConflict - so this asserts exact
        // reusability without betting on which port `pick_ephemeral_port`
        // happened to hand out.
        let res2 = alloc
            .reserve_near(PortSearch::new("127.0.0.1", base, 5), |p| p == first_port)
            .await
            .unwrap_or_else(|e| {
                panic!(
                    "a dropped reservation must be handed out again, got {e:?} \
                     (the port may have been claimed by the OS during the sleep)"
                )
            });

        assert_eq!(
            res2.port(),
            first_port,
            "a dropped reservation must be handed out again"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn given_occupied_port_when_reserving_then_skips_it() {
        let alloc = PortAllocator::new();
        let (base, mut held) = reserve_contiguous(3);
        held.truncate(1); // Keep `base` occupied
        let res = alloc
            .reserve_near(PortSearch::new("127.0.0.1", base, 5), |_| true)
            .await
            .unwrap();

        // `base` is held by a live listener, so it must be skipped. Do not
        // assert the exact `base + 1`: the contiguous block above is released
        // except for `base`, so a concurrent test can claim `base + 1` in
        // between and push the result further along the range.
        assert_ne!(
            res.port(),
            base,
            "an occupied port must never be handed out"
        );
        assert!(
            in_search_range(base, 5, res.port()),
            "reservation must be a candidate of the search range, got {}",
            res.port()
        );
    }

    #[tokio::test]
    async fn given_predicate_rejecting_port_when_reserving_then_skips_it() {
        let alloc = PortAllocator::new();
        let base = pick_ephemeral_port();
        let res = alloc
            .reserve_near(PortSearch::new("127.0.0.1", base, 5), |p| p != base)
            .await
            .unwrap();
        // The contract: the rejected port is skipped, the returned port lies
        // within the search range. Asserting an exact `base + 1` is fragile on
        // kernels that briefly hold the next ephemeral port in TIME_WAIT
        // (observed on macOS CI).
        assert_ne!(res.port(), base, "predicate must reject base");
        assert!(
            in_search_range(base, 5, res.port()),
            "reserved port must be within the search range, got {}",
            res.port()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn given_no_acceptable_port_in_range_when_reserving_then_port_conflict() {
        let alloc = PortAllocator::new();
        let (base, _held) = reserve_contiguous(2);
        let err = alloc
            .reserve_near(PortSearch::new("127.0.0.1", base, 2), |_| true)
            .await
            .unwrap_err();
        match err {
            BrowserError::PortConflict { port } => assert_eq!(port, base),
            _ => panic!("Expected PortConflict"),
        }
    }

    #[tokio::test]
    async fn given_tries_zero_when_reserving_then_port_conflict() {
        let alloc = PortAllocator::new();
        let base = pick_ephemeral_port();
        let err = alloc
            .reserve_near(PortSearch::new("127.0.0.1", base, 0), |_| true)
            .await
            .unwrap_err();
        match err {
            BrowserError::PortConflict { port } => assert_eq!(port, base),
            _ => panic!("Expected PortConflict"),
        }
    }

    #[tokio::test]
    async fn given_many_concurrent_reservations_when_awaited_then_all_ports_are_distinct() {
        let alloc = PortAllocator::new();
        let base = pick_ephemeral_port();
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..32 {
            let alloc_clone = alloc.clone();
            tasks.spawn(async move {
                alloc_clone
                    .reserve_near(PortSearch::new("127.0.0.1", base, 100), |_| true)
                    .await
                    .unwrap()
            });
        }
        let mut ports = HashSet::new();
        while let Some(res) = tasks.join_next().await {
            let port = res.unwrap().port();
            assert!(!ports.contains(&port), "Duplicate port reserved: {}", port);
            ports.insert(port);
        }
        assert_eq!(ports.len(), 32);
    }
}
