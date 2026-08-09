#ifndef GUARD_MMO_TEXT_H
#define GUARD_MMO_TEXT_H

// Converting plain text from the network into the game's own character encoding.
//
// Everything the server sends -- Discord display names, chat lines -- is UTF-8 from the
// outside world, but the game's text renderer reads a bespoke single-byte charmap. These
// helpers bridge the two and guarantee a terminated, bounded result.

#include "global.h"

// Convert an ASCII C string into game-encoded text.
//
// `destSize` counts the EOS terminator, so at most destSize - 1 characters are written.
// Characters with no charmap equivalent become spaces, which keeps foreign names legible
// as spacing rather than turning them into garbage glyphs.
// Returns the number of characters written, excluding the terminator.
u8 MmoText_FromAscii(u8 *dest, const char *src, u8 destSize);

#endif // GUARD_MMO_TEXT_H
