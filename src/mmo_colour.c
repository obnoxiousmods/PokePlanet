// Giving every player their own colours.
//
// The overworld had four palette slots for everyone who is not the player, with no reference
// counting, so characters repainted each other and two people could not reliably look
// different. The renderer no longer has that ceiling (see obj_palette_ext.h); this is the part
// that decides who gets which colours.
//
// The colour is derived from the character id rather than sent. Every client computes the same
// answer for the same player, so nothing has to be stored, migrated, or kept in step across a
// wire -- and a character keeps its colours through a database restore, which anything stored
// alongside the character would not survive as reliably.
//
// The transform is a rotation of the red, green and blue channels plus a brightness tilt,
// applied to whatever palette the sprite already uses. That keeps the shading of the original
// artwork -- a recoloured trainer still reads as the same drawing in different clothes, where
// replacing colours outright tends to produce something flat.

#include "global.h"
#include "mmo_colour.h"
#include "obj_palette_ext.h"
#include "palette.h"
#include "sprite.h"
#include "constants/rgb.h"

// How many distinct looks there are.
//
// Chosen so that two players who collide look plainly different rather than nearly the same.
// Ten thousand players share twelve appearances, which is what an overworld of NPC sprites did
// anyway; the point is telling the person next to you apart, not being unique in the world.
#define MMO_COLOUR_COUNT 12

// Which extended palettes belong to players. Slot 0 is left alone as a "no colour" answer.
#define MMO_COLOUR_FIRST_SLOT 1

u8 MmoColour_ForPlayer(u32 playerId)
{
    // A cheap mix, so that ids one apart do not produce colours one apart -- consecutive
    // sign-ups would otherwise arrive looking like a gradient.
    u32 mixed = playerId * 2654435761u;

    return (u8)(mixed % MMO_COLOUR_COUNT);
}

// Build one player's palette from the sprite's own, and hand it to the renderer.
//
// Returns the extended slot to draw with, or 0 to leave the sprite as the artwork intended.
u8 MmoColour_SlotFor(u32 playerId, u8 basePaletteNum)
{
    u16 colours[16];
    u8 colour = MmoColour_ForPlayer(playerId);
    u8 slot = MMO_COLOUR_FIRST_SLOT + colour;
    u16 offset = OBJ_PLTT_OFFSET + basePaletteNum * 16;
    u8 i;

    if (slot >= OBJ_PALETTE_EXT_COUNT)
        return 0;

    // Colour 0 is the artwork as drawn, so one player in twelve looks exactly like the
    // original. That is deliberate: it keeps a recognisable baseline in the world rather than
    // making everyone equally unfamiliar.
    if (colour == 0)
        return 0;

    for (i = 0; i < 16; i++)
    {
        // Read the unfaded copy: the faded one is whatever the current transition left behind,
        // and building a palette from that would bake a fade into the colours permanently.
        u16 source = gPlttBufferUnfaded[offset + i];
        u16 r = source & 0x1F;
        u16 g = (source >> 5) & 0x1F;
        u16 b = (source >> 10) & 0x1F;
        u16 nr, ng, nb;

        switch (colour % 3)
        {
        case 1:  nr = b; ng = r; nb = g; break; // channels rotated one way
        case 2:  nr = g; ng = b; nb = r; break; // and the other
        default: nr = r; ng = g; nb = b; break; // left alone, but tilted below
        }

        // A tilt as well as a rotation, so that twelve looks are not four repeated three
        // times. Kept mild, and clamped, because a trainer sprite that loses its shading stops
        // reading as a person.
        switch (colour / 3)
        {
        case 1: nr = nr + (31 - nr) / 3; break;      // lighter reds
        case 2: nb = nb + (31 - nb) / 3; break;      // lighter blues
        case 3: nr = nr / 2; ng = ng + (31 - ng) / 4; break; // darker, greener
        default: break;
        }

        colours[i] = RGB(nr > 31 ? 31 : nr, ng > 31 ? 31 : ng, nb > 31 ? 31 : nb);
    }

    ObjPaletteExt_Load(slot, colours);
    return slot + 1; // the slot table is 1-based, 0 meaning "no colour"
}
