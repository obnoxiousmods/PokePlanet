// Starting a battle against another player.
//
// The engine is not reimplemented and barely touched: this fills in the things the cable
// handshake would have agreed -- who the two players are and which slot each holds -- and
// then enters the ordinary link battle. Everything from the intro onwards is the game's own
// code, exchanging blocks through mmo_link.c instead of a wire.
//
// gLinkPlayers is filled here rather than exchanged. On hardware each side sends a
// LinkPlayerBlock and the link layer fills the table; that layer is dead on this port, and
// the server already knows both players well enough to say. Filling it locally also keeps
// the slots consistent with the ones the server assigned, which is what decides who runs
// the battle engine -- and getting that wrong means either both machines run it or neither
// does.

#include "global.h"
#include "battle.h"
#include "battle_setup.h"
#include "field_screen_effect.h"
#include "link.h"
#include "item.h"
#include "load_save.h"
#include "main.h"
#include "money.h"
#include "mmo_battle.h"
#include "mmo_deadman.h"
#include "mmo_link.h"
#include "mmo_text.h"
#include "net_client.h"
#include "overworld.h"
#include "script.h"
#include "sound.h"
#include "string_util.h"
#include "tv.h"
#include "constants/songs.h"
#include "constants/trainers.h"

static void CB2_ReturnFromMmoBattle(void);

// Fill one side of the table.
//
// `name` is ASCII from the server and has to be encoded; it is also allowed to be longer
// than the field, which holds PLAYER_NAME_LENGTH characters like every other name record in
// the save. MmoText_FromAscii truncates rather than overruns, and the battle text that
// matters goes through the display-name substitution anyway.
static void SeatPlayer(u8 slot, const char *name, u32 trainerId, u8 gender)
{
    struct LinkPlayer *p;

    if (slot >= MAX_LINK_PLAYERS)
        return;

    p = &gLinkPlayers[slot];
    memset(p, 0, sizeof(*p));
    p->version = gGameVersion;
    p->language = gGameLanguage;
    p->trainerId = trainerId;
    p->gender = gender;
    p->linkType = LINKTYPE_BATTLE;
    p->id = slot;
    MmoText_FromAscii(p->name, name, LINK_PLAYER_NAME_LENGTH + 1);
}

// Both players agreed and the server has assigned the slots. Go.
void MmoBattle_Start(const struct NetBattleStart *start)
{
    u8 mine = start->linkId;
    u8 theirs = mine ^ 1;

    if (mine >= 2)
        return;

    // The slot the server gave us, which GetMultiplayerId now answers with. Everything
    // below -- master election included -- reads it.
    Link_SetAssignedMultiplayerId(mine);

    SeatPlayer(mine, Net_GetPlayerName(), GetPlayerIDAsU32(), gSaveBlock2Ptr->playerGender);
    // The opponent's trainer id is not known here and is only used for cosmetics: which
    // battle theme plays, and the "trainer" half of a shininess check that a link opponent's
    // own game has already decided. Deriving it from their player id keeps it stable for
    // the length of the battle rather than changing every frame.
    SeatPlayer(theirs, start->opponentName, start->opponent, 0);

    // What the cable handshake would also have settled.
    //
    // gLocalLinkPlayerId is the slot the rest of the link code reads when it wants to know
    // which player this machine is, and gWirelessCommType decides whether the block layer
    // talks to the cable or to the wireless adapter. Left alone, the second is whatever the
    // last thing to touch it wanted -- and if it is not zero, GetBlockReceivedStatus asks
    // the RFU code about blocks that were delivered here instead, and the battle waits on an
    // answer that never comes.
    gLocalLinkPlayerId = mine;
    gWirelessCommType = 0;

    // The link layer normally sets this once both sides have introduced themselves. Nothing
    // will do it here, and CB2_HandleStartBattle waits on it before sending anything.
    gReceivedRemoteLinkPlayers = TRUE;

    // Blocks only flow while this is set, so it must come before the battle starts asking.
    MmoLink_BeginBattle();

    // Take the party and bag aside for the duration.
    //
    // A battle against another player is not supposed to cost anything: no experience is
    // awarded, and the fainting and the potions spent are undone when it ends. The game does
    // that by snapshotting both here and restoring them afterwards. Without it a player
    // walks away from every match with a hurt team and a lighter bag -- and autosave then
    // makes that permanent on the server, where no amount of healing undoes it.
    SavePlayerParty();
    LoadPlayerBag();

    PlayMapChosenOrBattleBGM(MUS_VS_TRAINER);
    gBattleTypeFlags = BATTLE_TYPE_LINK | BATTLE_TYPE_TRAINER;
    CleanupOverworldWindowsAndTilemaps();
    gTrainerBattleOpponent_A = TRAINER_LINK_OPPONENT;
    gMain.savedCallback = CB2_ReturnFromMmoBattle;
    SetMainCallback2(CB2_InitBattle);
}

