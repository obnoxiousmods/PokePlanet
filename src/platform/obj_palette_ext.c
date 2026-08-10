// Storage for the extended sprite palettes. See obj_palette_ext.h for why they exist.
//
// Deliberately plain arrays with no allocator here: what is contentious is who gets a
// palette, not where the bytes live, and putting the policy next to the storage would make
// the renderers depend on it.

#include "global.h"
#include "obj_palette_ext.h"

u16 gObjPaletteExt[OBJ_PALETTE_EXT_COUNT][16];

// Zero means "draw this the way the hardware would", so an OAM entry nobody has assigned a
// palette to behaves exactly as before. That is what makes this safe to add ahead of anything
// using it.
u8 gObjPaletteExtSlot[128];
