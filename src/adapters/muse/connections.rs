//! Cache isolated connection owners by conversation. Each owner has exactly one
//! Noise reader; independent owners never share nonce or receive state.
use super::session::AccountConnection;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;
const MAX_CACHED: usize = 32;

#[derive(Default)]
pub(super) struct ConversationConnections {
    slots: Mutex<HashMap<String, Arc<Mutex<AccountConnection>>>>,
}

impl ConversationConnections {
    pub async fn get(&self, id: &str) -> Result<Arc<Mutex<AccountConnection>>, &'static str> {
        let mut slots = self.slots.lock().await;
        if let Some(slot) = slots.get(id) {
            return Ok(Arc::clone(slot));
        }
        if slots.len() >= MAX_CACHED {
            let idle = slots
                .iter()
                .find(|(_, slot)| Arc::strong_count(slot) == 1)
                .map(|(id, _)| id.clone());
            if let Some(id) = idle {
                slots.remove(&id);
            } else {
                return Err("Muse connection pool is busy");
            }
        }
        let slot = Arc::new(Mutex::new(AccountConnection::default()));
        slots.insert(id.to_owned(), Arc::clone(&slot));
        Ok(slot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn same_conversation_serializes_but_another_conversation_does_not_wait() {
        let pool = ConversationConnections::default();
        let a = pool.get("a").await.unwrap();
        let a2 = pool.get("a").await.unwrap();
        let b = pool.get("b").await.unwrap();
        assert!(Arc::ptr_eq(&a, &a2));
        assert!(!Arc::ptr_eq(&a, &b));
        let _guard = a.lock().await;
        assert!(a2.try_lock().is_err());
        assert!(b.try_lock().is_ok());
    }
    #[tokio::test]
    async fn cache_never_evicts_a_connection_that_has_active_or_waiting_owners() {
        let pool = ConversationConnections::default();
        let active = pool.get("active").await.unwrap();
        for n in 0..MAX_CACHED * 2 {
            drop(pool.get(&format!("idle-{n}")).await.unwrap());
        }
        assert!(Arc::ptr_eq(&active, &pool.get("active").await.unwrap()));
        assert!(pool.slots.lock().await.len() <= MAX_CACHED);
    }
}
