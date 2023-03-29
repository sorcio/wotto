#include "rusto.h"

#ifndef __wasm32

#include <unistd.h>

#define MAX_OUTPUT ((size_t)512)
#define MAX_INPUT ((size_t)512)

u8 input_data[MAX_INPUT];
size_t input_length;

u8 output_data[MAX_OUTPUT];
size_t output_length;

unsigned int input(u8 *buf, int len) {
    if (len < input_length) {
        memcpy(buf, input_data, len);
    } else {
        memcpy(buf, input_data, input_length);
    }
    return input_length;
}

void output(const u8 *buf, int len) {
    write(STDERR_FILENO, "out: '", 6);
    write(STDERR_FILENO, buf, len);
    write(STDERR_FILENO, "'\n", 2);

    size_t new_length = output_length + len;
    if (new_length > MAX_OUTPUT) {
        write(STDERR_FILENO, "warning: discarding output bytes\n", 33);
        new_length = MAX_OUTPUT;
    }
    size_t actual_size = new_length - output_length;
    if (actual_size > 0) {
        memcpy(output_data + output_length, buf, actual_size);
    }
    output_length += actual_size;
}

size_t _strnlen(const char* str, size_t buflen) {
    size_t i;
    for (i = 0; i < buflen; i++) {
        if (str[i] == 0) {
            return i;
        }
    }
    return buflen;
}

// TODO: dynamically choose function
#define EXPORTED_FUNC cp
void EXPORTED_FUNC(void);

int main(int argc, const char* argv[]) {
    if (argc != 3) {
        write(STDERR_FILENO, "expected args: <function> <args>\n", 33);
        return 1;
    }

    // TODO: dynamically choose function
    void(*f)(void) = &EXPORTED_FUNC;

    input_length = _strnlen(argv[2], MAX_INPUT);
    memcpy(input_data, argv[2], input_length);
    output_length = 0;
    f();

    write(STDOUT_FILENO, "output:\n", 8);
    write(STDOUT_FILENO, output_data, output_length);
    write(STDOUT_FILENO, "\n", 1);
    return 0;
}

#endif // ifndef __wasm32
