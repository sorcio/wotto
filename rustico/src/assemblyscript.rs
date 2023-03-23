//! AssemblyScript support.

use std::fmt::Display;
use std::marker::PhantomData;
use std::mem::size_of;

use wasmtime::{Caller, Trap};

use crate::service::{get_memory, Error, WResult};

#[allow(dead_code)]
const AS_CLASS_ID_OBJECT: u32 = 0;
#[allow(dead_code)]
const AS_CLASS_ID_BUFFER: u32 = 1;
const AS_CLASS_ID_STRING: u32 = 2;

#[allow(dead_code, non_snake_case)]
#[repr(packed)]
struct AssemblyScriptHeader {
    /// mmInfo  20  usize   Memory manager info
    mmInfo: u32,
    /// gcInfo  16  usize   Garbage collector info
    gcInfo: u32,
    /// gcInfo2 12  usize   Garbage collector info
    gcInfo2: u32,
    /// rtId    8   u32     Unique id of the concrete class
    rtId: u32,
    /// rtSize  4   u32     Size of the data following the header
    rtSize: u32,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AssemblyScriptObject<'m> {
    ptr: *const u8,
    _marker: PhantomData<&'m ()>,
}

impl<'m> AssemblyScriptObject<'m> {
    pub(crate) fn from_memory(memory: &'m [u8], ptr: u32) -> Option<Self> {
        let offset = ptr as usize;
        if offset > memory.len() {
            return None;
        }
        let header_size = std::mem::size_of::<AssemblyScriptHeader>();
        let header_offset = offset.checked_sub(header_size)?;
        let header_ptr = memory[header_offset..offset].as_ptr() as *const AssemblyScriptHeader;
        let header = if header_ptr.is_aligned() {
            // Safe to be dereferenced because we have a shared ref to data, but
            // lifetime is toxic outside of this function.
            unsafe { &*header_ptr }
        } else {
            // Don't think this can ever happen in current AssemblyScript
            return None;
        };
        let size = header.rtSize as usize;
        if offset + size > memory.len() {
            return None;
        }
        let ptr = memory[offset..].as_ptr() as *const _;
        Some(Self {
            ptr,
            _marker: PhantomData,
        })
    }

    #[inline]
    fn header(self) -> &'m AssemblyScriptHeader {
        let header_ptr = unsafe { self.ptr.sub(size_of::<AssemblyScriptHeader>()) };
        unsafe { &*(header_ptr as *const _) }
    }

    #[inline]
    pub(crate) fn payload(self) -> &'m [u8] {
        let len = self.header().rtSize as usize;
        unsafe { std::slice::from_raw_parts(self.ptr, len) }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct AssemblyScriptString<'m> {
    inner: AssemblyScriptObject<'m>,
}

impl<'m> AssemblyScriptString<'m> {
    pub(crate) fn from_memory(memory: &'m [u8], ptr: u32) -> Option<Self> {
        let obj = AssemblyScriptObject::from_memory(memory, ptr)?;
        if obj.header().rtId == AS_CLASS_ID_STRING {
            Some(Self { inner: obj })
        } else {
            None
        }
    }

    fn string(self) -> String {
        // payload pointer is aligned because header is aligned
        let (prefix, mid, _) = unsafe { self.inner.payload().align_to::<u16>() };
        if prefix.is_empty() {
            String::from_utf16_lossy(mid)
        } else {
            unreachable!();
        }
    }
}

impl Display for AssemblyScriptString<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.string())
    }
}

pub(crate) fn env_abort<T>(
    mut caller: Caller<'_, T>,
    message_ptr: u32,
    file_name_ptr: u32,
    line: u32,
    column: u32,
) -> WResult<()> {
    let memory = get_memory(&mut caller)?.data(&mut caller);
    let message =
        AssemblyScriptString::from_memory(memory, message_ptr).ok_or(Error::InvalidPointer)?;
    let file_name =
        AssemblyScriptString::from_memory(memory, file_name_ptr).ok_or(Error::InvalidPointer)?;
    println!("env.abort {message} {file_name}:{line}:{column}");
    Err(Trap::Interrupt.into())
}
