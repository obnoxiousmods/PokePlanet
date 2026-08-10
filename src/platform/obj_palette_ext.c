// Storage for the extended sprite palettes. See obj_palette_ext.h for why they exist.
//
// Deliberately plain arrays with no allocator here: what is contentious is who gets a
// palette, not where the bytes live, and putting the policy next to the storage would make
// the renderers depend on it.

#include "global.h"
#include "obj_palette_ext.h"
#include "constants/rgb.h"

u16 gObjPaletteExt[OBJ_PALETTE_EXT_COUNT][16];
u16 gObjPaletteExtUnfaded[OBJ_PALETTE_EXT_COUNT][16];

void ObjPaletteExt_Load(u8 slot, const u16 *colours)
{
    u8 i;

    if (slot >= OBJ_PALETTE_EXT_COUNT || colours == NULL)
        return;

    // Both copies, so a palette loaded mid-fade is not left bright until the next fade starts.
    for (i = 0; i < 16; i++)
    {
        gObjPaletteExtUnfaded[slot][i] = colours[i];
        gObjPaletteExt[slot][i] = colours[i];
    }
}

void ObjPaletteExt_ApplyFade(u8 coeff, u16 blendColor)
{
    // The same arithmetic as BlendPalette in src/util.c, over this bank instead of the
    // hardware buffers. Copied rather than shared because that function is written against
    // gPlttBuffer* by name, and giving it a source and destination would touch every caller
    // in the game for one new one here.
    u8 slot;
    u8 i;

    for (slot = 0; slot < OBJ_PALETTE_EXT_COUNT; slot++)
    {
        for (i = 0; i < 16; i++)
        {
            struct PlttData *from = (struct PlttData *)&gObjPaletteExtUnfaded[slot][i];
            struct PlttData *to = (struct PlttData *)&blendColor;
            s8 r = from->r;
            s8 g = from->g;
            s8 b = from->b;

            gObjPaletteExt[slot][i] = RGB(r + (((to->r - r) * coeff) >> 4),
                                          g + (((to->g - g) * coeff) >> 4),
                                          b + (((to->b - b) * coeff) >> 4));
        }
    }
}

// Zero means "draw this the way the hardware would", so an OAM entry nobody has assigned a
// palette to behaves exactly as before. That is what makes this safe to add ahead of anything
// using it.
u8 gObjPaletteExtSlot[128];
