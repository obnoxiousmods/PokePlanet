#ifndef GUARD_MMO_NAMEPLATES_H
#define GUARD_MMO_NAMEPLATES_H

// A floating name tag over each other player's overworld sprite. Presentation only: the name is
// already known per slot in mmo_players; this renders it and follows the sprite.

// Reset the tag bookkeeping at boot.
void MmoNameplates_Init(void);

// Forget every tag without touching sprites, for a map change where the sprites are already gone.
void MmoNameplates_Reset(void);

// Ensure the tag for a slot exists, follows `objectEventId`, and shows `name` and how many Pokémon
// the player carries. Cheap to call every frame -- it only re-renders when the name or count
// changes.
void MmoNameplates_Set(u8 slot, u8 objectEventId, const char *name, u8 partyCount);

// Drop the tag for a slot whose player has left the map or disconnected.
void MmoNameplates_Clear(u8 slot);

#endif // GUARD_MMO_NAMEPLATES_H
