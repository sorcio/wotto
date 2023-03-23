use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use thiserror::Error;
use tokio::sync::{mpsc, Mutex};
use wasmtime::*;

use crate::assemblyscript::{env_abort, AssemblyScriptString};

#[derive(Debug, Error)]
pub enum Error {
    #[error("wasm runtime error: {0}")]
    WasmError(anyhow::Error),
    #[error("invalid module name")]
    InvalidModuleName,
    #[error("no such module")]
    ModuleNotFound,
    #[error("no such function")]
    FunctionNotFound,
    #[error("wrong function signature")]
    FunctionTypeError,
    #[error("memory is not exported")]
    MemoryNotExported,
    #[error("invalid pointer")]
    InvalidPointer,
}

pub(crate) type Result<T> = std::result::Result<T, Error>;
pub(crate) type WResult<T> = std::result::Result<T, anyhow::Error>;

#[derive(Debug)]
pub enum Command {
    LoadModule(String),
    RunModule { module: String, entry_point: String, args: String },
    Quit,
    Idle,
}

pub struct Service {
    engine: Engine,
    modules: Arc<Mutex<HashMap<String, Module>>>,
}

impl Service {
    pub fn new() -> Self {
        Service {
            engine: Engine::default(),
            modules: Arc::new(HashMap::new().into()),
        }
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
                } => self.run_module(module, entry_point, args).await,
                Command::Idle => { continue; },
                Command::Quit => { break; }
            };
            if let Err(_) = tx.send(result).await {
                break;
            };
        }
    }

    pub async fn load_module(&self, name: String) -> Result<String> {
        // quick and dirty name validation + path loading
        use std::path::{Path, PathBuf};
        const MODULES_PATH: &str = "examples";
        let name_as_path = PathBuf::from_str(&name).map_err(|_| Error::InvalidModuleName)?;
        let file_name = name_as_path.file_name().ok_or(Error::InvalidModuleName)?;
        let path = Path::new(MODULES_PATH).join(file_name);
        let canonical_name = path.with_extension("").file_name().unwrap().to_str().ok_or(Error::InvalidModuleName)?.to_string();

        let module = Module::from_file(&self.engine, &path).map_err(Error::WasmError)?;

        let mut modules = self.modules.lock().await;
        modules.insert(canonical_name.clone(), module);

        Ok(canonical_name)
    }

    pub async fn run_module(&self, module_name: String, entry_point: String, args: String) -> Result<String> {
        let modules = self.modules.lock().await;
        let module = modules.get(&module_name).ok_or(Error::ModuleNotFound)?;


        let mut linker = Linker::new(&self.engine);

        // AS-style print, remove later
        linker
            .func_wrap(
                "rusto",
                "print",
                |mut caller: Caller<'_, _>, ptr: u32| -> WResult<()> {
                    let memory = get_memory(&mut caller)?.data(&caller);
                    let txt = AssemblyScriptString::from_memory(memory, ptr)
                        .ok_or(Error::InvalidPointer)?;
                    println!("rusto.print {txt}");
                    Ok(())
                },
            )
            .map_err(Error::WasmError)?;
        linker
            .func_wrap(
                "rusto",
                "output",
                |mut caller: Caller<'_, RuntimeData>, ptr: u32, len: u32| -> WResult<()> {
                    let (memory, runtime_data) = get_memory(&mut caller)?.data_and_store_mut(&mut caller);
                    let offset = ptr as usize;
                    let size = len as usize;
                    let strdata = &memory[offset..][..size];
                    let txt = std::str::from_utf8(strdata)?;
                    println!("rusto.output {txt}");
                    runtime_data.output += txt;
                    Ok(())
                },
            )
            .map_err(Error::WasmError)?;
        linker
            .func_wrap(
                "rusto",
                "input",
                |mut caller: Caller<'_, RuntimeData>, ptr: u32, len: u32| -> WResult<u32> {
                    let (memory, runtime_data) = get_memory(&mut caller)?.data_and_store_mut(&mut caller);

                    let offset = ptr as usize;
                    let size = len as usize;
                    let buf = &mut memory[offset..][..size];

                    let message = runtime_data.message.as_bytes();
                    let actual_size = message.len();
                    if size >= actual_size {
                        buf[..actual_size].copy_from_slice(&message);
                    } else {
                        buf.copy_from_slice(&message[..size]);
                    }

                    Ok(actual_size.try_into().unwrap())
                },
            )
            .map_err(Error::WasmError)?;
        linker
            .func_wrap("env", "abort", env_abort)
            .map_err(Error::WasmError)?;

        let runtime_data = RuntimeData { message: args, output: String::with_capacity(512) };
        let mut store = Store::new(&self.engine, runtime_data);

        let instance = linker
            .instantiate(&mut store, module)
            .map_err(Error::WasmError)?;

        let func = instance
            .get_func(&mut store, &entry_point)
            .ok_or(Error::FunctionNotFound)?;
        let tyfunc = func
            .typed::<(), ()>(&mut store)
            .map_err(|_| Error::FunctionTypeError)?;

        tyfunc.call(&mut store, ()).map_err(Error::WasmError)?;

        Ok(store.into_data().output)
    }
}

struct RuntimeData {
    message: String,
    output: String,
}

pub(crate) fn get_memory<'a, T>(caller: &'a mut Caller<'_, T>) -> Result<Memory> {
    let mem = caller
        .get_export("memory")
        .ok_or(Error::MemoryNotExported)?
        .into_memory()
        .ok_or(Error::MemoryNotExported)?;
    Ok(mem)
}

// pub(crate) fn get_memory_mut<'a, T>(caller: &'a mut Caller<'_, T>) -> Result<&'a mut [u8]> {
//     let mem = caller
//         .get_export("memory")
//         .ok_or(Error::MemoryNotExported)?
//         .into_memory()
//         .ok_or(Error::MemoryNotExported)?;
//     Ok(mem.data_mut(caller))
// }
