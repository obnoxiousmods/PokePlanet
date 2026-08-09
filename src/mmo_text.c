// ASCII to game-charmap conversion.
//
// The byte values below come from charmap.txt, which the build uses to encode the game's
// own string literals. The alphanumeric runs are contiguous there, so they convert with
// arithmetic; the handful of punctuation marks worth supporting are listed explicitly.

#include "global.h"
#include "mmo_text.h"
#include "net_client.h"
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

// The signed-in player's name at full length, in the game's charmap, or NULL when playing
// offline.
//
// The save format stores a trainer name seven characters wide and cannot be widened without
// changing the layout of a dozen unrelated subsystems -- see PLAYER_NAME_LENGTH. So the long
// name is never stored; it is substituted at the point of display, where the destination is
// gStringVar4 and length costs nothing.
//
// Rebuilt whenever the name changes rather than on every call, since {PLAYER} is expanded
// for practically every line of dialogue in the game.
const u8 *MmoText_PlayerDisplayName(void)
{
    static u8 sEncoded[NET_NAME_LEN + 1];
    static char sCachedFrom[NET_NAME_LEN];
    const char *name = Net_GetPlayerName();
    u8 i;

    if (name == NULL || name[0] == '\0')
        return NULL;

    // Re-encode only when the name actually changed, since {PLAYER} is expanded for
    // practically every line of dialogue in the game.
    for (i = 0; i < NET_NAME_LEN - 1; i++)
    {
        if (sCachedFrom[i] != name[i])
            break;
        if (name[i] == '\0')
            return sEncoded; // Matched all the way to the terminator.
    }

    if (i < NET_NAME_LEN - 1)
    {
        u8 j;

        for (j = 0; j < NET_NAME_LEN - 1 && name[j] != '\0'; j++)
            sCachedFrom[j] = name[j];
        sCachedFrom[j] = '\0';
        MmoText_FromAscii(sEncoded, sCachedFrom, sizeof(sEncoded));
    }
    return sEncoded;
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
