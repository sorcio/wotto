use std::borrow::Borrow;
use std::collections::HashMap;
use std::fmt::Display;
use std::future::Future;
use std::hash::Hash;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use thiserror::Error;
use tokio::sync::{mpsc, Mutex};
use tracing::info;
use wasmtime::*;

use crate::registry::Registry;
use crate::webload::{Domain, InvalidUrl, ResolvedModule, WebError};
use crate::{runtime as rt, webload};

use self::utils::EpochTimer;

#[derive(Debug, Error)]
pub enum Error {
    #[error("wasm runtime error: {0}")]
    Wasm(anyhow::Error),
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
pub(crate) type WResult<T> = std::result::Result<T, anyhow::Error>;

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
struct FullyQualifiedName {
    fqn: String,
}

impl FullyQualifiedName {
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

impl core::ops::Deref for FullyQualifiedName {
    type Target = FullyQualifiedNameBorrow;

    #[inline]
    fn deref(&self) -> &Self::Target {
        unsafe { FullyQualifiedNameBorrow::from_str_unchecked(&self.fqn) }
    }
}

impl Display for FullyQualifiedName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.fqn)
    }
}

impl Borrow<FullyQualifiedNameBorrow> for FullyQualifiedName {
    fn borrow(&self) -> &FullyQualifiedNameBorrow {
        self
    }
}

#[derive(Debug, Hash, PartialEq, Eq)]
#[repr(transparent)]
struct FullyQualifiedNameBorrow {
    fqn: str,
}

impl FullyQualifiedNameBorrow {
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
        Ok(unsafe { Self::from_str_unchecked(s) })
    }

    unsafe fn from_str_unchecked(s: &str) -> &Self {
        // Safety: FullyQualifiedNameBorrow is repr(transparent) with str
        unsafe { std::mem::transmute(s) }
    }
}

impl ToOwned for FullyQualifiedNameBorrow {
    type Owned = FullyQualifiedName;

    fn to_owned(&self) -> Self::Owned {
        FullyQualifiedName {
            fqn: self.fqn.to_owned(),
        }
    }
}

impl Display for FullyQualifiedNameBorrow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.fqn)
    }
}

pub struct Service {
    engine: Engine,
    modules: Mutex<HashMap<FullyQualifiedName, Module>>,
    linker: Linker<RuntimeData>,
    registry: Registry<FullyQualifiedName, ResolvedModule>,
    epoch_timer: Arc<EpochTimer>,
}

fn make_engine() -> Engine {
    let mut config = Config::new();
    config
        .debug_info(true)
        .wasm_backtrace_details(WasmBacktraceDetails::Enable)
        .async_support(true)
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
                let Some(slf) = myself() else { break; };
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

    async fn add_module(&self, fqn: FullyQualifiedName, module: Module) {
        let mut modules = self.modules.lock().await;
        modules.insert(fqn, module);
    }

    #[tracing::instrument(skip(self))]
    pub async fn load_module(&self, name: String) -> Result<String> {
        let key = FullyQualifiedNameBorrow::from_str(&name)?;
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
        let fqn = FullyQualifiedName::new_builtin(canonical_name);
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
        let fqn = FullyQualifiedName::new(webmodule.domain(), canonical_name, webmodule.user());
        self.load_web_module(fqn.clone(), webmodule).await?;
        Ok(fqn.to_string())
    }

    #[tracing::instrument(skip(self))]
    async fn load_web_module(
        &self,
        fqn: FullyQualifiedName,
        webmodule: ResolvedModule,
    ) -> Result<()> {
        let mut entry = self.registry.lock_entry_mut(fqn.clone()).await;
        self.load_web_module_with_lock(&mut entry, &fqn, webmodule)
            .await
    }

    async fn load_web_module_with_lock<'a>(
        &'a self,
        entry: &'a mut Option<ResolvedModule>,
        fqn: &FullyQualifiedNameBorrow,
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
        let key = FullyQualifiedNameBorrow::from_str(module_name)?;
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
    limits: StoreLimits,
}

impl RuntimeData {
    fn new(message: String, output_capacity: usize) -> Self {
        let output = String::with_capacity(output_capacity);
        let limits = StoreLimitsBuilder::new()
            .memory_size(1 << 20)
            .table_elements(10 << 10)
            .build();
        Self {
            message,
            output,
            capacity: output_capacity,
            limits,
        }
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
        let Some(available) = self.capacity.checked_sub(self.output.len()) else { return; };
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

        pub(super) fn start(&self) -> EpochTimerGuard {
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
