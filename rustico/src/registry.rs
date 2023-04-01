use std::collections::HashMap;
use std::hash::Hash;

use tokio::sync::{Mutex, RwLock, RwLockWriteGuard};

use crate::webload::ResolvedModule;

/// Reference to a registered module. Holds a write guard.
/// Currently just a wrapper around RwLockWriteGuard, so maybe can be removed
/// later; or maybe it will be useful to implement module drop/deletion.
pub(crate) struct ModuleRefMut<'a> {
    inner: RwLockWriteGuard<'a, Option<ResolvedModule>>,
}

impl<'a> ModuleRefMut<'a> {
    fn new(inner: RwLockWriteGuard<'a, Option<ResolvedModule>>) -> Self {
        Self { inner }
    }
}

impl<'a> std::ops::Deref for ModuleRefMut<'a> {
    type Target = Option<ResolvedModule>;

    fn deref(&self) -> &Self::Target {
        self.inner.deref()
    }
}

impl<'a> std::ops::DerefMut for ModuleRefMut<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.deref_mut()
    }
}

struct ModuleEntry {
    module: RwLock<Option<ResolvedModule>>,
}

impl ModuleEntry {
    fn with_module(module: ResolvedModule) -> Self {
        Self {
            module: RwLock::new(Some(module)),
        }
    }

    fn empty() -> Self {
        Self {
            module: RwLock::default(),
        }
    }

    async fn lock(&self) -> ModuleRefMut {
        let guard = self.module.write().await;
        ModuleRefMut::new(guard)
    }
}

impl From<ResolvedModule> for ModuleEntry {
    fn from(value: ResolvedModule) -> Self {
        Self::with_module(value)
    }
}

pub(crate) struct Registry<K> {
    modules: Mutex<HashMap<K, ModuleEntry>>,
}

impl<K> Registry<K>
where K: Hash + Eq {
    async fn entry_or_default(&self, key: K) -> &ModuleEntry {
        // Since we never remove a ModuleEntry, we can force the lifetime to be
        // the same as self. I would like to do this without unsafe if possible
        // but can't think of a way. Since we are downgrading a mut ref to a
        // shared ref, but we are messing with the lifetime, we cannot ever
        // use a (safe) mut ref to the entry anytime again.
        let mut map = self.modules.lock().await;
        let entry: &ModuleEntry = map.entry(key).or_insert_with(ModuleEntry::empty);
        // Safety: no mutable references are ever created, and the entry is
        // only ever dropped if the Registry is dropped.
        unsafe { std::mem::transmute(entry) }
    }

    pub(crate) async fn lock_entry(&self, key: K) -> ModuleRefMut {
        self.entry_or_default(key).await.lock().await
    }
}

impl<K> Default for Registry<K> {
    fn default() -> Self {
        Self {
            modules: Default::default(),
        }
    }
}
