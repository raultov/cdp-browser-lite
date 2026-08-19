use std::collections::HashMap;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Mutex as TokioMutex;

use crate::ports::{PortAllocator, PortReservation, PortSearch};
use crate::{Browser, BrowserConfig, BrowserError};

/// How many times an ephemeral open retries with a fresh port after a direct
/// (non-allocator) binder races the reserved port. Bounded to avoid spinning on
/// genuine port exhaustion.
const EPHEMERAL_OPEN_RETRIES: usize = 5;

/// Identifier for a browser owned by a [`BrowserPool`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BrowserId(u64);

impl BrowserId {
    #[doc(hidden)]
    pub fn from_u64_for_test(id: u64) -> Self {
        Self(id)
    }
}

/// Snapshot of a browser's resolved runtime metadata, captured at open time.
/// Reading from a pool entry never touches `Browser`'s internal mutex, which
/// keeps accessors race-free under contention.
#[derive(Debug, Clone)]
pub struct BrowserEntry {
    pub id: BrowserId,
    pub host: String,
    pub port: u16,
    pub profile_dir: Option<PathBuf>,
    pub managed: bool,
}

#[derive(Debug)]
struct PoolInner {
    allocator: Arc<PortAllocator>,
    browsers: TokioMutex<HashMap<BrowserId, Arc<Browser>>>,
    entries: TokioMutex<HashMap<BrowserId, BrowserEntry>>,
    /// Port reservations held by still-open browsers; released when the entry
    /// is closed or the pool is dropped.
    reservations: TokioMutex<HashMap<BrowserId, PortReservation>>,
    next_id: AtomicU64,
}

#[derive(Debug, Clone)]
pub struct BrowserPool {
    inner: Arc<PoolInner>,
}

impl Default for BrowserPool {
    fn default() -> Self {
        Self::new()
    }
}

impl BrowserPool {
    pub fn new() -> Self {
        Self::with_allocator(PortAllocator::new())
    }

    pub fn with_allocator(allocator: Arc<PortAllocator>) -> Self {
        Self {
            inner: Arc::new(PoolInner {
                allocator,
                browsers: TokioMutex::new(HashMap::new()),
                entries: TokioMutex::new(HashMap::new()),
                reservations: TokioMutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
            }),
        }
    }

    /// Opens a new browser in the pool.
    ///
    /// Port handling:
    /// - `config.port == 0`: the pool reserves a free port via its internal
    ///   [`PortAllocator`] (kernel-managed ephemeral range) and passes that
    ///   port to Chrome, so concurrent `open` calls cannot collide.
    /// - `config.port != 0`: the pool rejects `PortConflict` if the port is
    ///   already owned by another entry in this pool.
    ///
    /// The reservation lives until the entry is closed or the pool is dropped,
    /// preventing another `open` from picking the same port while Chrome binds.
    pub async fn open(&self, config: BrowserConfig) -> Result<BrowserId, BrowserError> {
        config.validate()?;

        let id = if config.port == 0 {
            self.open_ephemeral(config).await?
        } else {
            self.check_port_conflict(config.port).await?;
            self.open_reserved_port(config).await?
        };
        Ok(id)
    }

    /// Opens an ephemeral (`port == 0`) browser, retrying with a fresh
    /// reservation on the (rare) race where a process that binds ports without
    /// going through the allocator — e.g. a mock devtools server in tests —
    /// grabs the reserved port between the allocator's probe and Chrome's bind.
    /// The reservation registry is process-wide, so this only fires against
    /// direct `bind(0)` callers, not against other pools.
    async fn open_ephemeral(&self, config: BrowserConfig) -> Result<BrowserId, BrowserError> {
        let mut last_error: Option<BrowserError> = None;
        for _ in 0..=EPHEMERAL_OPEN_RETRIES {
            let mut attempt = config.clone();
            let reservation = self.reserve_ephemeral_port().await?;
            attempt.port = reservation.port();

            let browser =
                match Browser::ensure_with_allocator(attempt, self.inner.allocator.clone()).await {
                    Ok(b) => b,
                    Err(e) => {
                        drop(reservation);
                        last_error = Some(e);
                        // Retry with a fresh ephemeral reservation. A concurrent
                        // direct binder may have won the race for the first port.
                        continue;
                    }
                };

            return self.register(browser, Some(reservation)).await;
        }
        Err(last_error.unwrap_or(BrowserError::PortConflict { port: 0 }))
    }

