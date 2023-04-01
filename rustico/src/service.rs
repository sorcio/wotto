use std::collections::HashMap;
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

pub struct Service {
    engine: Engine,
    modules: Arc<Mutex<HashMap<String, Module>>>,
    linker: Linker<RuntimeData>,
    registry: Registry<String>,
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
            modules: Arc::new(HashMap::new().into()),
            linker,
            registry: Registry::default(),
        }
    }

    pub fn increment_epoch(&self) {
        self.engine.increment_epoch();
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

    async fn add_module(&self, fqn: String, module: Module) {
        let mut modules = self.modules.lock().await;
        modules.insert(fqn, module);
    }

    #[tracing::instrument(skip(self))]
    pub async fn load_module(&self, name: String) -> Result<String> {
        let mut entry = self.registry.lock_entry(name.clone()).await;
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
            self.load_web_module_with_lock(&mut entry, name.clone(), new_webmodule)
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
        let fqn = canonicalize_name(&path)?;
        let module = Module::from_file(&self.engine, &path).map_err(Error::Wasm)?;
        self.add_module(fqn.clone(), module).await;
        Ok(fqn)
    }

    #[tracing::instrument(skip(self))]
    pub async fn load_module_from_url(&self, url: &str) -> Result<String> {
        let url: url::Url = url.parse().map_err(|_| InvalidUrl::ParseError)?;
        let webmodule = webload::resolve(url).await?;
        // the content might or might not be loaded at this point, but we have
        // enough information to determine the name of the module
        let fqn = self.fqn_for_module(&webmodule);
        self.load_web_module(fqn.clone(), webmodule).await?;
        Ok(fqn)
    }

    #[tracing::instrument(skip(self))]
    async fn load_web_module(&self, fqn: String, webmodule: ResolvedModule) -> Result<()> {
        let mut entry = self.registry.lock_entry(fqn.clone()).await;
        self.load_web_module_with_lock(&mut entry, fqn, webmodule)
            .await
    }

    async fn load_web_module_with_lock<'a>(
        &'a self,
        entry: &'a mut Option<ResolvedModule>,
        fqn: String,
        mut webmodule: ResolvedModule,
    ) -> Result<()> {
        webmodule.ensure_content().await?;
        let bytes = webmodule
            .content()
            .expect("loaded module should already have content");
        let wasm_module = Module::new(&self.engine, bytes).map_err(Error::Wasm)?;
        self.add_module(fqn.clone(), wasm_module).await;
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
        let module = {
            let modules = self.modules.lock().await;
            modules
                .get(module_name)
                .ok_or(Error::ModuleNotFound)?
                .clone()
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
