//! Ready sockets have never carried a chat. Each is transferred to exactly one
//! conversation; used conversations are never shared through this pool.
use super::session::{AccountConnection, MuseSession};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, Notify};

const MAX_AGE: Duration = Duration::from_secs(60);
#[derive(Default)]
pub(super) struct WarmConnections {
    sockets: Mutex<VecDeque<(Instant, AccountConnection)>>,
    filling: AtomicBool,
    available: Notify,
}
impl WarmConnections {
    pub async fn ready(&self, vm_id: &str) -> Option<AccountConnection> {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let available = self.available.notified();
                tokio::pin!(available);
                available.as_mut().enable();
                if let Some(connection) = self.take(vm_id).await {
                    return Some(connection);
                }
                if !self.filling.load(Ordering::Acquire) {
                    return None;
                }
                available.await;
            }
        })
        .await
        .ok()
        .flatten()
    }
    pub async fn take(&self, vm_id: &str) -> Option<AccountConnection> {
        let mut sockets = self.sockets.lock().await;
        while let Some((at, connection)) = sockets.pop_front() {
            if at.elapsed() < MAX_AGE && connection.vm_id.as_deref() == Some(vm_id) {
                return Some(connection);
            }
        }
        None
    }
}
impl MuseSession {
    pub fn schedule_warm(self: &Arc<Self>) {
        let target = self.performance.warm_connections;
        if target == 0 || self.warm.filling.swap(true, Ordering::AcqRel) {
            return;
        }
        let session = Arc::downgrade(self);
        tokio::spawn(async move {
            let Some(session) = session.upgrade() else {
                return;
            };
            // A foreground request may consume one socket while filling.
            // One extra attempt replenishes it without an unbounded loop.
            for _ in 0..target + 1 {
                {
                    let mut sockets = session.warm.sockets.lock().await;
                    sockets.retain(|(at, _)| at.elapsed() < MAX_AGE);
                    if sockets.len() >= target {
                        break;
                    }
                }
                let started = Instant::now();
                match session.connect_account().await {
                    Ok(connection) => {
                        session
                            .warm
                            .sockets
                            .lock()
                            .await
                            .push_back((Instant::now(), connection));
                        session.warm.available.notify_one();
                        tracing::info!(
                            elapsed_ms = started.elapsed().as_millis(),
                            "Prepared unused Muse socket in background"
                        );
                    }
                    Err(_) => {
                        tracing::debug!("Background socket preparation failed; foreground connection remains available");
                        break;
                    }
                }
            }
            session.warm.filling.store(false, Ordering::Release);
            session.warm.available.notify_waiters();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn ready_sockets_transfer_once_and_expired_or_foreign_vm_are_discarded() {
        let pool = WarmConnections::default();
        for (age, vm) in [
            (MAX_AGE + Duration::from_secs(1), "own"),
            (Duration::ZERO, "foreign"),
            (Duration::ZERO, "own"),
        ] {
            let mut connection = AccountConnection::default();
            connection.vm_id = Some(vm.into());
            pool.sockets
                .lock()
                .await
                .push_back((Instant::now() - age, connection));
        }
        assert_eq!(
            pool.take("own").await.unwrap().vm_id.as_deref(),
            Some("own")
        );
        assert!(pool.take("own").await.is_none());
    }
    #[tokio::test]
    async fn foreground_waits_for_an_inflight_ready_socket_without_losing_notification() {
        let pool = Arc::new(WarmConnections::default());
        pool.filling.store(true, Ordering::Release);
        let writer = pool.clone();
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            let mut connection = AccountConnection::default();
            connection.vm_id = Some("own".into());
            writer
                .sockets
                .lock()
                .await
                .push_back((Instant::now(), connection));
            writer.available.notify_one();
            writer.filling.store(false, Ordering::Release);
        });
        let connection = tokio::time::timeout(Duration::from_secs(1), pool.ready("own"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(connection.vm_id.as_deref(), Some("own"));
        assert!(pool.ready("own").await.is_none());
    }
}
