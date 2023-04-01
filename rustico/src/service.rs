use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use thiserror::Error;
use tokio::sync::{mpsc, Mutex};
use tracing::{info, span, Level};
use wasmtime::*;

use crate::registry::Registry;
use crate::runtime as rt;
use crate::webload::{load_module_from_url, Domain, InvalidUrl, ResolvedModule, WebError};

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
    registry: Registry,
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

    async fn add_module(&self, namespace: Option<String>, name: String, module: Module) -> String {
        let fqn = if let Some(namespace) = namespace {
            format!("{namespace}/{name}")
        } else {
            name
        };
        let mut modules = self.modules.lock().await;
        modules.insert(fqn.clone(), module);
        fqn
    }

    #[tracing::instrument(skip(self))]
    pub async fn load_module(&self, name: String) -> Result<String> {
        span!(Level::TRACE, "load_module", name);
        if let Some(mut webmodule) = self.registry.lock_entry(&name).await {
                info!(module = name, url = %webmodule.url(), "reloading module");
                webmodule.ensure_content().await?;
                return Ok(name);
        }
        // quick and dirty name validation + path loading
        const MODULES_PATH: &str = "examples";
        let name_as_path = PathBuf::from_str(&name).map_err(|_| Error::InvalidModuleName)?;
        let file_name = name_as_path.file_name().ok_or(Error::InvalidModuleName)?;
        let path = Path::new(MODULES_PATH).join(file_name);
        let canonical_name = canonicalize_name(&path)?;
        let module = Module::from_file(&self.engine, &path).map_err(Error::Wasm)?;
        let fqn = self.add_module(None, canonical_name, module).await;

        Ok(fqn)
    }

    #[tracing::instrument(skip(self))]
    pub async fn load_module_from_url(&self, url: &str) -> Result<String> {
        let url: url::Url = url.parse().map_err(|_| InvalidUrl::ParseError)?;
        let webmodule = load_module_from_url(url).await?;
        self.load_web_module(webmodule).await
    }

    #[tracing::instrument(skip(self))]
    async fn load_web_module(&self, webmodule: ResolvedModule) -> Result<String> {
        let canonical_name = canonicalize_name(webmodule.name())?;
        let user = webmodule.user();
        let namespace = match webmodule.domain() {
            Domain::Github => user.to_string(),
            Domain::Other(domain) => format!("{user}@{domain}"),
        };

        let bytes = webmodule
            .content()
            .expect("loaded module should already have content");
        let module = Module::new(&self.engine, bytes).map_err(Error::Wasm)?;
        let fqn = self
            .add_module(Some(namespace), canonical_name, module)
            .await;
        self.registry.register(fqn.clone(), webmodule).await;
        Ok(fqn)
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
