//! Functions exported to WASM modules.

use crate::assemblyscript::{env_abort, AssemblyScriptString};
use crate::service::{get_memory, Error, HasInput, HasOutput, WResult};
use wasmtime::*;

/// AssemblyScript-style print
///
/// ```ts
/// declare function print(text: string): void
/// ```
fn print<T>(mut caller: Caller<'_, T>, ptr: u32) -> WResult<()> {
    let memory = get_memory(&mut caller)?.data(&caller);
    let txt = AssemblyScriptString::from_memory(memory, ptr).ok_or(Error::InvalidPointer)?;
    println!("wotto.print {txt}");
    Ok(())
}

fn output<T: HasOutput>(mut caller: Caller<'_, T>, ptr: u32, len: u32) -> WResult<()> {
    let (memory, runtime_data) = get_memory(&mut caller)?.data_and_store_mut(&mut caller);
    let offset = ptr as usize;
    let size = len as usize;
    let strdata = &memory[offset..][..size];
    let txt = std::str::from_utf8(strdata)?;
    println!("wotto.output {txt}");
    runtime_data.output(txt);
    Ok(())
}

fn input<T: HasInput>(mut caller: Caller<'_, T>, ptr: u32, len: u32) -> WResult<u32> {
    let (memory, runtime_data) = get_memory(&mut caller)?.data_and_store_mut(&mut caller);

    let offset = ptr as usize;
    let size = len as usize;
    let buf = &mut memory[offset..][..size];

    let message = runtime_data.input().as_bytes();
    let actual_size = message.len();
    if size >= actual_size {
        buf[..actual_size].copy_from_slice(message);
    } else {
        buf.copy_from_slice(&message[..size]);
    }

    Ok(actual_size.try_into().unwrap())
}

pub(crate) fn add_to_linker<T>(
    linker: &mut Linker<T>,
    enable_assembly_script_support: bool,
) -> WResult<()>
where
    T: HasInput + HasOutput + 'static,
{
    linker.func_wrap("wotto", "output", output)?;
    linker.func_wrap("wotto", "input", input)?;

    if enable_assembly_script_support {
        linker.func_wrap("wotto", "print", print)?;
        linker.func_wrap("env", "abort", env_abort)?;
    }

    Ok(())
}
