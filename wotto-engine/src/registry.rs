use std::borrow::Borrow;
use std::collections::HashMap;
use std::future::Future;
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
    fn entry_or_default(&self, key: K) -> impl Future<Output = ValueRefMut<V>> {
        let entry = {
            let mut map = self.entries.lock();
            map.entry(key)
                .or_insert_with(RegistryEntry::default)
                .clone()
        };
        entry.write_owned()
    }

    fn entry<Q>(&self, key: &Q) -> Option<impl Future<Output = ValueRef<V>>>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let entry = {
            let map = self.entries.lock();
            map.get(key)?.clone()
        };
        Some(entry.read_owned())
    }

    pub(crate) fn lock_entry_mut(&self, key: K) -> impl Future<Output = ValueRefMut<V>> {
        self.entry_or_default(key)
    }

    /// Wait until no writers are touching the entry, if it exists; otherwise
    /// return immediately.
    pub(crate) fn wait_entry<Q>(&self, key: &Q) -> Option<impl Future<Output = ValueRef<V>>>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.entry(key)
    }

    pub(crate) async fn take_entry<Q>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let entry = self.entries.lock().remove(key)?;
        entry.write_owned().await.take()
    }

    pub(crate) fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.entries.lock().contains_key(key)
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
    fn test_contains_key() {
        let m = R::default();
        assert!(!m.contains_key("foo"));
        block_on(async {
            *m.lock_entry_mut("foo".to_string()).await = Some(100);
        });
        assert!(m.contains_key("foo"));
        let _ = block_on(async { m.take_entry("foo").await });
        assert!(!m.contains_key("foo"));
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
        use futures::future::FutureExt;

        let m = R::default();

        assert!(
            m.wait_entry("key").is_none(),
            "wait_entry must return immediately when entry does not exist"
        );

        block_on(async {
            let entry = m.lock_entry_mut("key".to_owned()).await;

            assert!(m
                .wait_entry("key")
                .expect("wait_entry must return a future when an entry is locked")
                .now_or_never()
                .is_none());

            std::mem::drop(entry);

            let _ = m
                .wait_entry("key")
                .expect("wait_entry must return a future when an entry exists")
                .now_or_never()
                .expect("wait_entry must poll instantly when entry is not locked");
        });
    }

    #[allow(dead_code)]
    async fn assert_entry_lifetimes() {
        async fn returning_an_entry(m: &R) -> ValueRefMut<i32> {
            m.lock_entry_mut("hello".to_owned()).await
        }
    }
}
