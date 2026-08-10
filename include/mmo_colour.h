#ifndef GUARD_MMO_COLOUR_H
#define GUARD_MMO_COLOUR_H

// Per-player colours, derived from the character id rather than sent. See mmo_colour.c.

// Which of the looks this player has. Same answer on every client, for ever.
u8 MmoColour_ForPlayer(u32 playerId);

// Build this player's palette from `basePaletteNum` and return the extended slot to draw with,
// as the sprite's `paletteExtSlot` wants it: 0 means leave the sprite as the artwork intended.
u8 MmoColour_SlotFor(u32 playerId, u8 basePaletteNum);

#endif // GUARD_MMO_COLOUR_H
