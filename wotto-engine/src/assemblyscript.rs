//! AssemblyScript support.

use std::fmt::Display;
use std::mem::{align_of, size_of};
use std::ops::Range;

use wasmtime::{Caller, Trap};

use crate::service::{get_memory, Error, WResult};

#[allow(dead_code)]
const AS_CLASS_ID_OBJECT: u32 = 0;
#[allow(dead_code)]
const AS_CLASS_ID_BUFFER: u32 = 1;
const AS_CLASS_ID_STRING: u32 = 2;

// AssemblyScript managed object references point at the payload. The runtime
// header is the 20 bytes immediately before that pointer:
//
//   -20: mmInfo  (usize on wasm32)
//   -16: gcInfo  (usize on wasm32)
//   -12: gcInfo2 (usize on wasm32)
//    -8: rtId    (u32)
//    -4: rtSize  (u32)
//
// Keep these as byte offsets into linear memory rather than overlaying a Rust
// struct on the wasm bytes. Wasm memory is little-endian and may be unaligned
// relative to host-side Rust types.
const AS_HEADER_SIZE: usize = 20;
const AS_HEADER_MM_INFO_OFFSET: usize = 0;
const AS_HEADER_GC_INFO_OFFSET: usize = 4;
const AS_HEADER_GC_INFO2_OFFSET: usize = 8;
const AS_HEADER_RT_ID_OFFSET: usize = 12;
const AS_HEADER_RT_SIZE_OFFSET: usize = 16;

#[allow(dead_code, non_snake_case)]
#[derive(Clone, Copy, Debug)]
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

impl AssemblyScriptHeader {
    const SIZE: usize = AS_HEADER_SIZE;

    fn from_bytes(bytes: &[u8; Self::SIZE]) -> Self {
        Self {
            mmInfo: read_u32_le(bytes, AS_HEADER_MM_INFO_OFFSET),
            gcInfo: read_u32_le(bytes, AS_HEADER_GC_INFO_OFFSET),
            gcInfo2: read_u32_le(bytes, AS_HEADER_GC_INFO2_OFFSET),
            rtId: read_u32_le(bytes, AS_HEADER_RT_ID_OFFSET),
            rtSize: read_u32_le(bytes, AS_HEADER_RT_SIZE_OFFSET),
        }
    }
}

fn read_u32_le(header: &[u8; AS_HEADER_SIZE], offset: usize) -> u32 {
    u32::from_le_bytes(
        header[offset..offset + size_of::<u32>()]
            .try_into()
            .expect("u32 field offset must fit in AssemblyScript header"),
    )
}

fn checked_object_range(header_offset: usize, offset: usize, size: usize) -> Option<Range<usize>> {
    let end = offset.checked_add(size)?;
    Some(header_offset..end)
}

fn is_linear_memory_offset_aligned_for<T>(offset: usize) -> bool {
    offset % align_of::<T>() == 0
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AssemblyScriptObject<'m> {
    bytes: &'m [u8],
}

impl<'m> AssemblyScriptObject<'m> {
    pub(crate) fn from_memory(memory: &'m [u8], ptr: u32) -> Option<Self> {
        let offset = ptr as usize;
        if offset > memory.len() {
            return None;
        }
        let header_size = AssemblyScriptHeader::SIZE;
        let header_offset = offset.checked_sub(header_size)?;
        let header_bytes = memory.get(header_offset..offset)?.as_array()?;
        let header = AssemblyScriptHeader::from_bytes(header_bytes);
        let size = header.rtSize as usize;
        let object_range = checked_object_range(header_offset, offset, size)?;
        let bytes = memory.get(object_range)?;
        Some(Self { bytes })
    }

    #[inline]
    fn header(self) -> AssemblyScriptHeader {
        let header_bytes = self.bytes[..AssemblyScriptHeader::SIZE]
            .as_array()
            .expect("validated AssemblyScript object always contains a full header");
        AssemblyScriptHeader::from_bytes(header_bytes)
    }

    #[inline]
    pub(crate) fn payload(self) -> &'m [u8] {
        &self.bytes[AssemblyScriptHeader::SIZE..]
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct AssemblyScriptString<'m> {
    inner: AssemblyScriptObject<'m>,
}

impl<'m> AssemblyScriptString<'m> {
    pub(crate) fn from_memory(memory: &'m [u8], ptr: u32) -> Option<Self> {
        let offset = ptr as usize;
        let obj = AssemblyScriptObject::from_memory(memory, ptr)?;
        if obj.header().rtId == AS_CLASS_ID_STRING
            && obj.payload().len() % size_of::<u16>() == 0
            && is_linear_memory_offset_aligned_for::<u16>(offset)
        {
            Some(Self { inner: obj })
        } else {
            None
        }
    }

