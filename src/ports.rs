use std::collections::HashSet;
use std::net::TcpListener;
use std::sync::{Arc, OnceLock};
use tokio::sync::Mutex;

use crate::error::BrowserError;

/// Process-wide registry of reserved ports.
///
/// Every [`PortAllocator`] shares this single set (and its lock). Without it,
/// two independent allocators — e.g. several `BrowserPool`s running in
/// parallel — could bind-probe the same free port and both reserve it, letting
/// two spawned browsers race to bind the same address. The mutex also
/// serialises the "bind to probe, drop, record" step in
/// [`PortAllocator::reserve_near`], closing that TOCTOU window within the
/// process.
static GLOBAL_RESERVED: OnceLock<Mutex<HashSet<u16>>> = OnceLock::new();

fn global_reserved() -> &'static Mutex<HashSet<u16>> {
    GLOBAL_RESERVED.get_or_init(Default::default)
}

/// Reserves ports from a shared, process-wide registry.
///
/// Instances are cheap and share the same underlying reservation set, so
/// distinct pools cannot double-reserve a port.
#[derive(Debug, Default)]
pub struct PortAllocator;

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
}

impl Drop for PortReservation {
    fn drop(&mut self) {
        // Drop can run in non-async contexts; use blocking_lock or spawn.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let port = self.port;
            handle.spawn(async move {
                let mut res = global_reserved().lock().await;
                res.remove(&port);
            });
        } else {
            let mut res = global_reserved().blocking_lock();
            res.remove(&self.port);
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
        Arc::new(Self)
    }

    pub async fn reserve_near<F>(
        self: &Arc<Self>,
        search: PortSearch<'_>,
        is_acceptable: F,
    ) -> Result<PortReservation, BrowserError>
    where
        F: Fn(u16) -> bool + Send,
    {
        let mut reserved = global_reserved().lock().await;
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
                return Ok(PortReservation { port: candidate });
            }
        }
        Err(BrowserError::PortConflict { port: search.base })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn pick_ephemeral_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        port
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
        let res = alloc
            .reserve_near(PortSearch::new("127.0.0.1", base, 5), |_| true)
            .await
            .unwrap();
        assert_eq!(res.port(), base);
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
        assert_eq!(res1.port(), base);
        assert_eq!(res2.port(), base + 1);
    }

    #[tokio::test]
    async fn given_reservation_dropped_when_reserving_then_port_is_reusable() {
        let alloc = PortAllocator::new();
        let base = pick_ephemeral_port();
        {
            let _res1 = alloc
                .reserve_near(PortSearch::new("127.0.0.1", base, 5), |_| true)
                .await
                .unwrap();
        }
        // Yield to allow Drop task to run
        tokio::time::sleep(Duration::from_millis(10)).await;

        let res2 = alloc
            .reserve_near(PortSearch::new("127.0.0.1", base, 5), |_| true)
            .await
            .unwrap();
        assert_eq!(res2.port(), base);
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
        assert_eq!(res.port(), base + 1);
    }

    #[tokio::test]
    async fn given_predicate_rejecting_port_when_reserving_then_skips_it() {
        let alloc = PortAllocator::new();
        let base = pick_ephemeral_port();
        let res = alloc
            .reserve_near(PortSearch::new("127.0.0.1", base, 5), |p| p != base)
            .await
            .unwrap();
        assert_eq!(res.port(), base + 1);
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
        let mut tasks = Vec::new();
        for _ in 0..32 {
            let alloc_clone = alloc.clone();
            tasks.push(tokio::spawn(async move {
                alloc_clone
                    .reserve_near(PortSearch::new("127.0.0.1", base, 100), |_| true)
                    .await
                    .unwrap()
            }));
        }
        let results = futures_util::future::join_all(tasks).await;
        let mut ports = HashSet::new();
        for res in results {
            let port = res.unwrap().port();
            assert!(!ports.contains(&port), "Duplicate port reserved: {}", port);
            ports.insert(port);
        }
        assert_eq!(ports.len(), 32);
    }

    #[tokio::test]
    async fn given_distinct_allocators_same_base_when_reserving_concurrently_then_ports_are_distinct()
     {
        // Regression test: two independent `PortAllocator`s (as used by
        // separate `BrowserPool`s running in parallel) must not both reserve
        // the same port from the same base. The reservation registry is
        // process-wide, so the first allocator to claim `base` forces the
        // second to skip it. This test was RED with a per-allocator reserved
        // set (0.2.2), where both allocators could claim `base`.
        let base = pick_ephemeral_port();
        let mut tasks = Vec::new();
        for _ in 0..16 {
            let alloc = PortAllocator::new();
            tasks.push(tokio::spawn(async move {
                alloc
                    .reserve_near(PortSearch::new("127.0.0.1", base, 200), |_| true)
                    .await
                    .unwrap()
            }));
        }
        let results = futures_util::future::join_all(tasks).await;
        let mut ports = HashSet::new();
        for res in results {
            let port = res.unwrap().port();
            assert!(!ports.contains(&port), "Duplicate port reserved: {}", port);
            ports.insert(port);
        }
        assert_eq!(ports.len(), 16);
    }
}
