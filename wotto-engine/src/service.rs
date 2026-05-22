use std::borrow::Borrow;
use std::collections::HashMap;
use std::fmt::Display;
use std::future::Future;
use std::hash::Hash;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use thiserror::Error;
use tokio::sync::{Mutex, mpsc};
use tracing::info;
use wasmtime::*;

use crate::registry::Registry;
use crate::webload::{Domain, InvalidUrl, ResolvedModule, WebError};
use crate::{runtime as rt, webload};

use self::utils::EpochTimer;

const MEMORY_LIMIT_BYTES: usize = 1 << 20;
const TABLE_ELEMENT_LIMIT: usize = 10 << 10;

#[derive(Debug, Error)]
pub enum Error {
    #[error("wasm runtime error: {0}")]
    Wasm(wasmtime::Error),
    #[error("invalid module name")]
    InvalidModuleName,
    #[error("no such module")]
    ModuleNotFound,
    #[error("no such function")]
    FunctionNotFound,
    #[error("wrong function signature")]
    WrongFunctionType,
    #[error("memory is not exported")]
    MemoryNotExported,
    #[error("invalid pointer")]
    InvalidPointer,
    #[error("execution timed out")]
    TimedOut,
    #[error("invalid url ({0}")]
    InvalidUrl(#[from] InvalidUrl),
    #[error("error while fetching url ({0})")]
    CannotFetch(#[from] WebError),
    #[error("module {0} previously at url {1} not found")]
    ModuleGone(String, url::Url),
}

pub(crate) type Result<T> = std::result::Result<T, Error>;
pub(crate) type WResult<T> = wasmtime::Result<T>;

#[derive(Debug)]
pub enum Command {
    LoadModule(String),
    RunModule {
        module: String,
        entry_point: String,
        args: String,
    },
    Quit,
    Idle,
}

#[derive(Debug)]
struct CanonicalName<'a>(&'a str);

impl<'a> TryFrom<&'a Path> for CanonicalName<'a> {
    type Error = Error;

    fn try_from(value: &'a Path) -> std::result::Result<Self, Self::Error> {
        Ok(CanonicalName(
            value
                .file_stem()
                .ok_or(Error::InvalidModuleName)?
                .to_str()
                .ok_or(Error::InvalidModuleName)?,
        ))
    }
}

impl<'a> TryFrom<&'a str> for CanonicalName<'a> {
    type Error = <Self as TryFrom<&'a Path>>::Error;

    fn try_from(value: &'a str) -> std::result::Result<Self, Self::Error> {
        Self::try_from(Path::new(value))
    }
}

impl<'a> TryFrom<&'a PathBuf> for CanonicalName<'a> {
    type Error = <Self as TryFrom<&'a Path>>::Error;

    fn try_from(value: &'a PathBuf) -> std::result::Result<Self, Self::Error> {
        Self::try_from(Path::new(value))
    }
}

impl Display for CanonicalName<'_> {
    #[inline]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Identifier for a webmodule
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
#[repr(transparent)]
struct FullyQualifiedNameBuf {
    fqn: String,
}

impl FullyQualifiedNameBuf {
    fn new(domain: webload::Domain, canonical_name: CanonicalName, user: &str) -> Self {
        let fqn = match domain {
            Domain::Github => format!("{user}/{canonical_name}"),
            Domain::Builtin => format!("{canonical_name}"),
            Domain::Other(domain) => format!("{user}@{domain}/{canonical_name}"),
        };
        Self { fqn }
    }

    #[inline]
    fn new_builtin(canonical_name: CanonicalName) -> Self {
        Self {
            fqn: canonical_name.0.to_string(),
        }
    }
}

impl core::ops::Deref for FullyQualifiedNameBuf {
    type Target = FullyQualifiedName;

    #[inline]
    fn deref(&self) -> &Self::Target {
        FullyQualifiedName::from_str_unchecked(&self.fqn)
    }
}

impl Display for FullyQualifiedNameBuf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.fqn)
    }
}

impl Borrow<FullyQualifiedName> for FullyQualifiedNameBuf {
    fn borrow(&self) -> &FullyQualifiedName {
        self
    }
}

#[derive(Debug, Hash, PartialEq, Eq)]
#[repr(transparent)]
struct FullyQualifiedName {
    fqn: str,
}

