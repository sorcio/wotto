use std::collections::HashMap;

use tokio::sync::{Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::webload::ResolvedModule;

/// Reference to a registered module. Holds a read guard.
pub(crate) struct ModuleRef<'a> {
    inner: RwLockReadGuard<'a, Option<ResolvedModule>>,
}

impl<'a> ModuleRef<'a> {
    fn new(inner: RwLockReadGuard<'a, Option<ResolvedModule>>) -> Self {
        debug_assert!(inner.is_some(), "ModuleRef cannot be initialized with None");
        Self { inner }
    }
}

impl<'a> std::ops::Deref for ModuleRef<'a> {
    type Target = ResolvedModule;

    fn deref(&self) -> &Self::Target {
        self.inner.as_ref().unwrap()
    }
}

pub(crate) struct ModuleRefMut<'a> {
    inner: RwLockWriteGuard<'a, Option<ResolvedModule>>,
}

impl<'a> ModuleRefMut<'a> {
    fn new(inner: RwLockWriteGuard<'a, Option<ResolvedModule>>) -> Self {
        debug_assert!(
            inner.is_some(),
            "ModuleRefMut cannot be initialized with None"
        );
        Self { inner }
    }
}

impl<'a> std::ops::Deref for ModuleRefMut<'a> {
    type Target = ResolvedModule;

    fn deref(&self) -> &Self::Target {
        self.inner.as_ref().unwrap()
    }
}

impl<'a> std::ops::DerefMut for ModuleRefMut<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.as_mut().unwrap()
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

    async fn replace(&self, module: ResolvedModule) -> (ModuleRef, Option<ResolvedModule>) {
        let mut guard = self.module.write().await;
        let old = guard.replace(module);
        (ModuleRef::new(guard.downgrade()), old)
    }

    async fn lock(&self) -> Option<ModuleRefMut> {
        let guard = self.module.write().await;
        match *guard {
            Some(_) => Some(ModuleRefMut::new(guard)),
            None => None,
        }
    }
}

impl From<ResolvedModule> for ModuleEntry {
    fn from(value: ResolvedModule) -> Self {
        Self::with_module(value)
    }
}

pub(crate) struct Registry {
    modules: Mutex<HashMap<String, ModuleEntry>>,
}

impl Registry {
    async fn entry_or_default(&self, key: String) -> &ModuleEntry {
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

    async fn entry(&self, key: &str) -> Option<&ModuleEntry> {
        // Similarly to entry_or_default() we force the lifetime. But we only
        // want a reference if the entry actually exists. This can save the
        // caller to allocate/copy the key.
        let map = self.modules.lock().await;
        // Safety: see entry_or_default()
        map.get(key)
            .map(|entry| unsafe { std::mem::transmute(entry) })
    }

    pub(crate) async fn register(
        &self,
        name: String,
        module: ResolvedModule,
    ) -> (ModuleRef, Option<ResolvedModule>) {
        // let entry = {
        //     self.modules.lock().await.entry(name).or_insert_with(ModuleEntry::empty)
        // };
        let entry = self.entry_or_default(name).await;
        // TODO there could be a no-await path if the entry is vacant, would it
        // be useful?
        entry.replace(module).await
    }

    pub(crate) async fn lock_entry(&self, name: &str) -> Option<ModuleRefMut> {
        match self.entry(name).await {
            Some(entry) => entry.lock().await,
            None => None,
        }
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            modules: Default::default(),
        }
    }
}
