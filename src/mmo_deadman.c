// Deadman Mode: the permadeath world.
//
// A fainted Pokemon dies forever. When a battle fought outside a Pokemon Center ends, every mon
// that fainted is moved to a reserved read-only "Graveyard" box and cleared from the party. The
// server independently records the dead and refuses any report that revives one, so this is only
// the client half of a rule the server enforces -- a tampered client can hide a death from itself
// but never bring the corpse back into a battle the server would accept.

#include "global.h"
#include "event_data.h"
#include "pokemon.h"
#include "pokemon_storage_system.h"
#include "save_location.h"
#include "platform.h"
#include "mmo_deadman.h"
#include "constants/flags.h"
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

// Progression caps by badge count. Mirror server/src/deadman.rs -- the client stops the player at
// the same wall the server would refuse to cross, so a cap is felt as a limit rather than as a
// rejected report after the fact.
static const u8 sLevelCapByBadges[9] = { 15, 19, 24, 29, 31, 33, 42, 46, 58 };
static const u8 sPartyCapByBadges[9] = { 2, 2, 3, 3, 4, 4, 5, 5, 6 };

u8 MmoDeadman_BadgeCount(void)
{
    u8 count = 0;
    u16 flag;

    for (flag = FLAG_BADGE01_GET; flag < FLAG_BADGE01_GET + NUM_BADGES; flag++)
    {
        if (FlagGet(flag))
            count++;
    }
    return count;
}

u8 MmoDeadman_LevelCap(void)
{
    u8 badges = MmoDeadman_BadgeCount();
    return sLevelCapByBadges[badges > 8 ? 8 : badges];
}

u8 MmoDeadman_PartyCap(void)
{
    u8 badges = MmoDeadman_BadgeCount();
    return sPartyCapByBadges[badges > 8 ? 8 : badges];
}

bool8 MmoDeadman_OwnsSpecies(u16 species)
{
    u8 i;
    u8 box;
    u8 slot;

    // A living party member (fainted does not count -- it will die and free the species).
    for (i = 0; i < PARTY_SIZE; i++)
    {
        if (GetMonData(&gPlayerParty[i], MON_DATA_SPECIES) == species
         && GetMonData(&gPlayerParty[i], MON_DATA_HP) != 0)
            return TRUE;
    }

    // Any boxed copy, except in the graveyard: a boxed mon is always alive, a corpse never is.
    for (box = 0; box < TOTAL_BOXES_COUNT; box++)
    {
        if (box == MMO_GRAVEYARD_BOX)
            continue;
        for (slot = 0; slot < IN_BOX_COUNT; slot++)
        {
            if (GetBoxMonDataAt(box, slot, MON_DATA_SPECIES) == species)
                return TRUE;
        }
    }
    return FALSE;
}
