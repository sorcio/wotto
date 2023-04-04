use std::borrow::Borrow;
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;

use tokio::sync::{Mutex, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

/// Reference to a registered entry value. Holds a write guard.
/// Currently just a wrapper around RwLockWriteGuard, so maybe can be removed
/// later; or maybe it will be useful to implement entry drop/deletion.
pub(crate) struct ValueRefMut<V> {
    inner: OwnedRwLockWriteGuard<Option<V>>,
}

impl<V> ValueRefMut<V> {
    fn new(inner: OwnedRwLockWriteGuard<Option<V>>) -> Self {
        Self { inner }
    }
}

impl<V> std::ops::Deref for ValueRefMut<V> {
    type Target = Option<V>;

    fn deref(&self) -> &Self::Target {
        self.inner.deref()
    }
}

impl<V> std::ops::DerefMut for ValueRefMut<V> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.deref_mut()
    }
}

pub(crate) struct ValueRef<V> {
    inner: OwnedRwLockReadGuard<Option<V>>,
}

impl<V> ValueRef<V> {
    fn new(inner: OwnedRwLockReadGuard<Option<V>>) -> Self {
        Self { inner }
    }
}

impl<V> std::ops::Deref for ValueRef<V> {
    type Target = Option<V>;

    fn deref(&self) -> &Self::Target {
        self.inner.deref()
    }
}

struct RegistryEntry<V> {
    value: Arc<RwLock<Option<V>>>,
}

impl<V> RegistryEntry<V> {
    fn with_value(value: V) -> Self {
        Self {
            value: Arc::new(RwLock::new(Some(value))),
        }
    }

    fn empty() -> Self {
        Self {
            value: Arc::default(),
        }
    }

    async fn write(&self) -> ValueRefMut<V> {
        let guard = self.value.clone().write_owned().await;
        ValueRefMut::new(guard)
    }

    async fn read(&self) -> ValueRef<V> {
        let guard = self.value.clone().read_owned().await;
        ValueRef::new(guard)
    }
}

impl<V> From<V> for RegistryEntry<V> {
    fn from(value: V) -> Self {
        Self::with_value(value)
    }
}

pub(crate) struct Registry<K, V> {
    entries: Mutex<HashMap<K, RegistryEntry<V>>>,
}

impl<K, V> Registry<K, V>
where
    K: Hash + Eq,
{
    async fn entry_or_default(&self, key: K) -> ValueRefMut<V> {
        let mut map = self.entries.lock().await;
        let entry = map.entry(key).or_insert_with(RegistryEntry::empty);
        entry.write().await
    }

    async fn entry<Q>(&self, key: &Q) -> Option<ValueRef<V>>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let map = self.entries.lock().await;
        let entry = map.get(key)?;
        Some(entry.read().await)
    }

    async fn entry_mut<Q>(&self, key: &Q) -> Option<ValueRefMut<V>>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let map = self.entries.lock().await;
        let entry = map.get(key)?;
        Some(entry.write().await)
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