    fn string(self) -> String {
        let utf16: Vec<_> = self
            .inner
            .payload()
            .chunks_exact(size_of::<u16>())
            .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
            .collect();
        String::from_utf16_lossy(&utf16)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::{HasInput, HasOutput};
    use wasmtime::{Engine, Linker, Module, Store};

    #[derive(Default)]
    struct TestRuntimeData {
        output: String,
    }

    impl HasInput for TestRuntimeData {
        fn input(&self) -> &str {
            ""
        }
    }

    impl HasOutput for TestRuntimeData {
        fn output(&mut self, text: &str) {
            self.output.push_str(text);
        }
    }

    fn header_bytes(rt_id: u32, rt_size: u32) -> [u8; AssemblyScriptHeader::SIZE] {
        let mut header = [0; AssemblyScriptHeader::SIZE];
        header[AS_HEADER_RT_ID_OFFSET..AS_HEADER_RT_SIZE_OFFSET]
            .copy_from_slice(&rt_id.to_le_bytes());
        header[AS_HEADER_RT_SIZE_OFFSET..AS_HEADER_SIZE].copy_from_slice(&rt_size.to_le_bytes());
        header
    }

    fn write_header(memory: &mut [u8], ptr: usize, rt_id: u32, rt_size: u32) {
        let header_start = ptr - AssemblyScriptHeader::SIZE;
        memory[header_start..ptr].copy_from_slice(&header_bytes(rt_id, rt_size));
    }

    #[test]
    fn decodes_valid_assemblyscript_string() {
        let ptr = AssemblyScriptHeader::SIZE;
        let mut memory = vec![0; ptr + 4];
        write_header(&mut memory, ptr, AS_CLASS_ID_STRING, 4);
        memory[ptr..ptr + 4].copy_from_slice(&[b'o', 0, b'k', 0]);

        let text = AssemblyScriptString::from_memory(&memory, ptr as u32)
            .expect("valid AssemblyScript string")
            .string();

        assert_eq!(text, "ok");
    }

    #[test]
    fn rejects_payload_extending_past_memory() {
        let ptr = AssemblyScriptHeader::SIZE;
        let mut memory = vec![0; ptr + 4];
        write_header(&mut memory, ptr, AS_CLASS_ID_STRING, 6);

        assert!(AssemblyScriptString::from_memory(&memory, ptr as u32).is_none());
    }

    #[test]
    fn rejects_odd_sized_string_payload() {
        let ptr = AssemblyScriptHeader::SIZE;
        let mut memory = vec![0; ptr + 1];
        write_header(&mut memory, ptr, AS_CLASS_ID_STRING, 1);

        assert!(AssemblyScriptString::from_memory(&memory, ptr as u32).is_none());
    }

    #[test]
    fn checked_object_range_rejects_overflow() {
        assert_eq!(
            checked_object_range(0, usize::MAX - 1, 1),
            Some(0..usize::MAX)
        );
        assert_eq!(checked_object_range(0, usize::MAX - 1, 2), None);
    }

    #[test]
    fn rejects_unaligned_string_payload() {
        let ptr = AssemblyScriptHeader::SIZE + 1;
        let mut memory = vec![0; ptr + 2];
        write_header(&mut memory, ptr, AS_CLASS_ID_STRING, 2);
        memory[ptr..ptr + 2].copy_from_slice(&[b'x', 0]);

        assert!(AssemblyScriptString::from_memory(&memory, ptr as u32).is_none());
    }

    #[test]
    fn malicious_wasm_print_pointer_cannot_read_out_of_bounds() {
        let engine = Engine::default();
        let mut linker = Linker::new(&engine);
        crate::runtime::add_to_linker(&mut linker, true).expect("link host imports");

        let module = Module::new(
            &engine,
            r#"
            (module
                (import "wotto" "print" (func $print (param i32)))
                (memory (export "memory") 1)
                (func (export "bad_print")
                    i32.const 32
                    call $print))
            "#,
        )
        .expect("compile malicious wasm fixture");

        let mut store = Store::new(&engine, TestRuntimeData::default());
        let instance = linker
            .instantiate(&mut store, &module)
            .expect("instantiate malicious wasm fixture");
        let memory = instance
            .get_memory(&mut store, "memory")
            .expect("fixture exports memory");
        memory
            .write(
                &mut store,
                32 - AssemblyScriptHeader::SIZE,
                &header_bytes(AS_CLASS_ID_STRING, u32::MAX),
            )
            .expect("write crafted AssemblyScript header");

        let bad_print = instance
            .get_typed_func::<(), ()>(&mut store, "bad_print")
            .expect("fixture exports bad_print");
        let err = bad_print
            .call(&mut store, ())
            .expect_err("out-of-bounds payload must be rejected");

        assert!(
            err.chain()
                .any(|cause| cause.to_string().contains("invalid pointer")),
            "unexpected error: {err:?}"
        );
    }
}