    /// Registers a freshly opened browser in the pool and returns its id.
    async fn register(
        &self,
        browser: Browser,
        reservation: Option<PortReservation>,
    ) -> Result<BrowserId, BrowserError> {
        let id = BrowserId(self.inner.next_id.fetch_add(1, Ordering::Relaxed));
        let (host, port) = browser.debug_address().await;
        let profile_dir = browser.profile_dir().await;
        let managed = browser.is_managed().await;

        let entry = BrowserEntry {
            id,
            host,
            port,
            profile_dir,
            managed,
        };

        let mut entries = self.inner.entries.lock().await;
        let mut browsers = self.inner.browsers.lock().await;
        let mut reservations = self.inner.reservations.lock().await;

        if let Some(res) = reservation {
            reservations.insert(id, res);
        }
        entries.insert(id, entry);
        browsers.insert(id, Arc::new(browser));

        Ok(id)
    }

    /// Opens a browser on an explicitly configured (non-zero) port.
    async fn open_reserved_port(&self, config: BrowserConfig) -> Result<BrowserId, BrowserError> {
        let browser = Browser::ensure_with_allocator(config, self.inner.allocator.clone()).await?;
        self.register(browser, None).await
    }

    pub async fn get(&self, id: BrowserId) -> Option<Arc<Browser>> {
        self.inner.browsers.lock().await.get(&id).cloned()
    }

    pub async fn entry(&self, id: BrowserId) -> Option<BrowserEntry> {
        self.inner.entries.lock().await.get(&id).cloned()
    }

    pub async fn entries(&self) -> Vec<BrowserEntry> {
        self.inner.entries.lock().await.values().cloned().collect()
    }

    pub async fn len(&self) -> usize {
        self.inner.entries.lock().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.inner.entries.lock().await.is_empty()
    }

    pub async fn close(&self, id: BrowserId) -> Result<(), BrowserError> {
        self.inner.entries.lock().await.remove(&id);
        self.inner.reservations.lock().await.remove(&id);
        if let Some(browser) = self.inner.browsers.lock().await.remove(&id) {
            browser.stop().await?;
        }
        Ok(())
    }

    pub async fn close_all(&self) -> Result<(), BrowserError> {
        let mut browsers = self.inner.browsers.lock().await;
        self.inner.entries.lock().await.clear();
        self.inner.reservations.lock().await.clear();
        for (_, browser) in browsers.drain() {
            browser.stop().await?;
        }
        Ok(())
    }

    async fn check_port_conflict(&self, port: u16) -> Result<(), BrowserError> {
        let entries = self.inner.entries.lock().await;
        for entry in entries.values() {
            if entry.port == port {
                return Err(BrowserError::PortConflict { port });
            }
        }
        Ok(())
    }

    /// Reserves a free kernel-ephemeral port via the pool's allocator,
    /// excluding ports already owned by live pool entries.
    async fn reserve_ephemeral_port(&self) -> Result<PortReservation, BrowserError> {
        let base = kernel_ephemeral_port();
        let taken: Vec<u16> = {
            let entries = self.inner.entries.lock().await;
            entries.values().map(|e| e.port).collect()
        };
        self.inner
            .allocator
            .reserve_near(
                PortSearch::new("127.0.0.1", base, u16::MAX.wrapping_sub(base)),
                move |p| !taken.contains(&p),
            )
            .await
    }
}

/// Returns a port the kernel just assigned for an ephemeral bind. Used as
/// the starting point for pool-side port reservations so they land in the
/// range Chrome is most likely to accept.
fn kernel_ephemeral_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}
