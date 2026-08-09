// ASCII to game-charmap conversion.
//
// The byte values below come from charmap.txt, which the build uses to encode the game's
// own string literals. The alphanumeric runs are contiguous there, so they convert with
// arithmetic; the handful of punctuation marks worth supporting are listed explicitly.

#include "global.h"
#include "mmo_text.h"
#include "constants/characters.h"

static u8 EncodeChar(char c)
{
    // The alphanumeric runs are contiguous in the charmap, so offset arithmetic is exact.
    if (c >= 'A' && c <= 'Z')
        return CHAR_A + (c - 'A');
    if (c >= 'a' && c <= 'z')
        return CHAR_a + (c - 'a');
    if (c >= '0' && c <= '9')
        return CHAR_0 + (c - '0');

    switch (c)
    {
    case ' ':  return CHAR_SPACE;
    case '!':  return CHAR_EXCL_MARK;
    case '?':  return CHAR_QUESTION_MARK;
    case '.':  return CHAR_PERIOD;
    case '-':  return CHAR_HYPHEN;
    case ',':  return CHAR_COMMA;
    case ':':  return CHAR_COLON;
    // Anything else -- emoji, accented letters, symbols the font has no glyph for --
    // becomes a space. A name rendered with gaps is readable; one rendered with
    // arbitrary charmap bytes would be visual noise.
    default:   return CHAR_SPACE;
    }
}

u8 MmoText_FromAscii(u8 *dest, const char *src, u8 destSize)
{
    u8 written = 0;

    if (dest == NULL || destSize == 0)
        return 0;

    if (src != NULL)
    {
        while (written < destSize - 1 && src[written] != '\0')
        {
            dest[written] = EncodeChar(src[written]);
            written++;
        }
    }
    dest[written] = EOS;
    return written;
}
