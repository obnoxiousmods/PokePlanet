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
#include "link.h"
#include "main.h"
#include "mmo_battle.h"
#include "mmo_link.h"
#include "mmo_text.h"
#include "net_client.h"
#include "overworld.h"
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
    MmoText_FromAscii(p->name, name, PLAYER_NAME_LENGTH + 1);
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

    // The link layer normally sets this once both sides have introduced themselves. Nothing
    // will do it here, and CB2_HandleStartBattle waits on it before sending anything.
    gReceivedRemoteLinkPlayers = TRUE;

    // Blocks only flow while this is set, so it must come before the battle starts asking.
    MmoLink_BeginBattle();

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

    SetMainCallback2(CB2_ReturnToField);
}
