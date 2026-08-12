// Deadman Mode: the permadeath world.
//
// A fainted Pokemon dies forever. When a battle fought outside a Pokemon Center ends, every mon
// that fainted is moved to a reserved read-only "Graveyard" box and cleared from the party. The
// server independently records the dead and refuses any report that revives one, so this is only
// the client half of a rule the server enforces -- a tampered client can hide a death from itself
// but never bring the corpse back into a battle the server would accept.

#include "global.h"
#include "pokemon.h"
#include "pokemon_storage_system.h"
#include "save_location.h"
#include "platform.h"
#include "mmo_deadman.h"
#include "constants/species.h"

// The last PC box is the graveyard: a reserved box the dead are laid in and never leave. Reusing an
// existing box rather than adding a fifteenth keeps sizeof(struct PokemonStorage) -- and therefore
// the server's whole-storage report contract -- unchanged.
#define MMO_GRAVEYARD_BOX (TOTAL_BOXES_COUNT - 1)

bool8 MmoDeadman_IsActive(void)
{
    return Platform_IsDeadman();
}

bool8 MmoDeadman_InSafezone(void)
{
    return IsCurMapPokeCenter();
}

// Move every fainted party mon to the graveyard, forever, then compact the party.
static void BuryFaintedParty(void)
{
    u8 i;
    bool8 buried = FALSE;

    for (i = 0; i < PARTY_SIZE; i++)
    {
        if (GetMonData(&gPlayerParty[i], MON_DATA_SPECIES) == SPECIES_NONE)
            continue;
        if (GetMonData(&gPlayerParty[i], MON_DATA_IS_EGG))
            continue;
        if (GetMonData(&gPlayerParty[i], MON_DATA_HP) != 0)
            continue;

        // Fainted outside a safezone: it dies. Lay the corpse in the graveyard if there is room --
        // a full graveyard just means the body is lost -- then clear the party slot.
        {
            s16 pos = GetFirstFreeBoxSpot(MMO_GRAVEYARD_BOX);
            if (pos >= 0)
                SetBoxMonAt(MMO_GRAVEYARD_BOX, (u8)pos, &gPlayerParty[i].box);
        }
        ZeroMonData(&gPlayerParty[i]);
        buried = TRUE;
    }

    if (buried)
    {
        CompactPartySlots();
        CalculatePlayerPartyCount();
    }
}

void MmoDeadman_OnBattleEnd(void)
{
    if (!MmoDeadman_IsActive())
        return;
    // A Pokemon Center is the one safezone: a battle there never kills.
    if (MmoDeadman_InSafezone())
        return;
    BuryFaintedParty();
}
