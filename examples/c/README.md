# Building Wotto modules with C

⚠️ Note: this is all very preliminary and expected to change drastically.

You will need:
- `wotto.h` (included here)
- LLVM with clang and wasm32 support (see below if unsure)

## Which LLVM

The examples have been tested with clang 16.0.0.

You will need LLVM with both clang and support for the wasm32 target. You can
verify this with `clang -print-targets`:

```sh
$ clang -print-targets

  Registered Targets:

    ...
    wasm32      - WebAssembly 32-bit
    ...

```

Depending on your platform, you might have to build LLVM yourself. Binary
releases are also available (e.g. [llvm 16.0.0 on
GitHub](https://github.com/llvm/llvm-project/releases/tag/llvmorg-16.0.0)).

### macOS notes

The LLVM version shipped by Apple (e.g. with Xcode or xcode-select) does not
come with WebAssembly support.

I'm using Homebrew to install it:

```sh
$ brew install llvm
```

Note that this is a rather big install (1.5 GB on my machine) and can take some
time to download and build. It includes support for basically all LLVM targets,
including wasm32.

Some people recommend to `brew link llvm`, but I prefer not to, because I want
to explicitly decide whether I use Homebrew LLVM or Xcode LLVM. This means that
in order to invoke the compiler I need to do something like:

```sh
$ /opt/homebrew/opt/llvm/bin/clang # ...
```

## Writing a module

Hello world:

```c
#include "wotto.h"

WottoFunction(hello)
{
    output("hello world!", 12);
}
```

Commands are implemented as functions defined with the `WottoFunction` macro.
The name specified here defines both the function name and the export name.

The function doesn't take any argument and doesn't have a return value.
Instead, the `input()` and `output()` functions can be used, defined as:

```c
unsigned int input(u8 *buf, int len);
void output(const u8 *buf, int len);
```

TODO: include documentation. Refer to [`wotto.h`](wotto.h) for now.


## Compiling

Assuming we have the above example as a file called `hello.c` we can now
compile it to a wasm module:

```sh
$ path/to/clang --target=wasm32 -mbulk-memory \
                -nostdlib -Wl,--no-entry -g \
                -o hello.wasm hello.c
```

If compilation is successful, clang will create a `hello.wasm` file. This is
it, this is the WebAssembly module. You can load it into Wotto, and run the
`hello` command you have defined. If you defined multiple commands, they will
all be included in the same module.

### clang arguments

Some explanation about the command line arguments:

* `--target=wasm32` selects the wasm32 target, which will compile wasm32
  instructions, use wasm32 object files, and link into a WebAssembly module

* `-mbulk-memory` enables an [extension to
  WebAssembly](https://github.com/WebAssembly/bulk-memory-operations/blob/master/proposals/bulk-memory-operations/Overview.md)
  to support fast memory operations. This is almost always necessary, because
  clang will generate code that implicitly uses memcpy. Without this, you would
  need to implement memcpy yourself.

* `-nostdlib` compiles without the C standard library. See below for details.

* `-Wl,--no-entry` without this, the linker will complain that no entry symbol
  is defined. As we are building a library, we can omit an entry point.

* `-g` includes DWARF debug information in the module. This information will
  help to get nicer stack traces with references to line numbers. This is not
  mandatory, and you can omit it if you want a smaller binary, (or don't want
  to expose internal file and function names).

Everything else is clang as usual (if you are more familiar with gcc, clang
takes pretty much the same set of arguments). Some additional arguments that
will be useful at some point:

* `-z stack-size=65536` sets the amount of linear memory space that is reserved
  for the llvm stack. The default might change across clang/llvm versions. On
  my setup it defaults to 65536 bytes, which corresponds to one WebAssembly
  page. Note that the stack is not used for all variables, as in usual hardware
  architectures. WebAssembly is a stack machine, and has a separate stack for
  function calls and locals, which is not limited by this configuration. Not
  all locals go in the native WebAssembly stack (e.g. arrays and objects wider
  than 64 bit). For this purpose, clang manages a parallel stack in the linear
  memory, with a fixed maximum size, determined here.

  Usually you want this to be large enough for your program, but not much
  larger, because this space is allocated upfront and concurs to the memory
  limit the runtime enforces.

* `-Wl,--stack-first` is just a safeguard to detect stack overflows. It puts
  the space reserved for the stack (see above) at the beginning of the linear
  memory space, so that any access beyond the reserved size will trigger a
  trap at runtime, instead of smashing globals or static data.

* The usual optimization options, especially `-Os`, will be useful to reduce
  the binary size. This can be relevant if you want it to be loaded over the
  network, and if Wotto ever enforces a size limit for modules (currently it
  doesn't). For the same reason, you might want link-time optimization options,
  such as `-flto -Wl,--lto-O3`. Note that either might render debug information
  provided by `-g` useless

## Standard library and third party libraries

As mentioned [above](#compiling), we are currently compiling without libc. This
means no standard library, which can be a pretty huge limitation.

One exception: memcpy is included, because it's a llvm compiler builtin, not a
library function.

Nothing forbids you to include e.g. string.h and link with your favorite libc,
or any other library. This is not well tested, and there might be cases where
it doesn't work out of the box. This is expected to improve later on.

The only obvious limitation is that you cannot _dynamically_ link other
libraries, because of how WebAssembly works. Something akin to dynamic
libraries would be possible in theory. But it's not included in the current
Wotto runtime design. It might be in the future.

Static libraries and source code libraries are fine in principle, but might not
fit well if they expect a working libc.

## Dynamic memory allocation

Since there is no libc, there are no malloc/free. You can try linking a libc,
but this is not tested.

You can bring your own allocator. Since the runtime is single-threaded, any
allocator made for embedded should work fine. You can even build a very simple
one without free, since modules are made to be short-lived (any invocation will
start with a clean slate).

You have two ways to reserve heap space. The easiest is the one you will
probably not love:

```c
char heap[524288];
```

Static data is in fact a very effective way to make sure you have your space
usage under control. Remember that the runtime can enforce a hard limit on
memory usage.

In the future Wotto will provide a more explicit memory API. Before that, it's
worth noting that WebAssembly comes with native instructions to query the size
of the linear memory space, and to request it to grow. These instructions are
exposed as clang intrinsics which you might use:

```c
// Request a given number of pages (65KiB each). Return the previous memory
// size if successful, or ((size_t)-1) on error. mem must always be 0.
// (Corresponds to the memory.grow instruction)
size_t __builtin_wasm_memory_grow(unsigned int mem, size_t pages);

// Query the current linear memory size. mem must always be 0.
// (Corresponds to the memory.size instruction)
size_t __builtin_wasm_memory_size(unsigned int mem);
```

Any address in the linear memory can be accessed both for read and write.

## Testing

Testing support is not complete. Different approaches are possible:

* Compile for a native platform using `wotto.c` as stub. Check the included
  example for a preliminary form of this.

* Design a test interface and use wotto-cli to run wasm tests. Still in idea
  stage.

* Same as above, but use the web platform to provide richer interaction. Again,
  only an idea.

## Examples

A very limited example is provided:

* `foo.c` implements some example commands which manipulate strings without the
  standard library.

* `Makefile` shows compilations options both for wasm and native (for testing).


## More information

* [wasm-ld](https://lld.llvm.org/WebAssembly.html) (WebAssembly lld port, used
  implicitly by clang to link for wasm targets)

* [Compiling C to WebAssembly without Emscripten
  (2019-05-28)](https://surma.dev/things/c-to-webassembly/), blog post by Surma
  with an introduction and many insightful details