impl FullyQualifiedName {
    fn from_str(s: &str) -> Result<&Self> {
        let _domain = if let Some((ns, _name)) = s.rsplit_once('/') {
            if let Some((_, _domain_name)) = ns.split_once('@') {
                // String matches format for "Other" domain but currently none exists
                return Err(Error::InvalidModuleName);
            } else if !ns.is_empty() {
                // By default, all users are Github users
                Domain::Github
            } else {
                return Err(Error::InvalidModuleName);
            }
        } else {
            Domain::Builtin
        };
        Ok(Self::from_str_unchecked(s))
    }

    fn from_str_unchecked(s: &str) -> &Self {
        // SAFETY: FullyQualifiedName is a transparent wrapper around `str`,
        // so it has the same layout and pointer metadata as the source `str`.
        unsafe { &*(s as *const str as *const Self) }
    }
}

impl ToOwned for FullyQualifiedName {
    type Owned = FullyQualifiedNameBuf;

    fn to_owned(&self) -> Self::Owned {
        FullyQualifiedNameBuf {
            fqn: self.fqn.to_owned(),
        }
    }
}

impl Display for FullyQualifiedName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.fqn)
    }
}

pub struct Service {
    engine: Engine,
    modules: Mutex<HashMap<FullyQualifiedNameBuf, Module>>,
    linker: Linker<RuntimeData>,
    registry: Registry<FullyQualifiedNameBuf, ResolvedModule>,
    epoch_timer: Arc<EpochTimer>,
}

fn make_engine() -> Engine {
    let mut config = Config::new();
    config
        .debug_info(true)
        .wasm_backtrace_details(WasmBacktraceDetails::Enable)
        .wasm_multi_memory(true)
        .wasm_memory64(false)
        .shared_memory(false)
        .epoch_interruption(true)
        .cranelift_opt_level(OptLevel::Speed);

    Engine::new(&config).unwrap()
}

impl Service {
    pub fn new() -> Self {
        let engine = make_engine();
        let mut linker = Linker::new(&engine);
        rt::add_to_linker(&mut linker, true)
            .map_err(Error::Wasm)
            .expect("runtime linking should be possible without shadowing");

        Service {
            engine,
            modules: Mutex::default(),
            linker,
            registry: Registry::default(),
            epoch_timer: Arc::default(),
        }
    }

    pub fn increment_epoch(&self) {
        self.engine.increment_epoch();
    }

    pub fn epoch_timer<F, P>(myself: F) -> impl Future
    where
        F: Fn() -> Option<P> + Send + 'static,
        P: AsRef<Self>,
    {
        let epoch_timer = myself().unwrap().as_ref().epoch_timer.clone();
        tokio::task::spawn_blocking(move || {
            let interval = std::time::Duration::from_millis(5);
            loop {
                epoch_timer.wait();
                std::thread::sleep(interval);
                let Some(slf) = myself() else {
                    break;
                };
                slf.as_ref().engine.increment_epoch();
            }
        })
    }

    pub async fn listen(&self, mut rx: mpsc::Receiver<Command>, tx: mpsc::Sender<Result<String>>) {
        // used for manual testing, maybe deprecate?
        while let Some(cmd) = rx.recv().await {
            let result = match cmd {
                Command::LoadModule(name) => self.load_module(name).await,
                Command::RunModule {
                    module,
                    entry_point,
                    args,
                } => self.run_module(&module, &entry_point, &args).await,
                Command::Idle => {
                    continue;
                }
                Command::Quit => {
                    break;
                }
            };
            if tx.send(result).await.is_err() {
                break;
            };
        }
    }

    async fn add_module(&self, fqn: FullyQualifiedNameBuf, module: Module) {
        let mut modules = self.modules.lock().await;
        modules.insert(fqn, module);
    }

    #[tracing::instrument(skip(self))]
    pub async fn load_module(&self, name: String) -> Result<String> {
        let key = FullyQualifiedName::from_str(&name)?;
        let mut entry = self.registry.lock_entry_mut(key.to_owned()).await;
        if let Some(webmodule) = &mut *entry {
            let url = webmodule.url().clone();
            info!(module = name, %url, "reloading module");
            // TODO should we make explicit an distinction between the case
            // when the user requests re-resolution (e.g. same URL gives a
            // newer version) vs when we want to just attempt a reload?
            // unsure if the "just reload" case actually exists
            let new_webmodule = webload::resolve(url.clone()).await?;
            let new_fqn = self.fqn_for_module(webmodule);
            if new_fqn != name {
                // a given url used to provide a module name, but it
                // doesn't anymore. the resolver is supposed to make sure
                // this never happens, but let's make extra sure we don't
                // mess with the registry state here.
                return Err(Error::ModuleGone(name, url));
            }
            self.load_web_module_with_lock(&mut entry, key, new_webmodule)
                .await?;
            return Ok(name);
        }
        // quick and dirty name validation + path loading
        const MODULES_PATH: &str = "examples";
        let name_as_path = PathBuf::from_str(&name).map_err(|_| Error::InvalidModuleName)?;
        let file_name = name_as_path.file_name().ok_or(Error::InvalidModuleName)?;
        let path = Path::new(MODULES_PATH).join(file_name);
        // "builtin" modules have a short fqn with no namespace or prefix
        // TODO: unify the builtin and web code paths
        let canonical_name = CanonicalName::try_from(&path)?;
        let fqn = FullyQualifiedNameBuf::new_builtin(canonical_name);
        let module = Module::from_file(&self.engine, &path).map_err(Error::Wasm)?;
        self.add_module(fqn.clone(), module).await;
        Ok(fqn.to_string())
    }

