#ifndef GUARD_OBJ_PALETTE_EXT_H
#define GUARD_OBJ_PALETTE_EXT_H

// Sprite palettes beyond the sixteen the hardware had.
//
// The GBA gave objects sixteen palettes of sixteen colours, and the overworld spends them:
// two on the player and their reflection, eight on four NPC slots and their reflections, and
// the rest on specials. That leaves four for every other character on screen, which is why
// two remote players who share a slot repaint each other -- PatchObjectPalette calls
// LoadPalette unconditionally, with no reference counting and no notion of who is using what.
//
// None of that is hardware here. This port draws sprites in software and PLTT is an ordinary
// array, so the ceiling is a convention rather than a limit, and an MMO has no business
// rationing four palettes between everyone in a town.
//
// Rather than widening OamData's four-bit palette field -- which every system in the game
// writes, for the same result -- a sprite may name a palette out of a bank of its own. The
// renderers consult that first and fall back to the hardware behaviour, so anything that has
// not opted in is untouched.

#define OBJ_PALETTE_EXT_COUNT 64

// Sixteen colours each, in the same 15-bit BGR the hardware palette uses.
extern u16 gObjPaletteExt[OBJ_PALETTE_EXT_COUNT][16];

// Which extended palette each OAM entry draws with: 0 for none, otherwise 1 + the index.
// Written alongside the OAM buffer so it is indexed identically.
extern u8 gObjPaletteExtSlot[128];

#endif // GUARD_OBJ_PALETTE_EXT_H
