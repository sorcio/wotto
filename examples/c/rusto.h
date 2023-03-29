#pragma once

#include <stddef.h>

#ifdef __wasm32

#define RustoFunction(name) __attribute__((export_name(#name))) void name(void)
#define RUSTO_IMPORT(module, name) __attribute__((import_module(#module), import_name(#name)))

#else // ifdef __wasm32

#define RustoFunction(name) void name(void)
#define RUSTO_IMPORT(module, name)

#endif // ifdef __wasm32

// so that we can compile with -nostdlib:

// Copy n bytes from src to dst.
void *memcpy(void *restrict dst, const void *restrict src, size_t n);

typedef unsigned char u8;

// Read the input string into buf. At most len bytes will be copied. Return the
// length of the input string.
//
// The return value can be larger than `len`, indicating the length of the
// entire string.
// 
// The input string is always encoded as UTF-8 bytes.
RUSTO_IMPORT(rusto, input) unsigned int input(u8 *buf, int len);

// Write len bytes from buf into the output string. Subsequent calls will
// append to the output.
//
// The bytes must represent UTF-8 text. The runtime can validate that the text
// is properly encoded, and could either reject invalid data, or replace
// sequences that cannot be decoded with the Unicode replacement character
// (FFFD). Nevertheless, this function will always succeed and not report an
// error.
//
// The runtime can implement a limit on the size of the output, typically in
// number of bytes (typically 512). This function will not report an error if
// the limit is exceeded, but the output will be truncated. If the output is
// truncated in the middle of a UTF-8 sequence, this can result in invalid
// UTF-8, which is handled as above.
//
// TODO: there is currently no way to inspect the limit
//
// You must expect output to be shown only after the command returns. There is
// currently no facility to stream output.
RUSTO_IMPORT(rusto, output) void output(const u8 *buf, int len);