    #[tracing::instrument(skip(self))]
    pub async fn load_module_from_url(&self, url: &str) -> Result<String> {
        let url: url::Url = url.parse().map_err(|_| InvalidUrl::ParseError)?;
        let webmodule = webload::resolve(url).await?;
        // the content might or might not be loaded at this point, but we have
        // enough information to determine the name of the module
        let canonical_name = CanonicalName::try_from(webmodule.name())?;
        let fqn = FullyQualifiedNameBuf::new(webmodule.domain(), canonical_name, webmodule.user());
        self.load_web_module(fqn.clone(), webmodule).await?;
        Ok(fqn.to_string())
    }

    #[tracing::instrument(skip(self))]
    pub async fn unload_module(&self, name: &str) -> Result<String> {
        let key = FullyQualifiedName::from_str(name)?;
        self.registry
            .take_entry(key)
            .await
            .ok_or(Error::ModuleNotFound)?;
        Ok(key.to_string())
    }

    #[tracing::instrument(skip(self))]
    async fn load_web_module(
        &self,
        fqn: FullyQualifiedNameBuf,
        webmodule: ResolvedModule,
    ) -> Result<()> {
        let mut entry = self.registry.lock_entry_mut(fqn.clone()).await;
        self.load_web_module_with_lock(&mut entry, &fqn, webmodule)
            .await
    }

    async fn load_web_module_with_lock<'a>(
        &'a self,
        entry: &'a mut Option<ResolvedModule>,
        fqn: &FullyQualifiedName,
        mut webmodule: ResolvedModule,
    ) -> Result<()> {
        webmodule.ensure_content().await?;
        let bytes = webmodule
            .content()
            .expect("loaded module should already have content");
        let wasm_module = Module::new(&self.engine, bytes).map_err(Error::Wasm)?;
        self.add_module(fqn.to_owned(), wasm_module).await;
        *entry = Some(webmodule);
        Ok(())
    }

    fn fqn_for_module(&self, webmodule: &ResolvedModule) -> String {
        let canonical_name = canonicalize_name(webmodule.name()).unwrap();
        let user = webmodule.user();
        let namespace = match webmodule.domain() {
            Domain::Github => Some(user.to_string()),
            Domain::Builtin => None,
            Domain::Other(domain) => Some(format!("{user}@{domain}")),
        };

        if let Some(namespace) = namespace {
            format!("{namespace}/{canonical_name}")
        } else {
            canonical_name
        }
    }

    #[tracing::instrument(skip(self))]
    pub async fn run_module(
        &self,
        module_name: &str,
        entry_point: &str,
        args: &str,
    ) -> Result<String> {
        // If module is being reloaded, wait until new code is available
        let key = FullyQualifiedName::from_str(module_name)?;
        self.registry.wait_entry(key).await;
        let module = {
            let modules = self.modules.lock().await;
            modules.get(key).ok_or(Error::ModuleNotFound)?.clone()
        };

        let runtime_data = RuntimeData::new(args.to_string(), 512);
        let mut store = Store::new(&self.engine, runtime_data);
        store.limiter(|state| &mut state.limits);
        store.epoch_deadline_async_yield_and_update(1);

        let instance = self
            .linker
            .instantiate_async(&mut store, &module)
            .await
            .map_err(Error::Wasm)?;

        let func = instance
            .get_func(&mut store, entry_point)
            .ok_or(Error::FunctionNotFound)?;
        let tyfunc = func
            .typed::<(), ()>(&mut store)
            .map_err(|_| Error::WrongFunctionType)?;

        let _timer = self.epoch_timer.start();
        let duration = std::time::Duration::from_millis(5000);
        let fut = tyfunc.call_async(&mut store, ());
        match tokio::time::timeout(duration, fut).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                return Err(Error::Wasm(err));
            }
            Err(_) => {
                return Err(Error::TimedOut);
            }
        }

        Ok(store.into_data().output)
    }
}

