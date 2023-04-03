use std::borrow::Borrow;
use std::collections::HashMap;
use std::hash::Hash;

use tokio::sync::{Mutex, RwLock, RwLockWriteGuard};

/// Reference to a registered entry value. Holds a write guard.
/// Currently just a wrapper around RwLockWriteGuard, so maybe can be removed
/// later; or maybe it will be useful to implement entry drop/deletion.
pub(crate) struct ValueRefMut<'a, V> {
    inner: RwLockWriteGuard<'a, Option<V>>,
}

impl<'a, V> ValueRefMut<'a, V> {
    fn new(inner: RwLockWriteGuard<'a, Option<V>>) -> Self {
        Self { inner }
    }
}

impl<'a, V> std::ops::Deref for ValueRefMut<'a, V> {
    type Target = Option<V>;

    fn deref(&self) -> &Self::Target {
        self.inner.deref()
    }
}

impl<'a, V> std::ops::DerefMut for ValueRefMut<'a, V> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.deref_mut()
    }
}

struct RegistryEntry<V> {
    value: RwLock<Option<V>>,
}

impl<V> RegistryEntry<V> {
    fn with_value(value: V) -> Self {
        Self {
            value: RwLock::new(Some(value)),
        }
    }

    fn empty() -> Self {
        Self {
            value: RwLock::default(),
        }
    }

    async fn write(&self) -> ValueRefMut<V> {
        let guard = self.value.write().await;
        ValueRefMut::new(guard)
    }

    async fn read(&self) -> impl Drop + '_ {
        self.value.read().await
    }

    async fn take(&self) -> Option<V> {
        let mut guard = self.value.write().await;
        guard.take()
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
    async fn entry_or_default(&self, key: K) -> &RegistryEntry<V> {
        // Since we never remove a RegistryEntry, we can force the lifetime to
        // be the same as self. I would like to do this without unsafe if
        // possible but can't think of a way. Since we are downgrading a mut
        // ref to a shared ref, but we are messing with the lifetime, we cannot
        // ever use a (safe) mut ref to the entry anytime again.
        let mut map = self.entries.lock().await;
        let entry: &RegistryEntry<V> = map.entry(key).or_insert_with(RegistryEntry::empty);
        // Safety: no mutable references are ever created, and the entry is
        // only ever dropped if the Registry is dropped.
        unsafe { std::mem::transmute(entry) }
    }

    async fn entry<Q>(&self, key: &Q) -> Option<&RegistryEntry<V>>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let map = self.entries.lock().await;
        let entry = map.get(key);
        // Safety: see entry_or_default()
        unsafe { std::mem::transmute(entry) }
    }

    pub(crate) async fn lock_entry_mut(&self, key: K) -> ValueRefMut<V> {
        self.entry_or_default(key).await.write().await
    }

    /// Wait until no writers are touching the entry, if it exists; otherwise
    /// return immediately.
    pub(crate) async fn wait_entry<Q>(&self, key: &Q)
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        if let Some(entry) = self.entry(key).await {
            entry.read().await;
        }
    }

    pub(crate) async fn take_entry<Q>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.entries.lock().await.get_mut(key)?.take().await
    }
}

impl<K, V> Default for Registry<K, V> {
    fn default() -> Self {
        Self {
            entries: Default::default(),
        }
    }
}
