#include "wotto.h"
#include "foo.h"

/// Always crash
WottoFunction(crash)
{
    size_t accumulator = 0;
    char *max_ptr = (char *)0x10000000;
    for (char *bad_ptr = 0; bad_ptr < max_ptr; bad_ptr++)
    {
        accumulator += *bad_ptr;
    }
    // this should crash before, but want to ensure code is not optimized
    // away, so we use the result of the loop in some way
    output((u8 *)"", accumulator);
}

/// Reverse a string
///
/// Reverse the given input string. Understands UTF-8 but doesn't respect
/// grapheme clusters, so "abc 🐕" will be reversed correctly but "🇮🇹" will not.
WottoFunction(rev)
{
    u8 buf[512];
    unsigned int len = input(buf, 512);
    reverse_utf8(buf, len);
    output(buf, len);
}

/// Show the codepoints that make up the input string.
WottoFunction(cp)
{
    u8 buf[512];
    // u8 *buf = heap;
    unsigned int len = input(buf, 512);
    u8 *pos = buf;
    u8 *end = buf + len;
    while (1)
    {
        unsigned int cp = utf8_decode(&pos);
        output_u32(cp);
        if (pos < end)
        {
            output_char(' ');
        }
        else
        {
            break;
        }
    }
}

/* utility functions */

/// Write a number as a decimal string in the given buffer
/// @param n the number to convert
/// @param buf a pointer to the output string buffer
/// @param len the size of the buffer
/// @return the number of bytes written to the buffer
size_t u32_to_str(unsigned int n, u8 *buf, size_t len)
{
    u8 digits[16] = {0};
    u8 pos = 0;
    while (n)
    {
        unsigned int r = n % 10;
        digits[pos] = r;
        n /= 10;
        pos += 1;
    }
    size_t actual_len = pos;
    for (size_t i = 0; i < len; i++)
    {
        buf[i] = '0' + digits[pos - 1];
        pos -= 1;
    }
    return actual_len;
}

/// Append a single character to the output string
/// @param c the character to append
void output_char(u8 c)
{
    output(&c, 1);
}

/// Append a number to the output string in decimal form.
///
/// Uses @ref u32_to_str to convert the character before outputting it.
/// @param n the number to append
void output_u32(unsigned int n)
{
    u8 buf[10];
    size_t len = u32_to_str(n, buf, 10);
    output(buf, len);
}

const u8 UTF_8_TWO_BYTES = 0xc0;
const u8 UTF_8_THREE_BYTES = 0xe0;
const u8 UTF_8_FOUR_BYTES = 0xf0;

/// Classify a UTF-8 byte at the beginning of a sequence
/// @param c the byte to classify (must not be a continuation byte)
/// @return the number of bytes in the sequence (1 to 4)
unsigned int utf8_byte(u8 c)
{
    if (c < UTF_8_TWO_BYTES)
        return 1;
    else if (c < UTF_8_THREE_BYTES)
        return 2;
    else if (c < UTF_8_FOUR_BYTES)
        return 3;
    else
        return 4;
}

/// Decode a character from a UTF-8 string.
///
/// Reads a single codepoint from the given UTF-8 string and advances the
/// pointer to the next valid position.
/// @param str a mutable pointer to the string to read from; must be aligned
///        to the beginning of a UTF-8 sequence
/// @return the codepoint
inline unsigned int utf8_decode(u8 **str)
{
    u8 b1 = **str;
    if (b1 < UTF_8_TWO_BYTES)
    {
        (*str)++;
        return b1;
    }
    else if (b1 < UTF_8_THREE_BYTES)
    {
        (*str)++;
        u8 b2 = **str;
        (*str)++;
        return ((b1 & 0x1f) << 6) + (b2 & 0x3f);
    }
    else if (b1 < UTF_8_FOUR_BYTES)
    {
        (*str)++;
        u8 b2 = **str;
        (*str)++;
        u8 b3 = **str;
        (*str)++;
        return ((b1 & 0xf) << 12) + ((b2 & 0x3f) << 6) + (b3 & 0x3f);
    }
    else
    {
        (*str)++;
        u8 b2 = **str;
        (*str)++;
        u8 b3 = **str;
        (*str)++;
        u8 b4 = **str;
        (*str)++;
        return ((b1 & 0x7) << 18) + ((b2 & 0x3f) << 12) + ((b3 & 0x3f) << 6) + (b4 & 0x3f);
    }
}

/// Reverse a UTF-8 string of up to 512 bytes.
/// @param a the string to reverse
/// @param len the size of the string in bytes (must be less than 512)
void reverse_utf8(u8 *a, unsigned int len)
{
    u8 buf[512];

    size_t fwd = 0, bwd = len - 1;

    while (fwd < len)
    {
        u8 c = a[fwd];
        unsigned int seqlen = utf8_byte(c);
        for (unsigned int i = 0; i < seqlen; i++)
        {
            buf[bwd - seqlen + i + 1] = a[fwd + i];
        }
        fwd += seqlen;
        bwd -= seqlen;
    }

    memcpy(a, buf, len);
}