impl Default for Service {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.epoch_timer.shutdown();
    }
}

struct RuntimeData {
    message: String,
    output: String,
    capacity: usize,
    limits: RuntimeLimits,
}

impl RuntimeData {
    fn new(message: String, output_capacity: usize) -> Self {
        let output = String::with_capacity(output_capacity);
        Self {
            message,
            output,
            capacity: output_capacity,
            limits: RuntimeLimits::new(),
        }
    }
}

#[derive(Debug, Default)]
struct RuntimeLimits {
    memory_bytes: usize,
    pending_memory_delta: Option<usize>,
}

impl RuntimeLimits {
    fn new() -> Self {
        Self::default()
    }

    fn clear_successful_pending_memory_grow(&mut self) {
        self.pending_memory_delta = None;
    }
}

impl ResourceLimiter for RuntimeLimits {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        self.clear_successful_pending_memory_grow();

        if matches!(maximum, Some(maximum) if desired > maximum) {
            return Ok(false);
        }

        let Some(delta) = desired.checked_sub(current) else {
            return Ok(false);
        };
        if delta == 0 {
            return Ok(true);
        }

        let Some(next_memory_bytes) = self.memory_bytes.checked_add(delta) else {
            return Ok(false);
        };
        if next_memory_bytes > MEMORY_LIMIT_BYTES {
            return Ok(false);
        }

        self.memory_bytes = next_memory_bytes;
        self.pending_memory_delta = Some(delta);
        Ok(true)
    }

    fn memory_grow_failed(&mut self, error: wasmtime::Error) -> wasmtime::Result<()> {
        if let Some(delta) = self.pending_memory_delta.take() {
            self.memory_bytes = self.memory_bytes.saturating_sub(delta);
        }
        tracing::debug!(%error, "wasm memory growth failed after limit reservation");
        Ok(())
    }

    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        if matches!(maximum, Some(maximum) if desired > maximum) {
            return Ok(false);
        }
        Ok(desired <= TABLE_ELEMENT_LIMIT)
    }
}

pub(crate) trait HasInput {
    fn input(&self) -> &str;
}

pub(crate) trait HasOutput {
    fn output(&mut self, text: &str);
}

impl HasInput for RuntimeData {
    fn input(&self) -> &str {
        &self.message
    }
}

impl HasOutput for RuntimeData {
    fn output(&mut self, text: &str) {
        let Some(available) = self.capacity.checked_sub(self.output.len()) else {
            return;
        };
        self.output += &text[..available.min(text.len())];
    }
}

pub(crate) fn get_memory<T>(caller: &mut Caller<'_, T>) -> Result<Memory> {
    let mem = caller
        .get_export("memory")
        .ok_or(Error::MemoryNotExported)?
        .into_memory()
        .ok_or(Error::MemoryNotExported)?;
    Ok(mem)
}

