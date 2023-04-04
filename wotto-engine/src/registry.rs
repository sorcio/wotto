use std::borrow::Borrow;
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

pub(crate) type RegistryEntry<V> = Arc<RwLock<Option<V>>>;
pub(crate) type ValueRef<V> = OwnedRwLockReadGuard<Option<V>>;
pub(crate) type ValueRefMut<V> = OwnedRwLockWriteGuard<Option<V>>;

pub(crate) struct Registry<K, V> {
    entries: Mutex<HashMap<K, RegistryEntry<V>>>,
}

impl<K, V> Registry<K, V>
where
    K: Hash + Eq,
{
    async fn entry_or_default(&self, key: K) -> ValueRefMut<V> {
        let entry = {
            let mut map = self.entries.lock();
            map.entry(key)
                .or_insert_with(RegistryEntry::default)
                .clone()
        };
        entry.write_owned().await
    }

    async fn entry<Q>(&self, key: &Q) -> Option<ValueRef<V>>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let entry = {
            let map = self.entries.lock();
            map.get(key)?.clone()
        };
        Some(entry.read_owned().await)
    }

    async fn entry_mut<Q>(&self, key: &Q) -> Option<ValueRefMut<V>>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let entry = {
            let map = self.entries.lock();
            map.get(key)?.clone()
        };
        Some(entry.write_owned().await)
    }

    pub(crate) async fn lock_entry_mut(&self, key: K) -> ValueRefMut<V> {
        self.entry_or_default(key).await
    }

    /// Wait until no writers are touching the entry, if it exists; otherwise
    /// return immediately.
    pub(crate) async fn wait_entry<Q>(&self, key: &Q)
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.entry(key).await;
    }

    pub(crate) async fn take_entry<Q>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.entry_mut(key).await?.take()
    }
}

impl<K, V> Default for Registry<K, V> {
    fn default() -> Self {
        Self {
            entries: Default::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::time::Duration;

    use super::*;

    type R = Registry<String, i32>;

    /// Run the block in a Tokio executor with no I/O. Useful to write tests
    /// that need to run in Miri, which does not support Tokio I/O.
    fn block_on<F: Future>(future: F) -> F::Output {
        ::tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(future)
    }

    #[test]
    fn test_new() {
        let _ = R::default();
    }

    #[test]
    fn test_insert() {
        let m = R::default();
        let mut entry = block_on(async { m.lock_entry_mut("hello".to_owned()).await });
        assert!(entry.is_none());
        *entry = Some(100);
        assert_eq!(*entry, Some(100));
        std::mem::drop(entry);
        let entry = block_on(async { m.lock_entry_mut("hello".to_owned()).await });
        assert!(matches!(*entry, Some(100)));
    }

    #[test]
    fn test_take() {
        let m = R::default();
        let mut entry = block_on(async { m.lock_entry_mut("hello".to_owned()).await });
        *entry = Some(100);
        std::mem::drop(entry);
        let old = block_on(async { m.take_entry("hello").await });
        assert_eq!(old, Some(100));
        let entry = block_on(async { m.lock_entry_mut("hello".to_owned()).await });
        assert!(entry.is_none());
    }
    #[test]
    fn test_grow() {
        // Inserting many entries. This tests a case that used to trigger bad
        // behavior in a previous implementation which used unsafe code. Should
        // be consistently detected by Miri (probably ASAN too). Irrelevant in
        // safe code, but leaving this here as a memento.
        let m = R::default();
        block_on(async {
            let mut entries = vec![];
            for i in 0..4 {
                eprintln!("insert {i}...");
                let key = format!("key{i}");
                let entry = m.lock_entry_mut(key).await;
                assert!(entry.is_none());
                entries.push(entry);
            }
            for (i, e) in entries.iter_mut().enumerate() {
                eprintln!("get {i}...");
                **e = Some(i as i32);
            }
            for (i, e) in entries.iter_mut().enumerate() {
                assert_eq!(**e, Some(i as i32));
            }
        });
    }

    #[test]
    fn test_wait() {
        let m = R::default();
        block_on(async {
            tokio::time::timeout(Duration::from_millis(1), m.wait_entry("key"))
                .await
                .expect("wait_entry must return immediately when entry does not exist");
            let entry = m.lock_entry_mut("key".to_owned()).await;
            tokio::time::timeout(Duration::from_millis(10), m.wait_entry("key"))
                .await
                .expect_err("wait_entry must block when an entry is locked");
            std::mem::drop(entry);
            tokio::time::timeout(Duration::from_millis(1), m.wait_entry("key"))
                .await
                .expect("wait_entry must return immediately when entry is not locked");
        });
    }

    #[allow(dead_code)]
    async fn assert_entry_lifetimes() {
        async fn returning_an_entry(m: &R) -> ValueRefMut<i32> {
            m.lock_entry_mut("hello".to_owned()).await
        }
    }
}
