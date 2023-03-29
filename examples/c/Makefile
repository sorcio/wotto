.PHONY: native wasm

# currently this is not auto-configured, it just assumes whatever worked on my
# machine, so you might have to change some stuff around this makefile.

# ideally we use the same compiler for both native and wasm:
CC = /opt/homebrew/opt/llvm/bin/clang

native: a.out

wasm: foo.wasm

foo.wasm: foo.c
	"$(CC)" -Wall -pedantic --target=wasm32 -mbulk-memory -nostdlib -Wl,--no-entry -o foo.wasm -Oz -Wl,--stack-first -flto -Wl,--lto-O3 -z stack-size=65536 -g foo.c

# native only stuff:

rusto.o: rusto.c
	"$(CC)" -Wall -pedantic -nostdlib -g -c -o rusto.o rusto.c -I$$(xcrun --sdk macosx --show-sdk-path)/usr/include

foo.o: foo.c
	"$(CC)" -Wall -pedantic -nostdlib -g -c -o foo.o foo.c -I$$(xcrun --sdk macosx --show-sdk-path)/usr/include

a.out: rusto.o foo.o	
	"$(CC)" -fuse-ld=lld -v rusto.o foo.o -nostdlib -lSystem -o a.out -Wl,-L$$(xcrun --sdk macosx --show-sdk-path)/usr/lib