fn canonicalize_name<P: AsRef<Path>>(path: P) -> Result<String> {
    Ok(path
        .as_ref()
        .with_extension("")
        .file_name()
        .unwrap()
        .to_str()
        .ok_or(Error::InvalidModuleName)?
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: usize = 1 << 16;

    fn limited_store(engine: &Engine) -> Store<RuntimeData> {
        let mut store = Store::new(engine, RuntimeData::new(String::new(), 512));
        store.limiter(|state| &mut state.limits);
        store.set_epoch_deadline(1);
        store
    }

    fn instantiate(wat: &str) -> wasmtime::Result<()> {
        let engine = make_engine();
        let module = Module::new(&engine, wat)?;
        let mut store = limited_store(&engine);
        Instance::new(&mut store, &module, &[])?;
        Ok(())
    }

    #[test]
    fn allows_single_memory_at_total_limit() {
        instantiate("(module (memory 16))").expect("memory exactly at aggregate limit");
    }

    #[test]
    fn rejects_initial_memory_above_total_limit() {
        instantiate("(module (memory 17))").expect_err("memory above aggregate limit");
    }

    #[test]
    fn allows_multiple_memories_at_combined_total_limit() {
        instantiate(
            r#"
            (module
              (memory 8)
              (memory 8))
            "#,
        )
        .expect("combined memories exactly at aggregate limit");
    }

    #[test]
    fn rejects_multiple_memories_above_combined_total_limit() {
        instantiate(
            r#"
            (module
              (memory 16)
              (memory 1))
            "#,
        )
        .expect_err("combined memories above aggregate limit");
    }

    #[test]
    fn memory_grow_past_total_limit_returns_failure() {
        let engine = make_engine();
        let module = Module::new(
            &engine,
            r#"
            (module
              (memory 16 100)
              (func (export "grow") (result i32)
                i32.const 1
                memory.grow))
            "#,
        )
        .expect("compile memory grow fixture");
        let mut store = limited_store(&engine);
        let instance = Instance::new(&mut store, &module, &[]).expect("instantiate grow fixture");
        let grow = instance
            .get_typed_func::<(), i32>(&mut store, "grow")
            .expect("fixture exports grow");

        assert_eq!(grow.call(&mut store, ()).expect("call grow"), -1);
    }

    #[test]
    fn denied_growth_does_not_poison_later_valid_growth() {
        let engine = make_engine();
        let module = Module::new(
            &engine,
            r#"
            (module
              (memory 15 100)
              (func (export "grow2") (result i32)
                i32.const 2
                memory.grow)
              (func (export "grow1") (result i32)
                i32.const 1
                memory.grow))
            "#,
        )
        .expect("compile retry grow fixture");
        let mut store = limited_store(&engine);
        let instance = Instance::new(&mut store, &module, &[]).expect("instantiate grow fixture");
        let grow2 = instance
            .get_typed_func::<(), i32>(&mut store, "grow2")
            .expect("fixture exports grow2");
        let grow1 = instance
            .get_typed_func::<(), i32>(&mut store, "grow1")
            .expect("fixture exports grow1");

        assert_eq!(grow2.call(&mut store, ()).expect("call grow2"), -1);
        assert_eq!(grow1.call(&mut store, ()).expect("call grow1"), 15);
    }

    #[test]
    fn failed_growth_reservation_rolls_back() {
        let mut limits = RuntimeLimits::new();

        assert!(
            limits
                .memory_growing(0, MEMORY_LIMIT_BYTES, Some(MEMORY_LIMIT_BYTES * 2))
                .expect("reserve memory")
        );
        limits
            .memory_grow_failed(wasmtime::Error::msg("simulated allocation failure"))
            .expect("rollback failed grow");

        assert_eq!(limits.memory_bytes, 0);
        assert!(
            limits
                .memory_growing(0, MEMORY_LIMIT_BYTES, Some(MEMORY_LIMIT_BYTES))
                .expect("reserve memory after rollback")
        );
    }

    #[test]
    fn table_limit_is_still_enforced() {
        instantiate("(module (table 10241 funcref))").expect_err("table above element limit");
        instantiate("(module (table 10240 funcref))").expect("table at element limit");
    }

    #[test]
    fn memory64_is_rejected() {
        let engine = make_engine();
        assert!(
            Module::new(&engine, "(module (memory i64 1))").is_err(),
            "memory64 should remain disabled"
        );
    }

    #[test]
    fn shared_memory_is_rejected() {
        let engine = make_engine();
        assert!(
            Module::new(&engine, "(module (memory 1 1 shared))").is_err(),
            "shared memory should remain disabled"
        );
    }

    #[test]
    fn constants_match_existing_resource_budget() {
        assert_eq!(MEMORY_LIMIT_BYTES, 16 * PAGE);
        assert_eq!(TABLE_ELEMENT_LIMIT, 10 * 1024);
    }
}

mod utils {
    use parking_lot::{Condvar, Mutex};
    #[derive(Debug, Default)]
    pub(super) struct EpochTimer {
        cond_var: Condvar,
        counter: Mutex<i32>,
    }

    impl EpochTimer {
        fn increment(&self) {
            let mut counter = self.counter.lock();
            *counter += 1;
            self.cond_var.notify_all();
        }

        fn decrement(&self) {
            let mut counter = self.counter.lock();
            *counter -= 1;
            self.cond_var.notify_all();
        }

        pub(super) fn start(&self) -> EpochTimerGuard<'_> {
            self.increment();
            EpochTimerGuard(self)
        }

        pub(super) fn shutdown(&self) {
            // TODO cleaner shutdown (with Drop guard)
            let mut counter = self.counter.lock();
            // just some obviously non-zero value:
            *counter = i32::MIN;
            self.cond_var.notify_all();
        }

        pub(super) fn wait(&self) {
            let mut counter = self.counter.lock();
            if *counter == 0 {
                self.cond_var.wait(&mut counter);
            }
        }
    }

    pub(super) struct EpochTimerGuard<'a>(&'a EpochTimer);

    impl<'a> Drop for EpochTimerGuard<'a> {
        fn drop(&mut self) {
            self.0.decrement();
        }
    }
}