static void CB2_ReturnFromMmoBattle(void)
{
    // Stop carrying blocks first. Anything still arriving belongs to a battle that is over,
    // and filing it would leave it waiting in the buffer to be read as the first block of
    // the next one.
    MmoLink_EndBattle();
    gReceivedRemoteLinkPlayers = FALSE;
    Link_ClearAssignedMultiplayerId();
    gBattleTypeFlags = 0;

    // Give back what the battle borrowed -- unless this is a real Deadman fight. In a Deadman world
    // outside a safezone a PvP battle is not free: a mon that faints dies like any other, so keep the
    // battle's result instead of restoring the snapshot, and bury the fallen. A Deadman battle inside
    // a Pokemon Center is safe and still costs nothing, and Normal-mode PvP is unchanged.
    if (MmoDeadman_IsActive() && !MmoDeadman_InSafezone())
    {
        MmoDeadman_OnBattleEnd();
        // The loser of a Deadman PvP fight drops everything they were carrying: pokedollars forfeit,
        // and the carried items (the Items pocket -- not key items, balls or TMs) fall to the ground
        // where anyone can pick them up. Money banked at a PC is untouched; that is the bank's point.
        if (gBattleOutcome == B_OUTCOME_LOST)
        {
            u32 i;
            u16 items[BAG_ITEMS_COUNT];
            u32 count = 0;

            SetMoney(&gSaveBlock1Ptr->money, 0);

            // Snapshot the item ids first: removing an item compacts the pocket underneath us.
            for (i = 0; i < BAG_ITEMS_COUNT; i++)
            {
                u16 id = gSaveBlock1Ptr->bagPocket_Items[i].itemId;
                if (id != ITEM_NONE)
                    items[count++] = id;
            }
            for (i = 0; i < count; i++)
            {
                u16 quantity = CountTotalItemQuantityInBag(items[i]);
                if (quantity == 0)
                    continue;
                Net_DropItem(items[i], quantity);
                RemoveBagItem(items[i], quantity);
            }
        }
        // Losing your last living Pokemon to another player is the end of the run just as surely as
        // a whiteout is. The battle-return path never passes through DoWhiteOut, so check here:
        // nothing alive anywhere means the server wipes the character and the game restarts fresh.
        if (MmoDeadman_TryHardReset())
        {
            DoSoftReset();
            return;
        }
    }
    else
    {
        LoadPlayerParty();
    }
    SavePlayerBag();

    // Return to the overworld through the local, non-link path -- not
    // CB2_ReturnToFieldFromMultiplayer.
    //
    // That multiplayer return rebuilds the field, which is why it was reached for first, but it
    // installs FieldCB_ReturnToFieldCableLink, whose very first step is
    // CreateTask_ReestablishCableClubLink: a handshake with a real cable partner. There is no
    // such partner here and there never will be -- the battle ran over a QUIC server, not a wire,
    // and MmoLink_EndBattle just tore the bridge down -- so that task never finishes, the fade-in
    // that waits on it never runs, and both clients sit on the filled-black screen forever. That
    // was the bug: the battle ended correctly, but the way home was waiting on a cable.
    //
    // Nothing needs re-establishing, so come back the way a single-player battle does.
    // CB1_Overworld (rather than CB1_OverworldLink) makes CB2_ReturnToField take its local
    // branch, which clears the battle's vblank/hblank callbacks and rebuilds the torn-down map,
    // tilesets and object events exactly as an ordinary battle's return does;
    // FieldCB_WarpExitFadeFromBlack fades in and restores the map music with no link wait anywhere
    // in it.
    SetMainCallback1(CB1_Overworld);
    ResetAllMultiplayerState();
    gFieldCallback = FieldCB_WarpExitFadeFromBlack;
    ScriptContext_Init();
    UnlockPlayerFieldControls();
    CB2_ReturnToField();
}
