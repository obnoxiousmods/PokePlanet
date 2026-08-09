// Draws other PokePlanet players in the overworld.
//
// Remote players are ordinary object events, so they inherit the engine's sprite
// handling, elevation, reflections and animation for free. The only thing this file does
// is keep a small table of slots in sync with the server's snapshot: spawn when someone
// appears, drive their movement, and despawn when they leave.
//
// Ticked once per overworld frame from OverworldBasic().

#include "global.h"
#include "event_object_movement.h"
#include "field_player_avatar.h"
#include "mmo_chat.h"
#include "mmo_players.h"
#include "mmo_text.h"
#include "net_client.h"
#include "script.h"
#include "string_util.h"
#include "constants/event_object_movement.h"

// Local IDs for remote players. Real maps number their object events from 1 and never
// come close to this, so these cannot collide with a map's own NPCs.
#define MMO_LOCAL_ID_BASE 200

// How many frames to keep trying a refused step before giving up and placing them where
// the server says. Long enough for someone to walk out of the way, short enough that a
// remote player never lingers visibly behind where they really are.
#define MMO_MAX_STALLED_STEPS 16

struct MmoSlot
{
    u32 playerId;      // 0 when the slot is free
    bool8 spawned;
    // Consecutive frames a one-tile step has been refused. Deliberately the only position
    // state kept here: the object event itself is where the sprite actually is.
    u8 stalledSteps;
};

static struct MmoSlot sSlots[NET_MAX_REMOTE_PLAYERS];
static u8 sCurrentMapGroup = 0xFF;
static u8 sCurrentMapNum = 0xFF;

static u8 LocalIdForSlot(u8 slot)
{
    return MMO_LOCAL_ID_BASE + slot;
}

// Direction to walk to reach an adjacent tile, or DIR_NONE if it is not adjacent.
static u8 StepDirection(s16 dx, s16 dy)
{
    if (dx == 0 && dy == 1)
        return DIR_SOUTH;
    if (dx == 0 && dy == -1)
        return DIR_NORTH;
    if (dx == -1 && dy == 0)
        return DIR_WEST;
    if (dx == 1 && dy == 0)
        return DIR_EAST;
    return DIR_NONE;
}

static struct ObjectEvent *FindSlotObject(u8 slot)
{
    u8 objectEventId;

    // Note the inverted convention: TryGetObjectEventIdByLocalIdAndMap returns TRUE when
    // the object was NOT found. Reading it the other way round made every spawned player
    // look missing, so the next frame tried to spawn them again and hit the duplicate
    // localId check forever.
    if (TryGetObjectEventIdByLocalIdAndMap(LocalIdForSlot(slot), sCurrentMapNum,
                                           sCurrentMapGroup, &objectEventId))
        return NULL;
    return &gObjectEvents[objectEventId];
}

static void DespawnSlot(u8 slot)
{
    struct ObjectEvent *object = sSlots[slot].spawned ? FindSlotObject(slot) : NULL;

    if (object != NULL)
    {
        // Deliberately not RemoveObjectEventByLocalIdAndMap: that calls
        // FlagSet(GetObjectEventFlagIdByObjectEventId(...)), and the template built by
        // SpawnSpecialObjectEventParameterized never initialises flagId, so it would set
        // an arbitrary story flag from stack garbage. Tear the sprite down directly.
        //
        // spriteId is only meaningful while the object is active and can be MAX_SPRITES
        // if creation failed, so it must be range checked before indexing gSprites.
        if (object->active && object->spriteId < MAX_SPRITES)
        {
            struct SpriteFrameImage image;

            image.size = GetObjectEventGraphicsInfo(object->graphicsId)->size;
            gSprites[object->spriteId].images = &image;
            DestroySprite(&gSprites[object->spriteId]);
        }
        object->active = FALSE;
    }
    sSlots[slot].playerId = 0;
    sSlots[slot].spawned = FALSE;
}

// Drop every remote player. Used on a map change, when the engine has already torn down
// its object events and our bookkeeping would otherwise be stale.
void MmoPlayers_Reset(void)
{
    u8 i;
    for (i = 0; i < NET_MAX_REMOTE_PLAYERS; i++)
    {
        sSlots[i].playerId = 0;
        sSlots[i].spawned = FALSE;
    }
}

static void ReportSelf(void)
{
    struct ObjectEvent *player;
    struct NetProfile profile;
    u8 graphicsId;

    if (gPlayerAvatar.objectEventId >= OBJECT_EVENTS_COUNT)
        return;
    player = &gObjectEvents[gPlayerAvatar.objectEventId];
    if (!player->active)
        return;

    // Report the sprite the server assigned this character, not the local avatar's.
    // The avatar is always Brendan or May; the server picks a distinct NPC sprite per
    // character so other players can tell each other apart.
    graphicsId = Net_GetProfile(&profile) ? profile.graphicsId : player->graphicsId;

    Net_SendSelf(sCurrentMapGroup, sCurrentMapNum,
                 player->currentCoords.x, player->currentCoords.y,
                 player->facingDirection,
                 player->heldMovementActive && !player->heldMovementFinished,
                 graphicsId,
                 player->currentElevation);
}

// Bring one slot in line with what the server says about that player.
static void ApplyRemote(u8 slot, const struct NetRemotePlayer *remote)
{
    struct MmoSlot *state = &sSlots[slot];
    struct ObjectEvent *object;
    s16 dx, dy;
    u8 direction;

    if (!state->spawned)
    {
        // MOVEMENT_TYPE_NONE: the engine must not wander them about; the server decides
        // where they are.
        u8 objectEventId = SpawnSpecialObjectEventParameterized(
            remote->graphicsId, MOVEMENT_TYPE_NONE, LocalIdForSlot(slot),
            remote->x, remote->y, remote->elevation);

        // No free object event slot this frame -- the map's own NPCs have them all.
        // Try again next frame rather than dropping the player.
        if (objectEventId == OBJECT_EVENTS_COUNT)
            return;

        state->spawned = TRUE;
        state->stalledSteps = 0;
        ObjectEventTurn(&gObjectEvents[objectEventId], remote->facing);
        return;
    }

    object = FindSlotObject(slot);
    if (object == NULL)
    {
        // The engine removed it behind our back, e.g. it scrolled out of view. Forget the
        // spawn so the next frame recreates it.
        state->spawned = FALSE;
        return;
    }

    // Let an in-progress step finish before starting another, otherwise the walk
    // animation restarts every frame and the sprite never actually moves.
    if (ObjectEventIsMovementOverridden(object)
     && !ObjectEventClearHeldMovementIfFinished(object))
        return;

    // Measure against where the sprite actually is, not against a copy of where it was
    // last told to go. The engine is what moves it, and a step can be refused -- by a
    // collision, or by the movement being overridden -- so any private idea of its
    // position drifts the moment one does not happen. Reading the object back makes every
    // frame self-correcting instead.
    dx = remote->x - object->currentCoords.x;
    dy = remote->y - object->currentCoords.y;
    direction = StepDirection(dx, dy);

    if (direction != DIR_NONE)
    {
        // One tile: walk it, so observers see the same animation the owner does.
        //
        // ObjectEventSetHeldMovement returns TRUE when it could *not* set the movement.
        // Reading that as success is what made remote players walk forever: the step
        // succeeded, the recorded position was left behind, and every later snapshot
        // measured the same difference again and reissued the same step.
        if (ObjectEventSetHeldMovement(object, GetWalkNormalMovementAction(direction)))
        {
            // Refused, most likely something standing in the way. Give it a few frames to
            // clear, then put them where the server says rather than sliding further out
            // of step with everyone else.
            if (++state->stalledSteps > MMO_MAX_STALLED_STEPS)
            {
                MoveObjectEventToMapCoords(object, remote->x, remote->y);
                state->stalledSteps = 0;
            }
        }
        else
        {
            state->stalledSteps = 0;
        }
    }
    else if (dx != 0 || dy != 0)
    {
        // Anything further is a warp, a dropped update, or a doorway. Snap.
        MoveObjectEventToMapCoords(object, remote->x, remote->y);
        state->stalledSteps = 0;
    }
    else if (remote->facing != DIR_NONE && object->facingDirection != remote->facing)
    {
        ObjectEventTurn(object, remote->facing);
    }
}

// Names of the players currently occupying each slot, kept so an interaction can address
// the player by name without another round trip.
static char sSlotNames[NET_MAX_REMOTE_PLAYERS][NET_NAME_LEN];

bool8 MmoPlayers_ShouldSkipIntro(void)
{
    return Net_GetAuthState() == NET_AUTH_ONLINE;
}

bool8 MmoPlayers_IsRemoteObject(u8 objectEventId)
{
    if (objectEventId >= OBJECT_EVENTS_COUNT)
        return FALSE;
    return gObjectEvents[objectEventId].localId >= MMO_LOCAL_ID_BASE
        && gObjectEvents[objectEventId].localId < MMO_LOCAL_ID_BASE + NET_MAX_REMOTE_PLAYERS;
}

const char *MmoPlayers_GetRemoteName(u8 objectEventId)
{
    u8 slot;

    if (!MmoPlayers_IsRemoteObject(objectEventId))
        return NULL;
    slot = gObjectEvents[objectEventId].localId - MMO_LOCAL_ID_BASE;
    if (sSlots[slot].playerId == 0)
        return NULL;
    return sSlotNames[slot];
}

extern const u8 PokePlanet_EventScript_PlayerInteract[];
extern const u8 PokePlanet_EventScript_PlayerGone[];

// Who the player is currently talking to. The interaction script runs across several
// frames and cannot carry a 32-bit id in a script variable, so it is held here between
// GetInteractionScript and the special that sends the challenge.
static u32 sInteractingWith;

// Called from the interaction script's YES branch (see data/scripts/pokeplanet.inc).
void PokePlanet_SendBattleRequest(void)
{
    if (sInteractingWith != 0)
        Net_RequestBattle(sInteractingWith);
}

extern const u8 PokePlanet_EventScript_BattleInvite[];

// Who challenged us, kept for the script's YES branch for the same reason as above: a
// script variable is 16 bits and a player id is 32.
static u32 sChallengedBy;

// Called from the invitation script's branches (see data/scripts/pokeplanet.inc).
void PokePlanet_AcceptBattle(void)
{
    if (sChallengedBy != 0)
        Net_RespondToBattle(sChallengedBy, TRUE);
    sChallengedBy = 0;
}

void PokePlanet_DeclineBattle(void)
{
    if (sChallengedBy != 0)
        Net_RespondToBattle(sChallengedBy, FALSE);
    sChallengedBy = 0;
}

// Interrupt the player with a challenge, but only while they are actually standing in the
// field doing nothing. Starting a script on top of a running one, mid-warp or mid-battle
// would corrupt the script context, so a challenge that arrives at a bad moment simply
// waits for the next frame that is safe.
static void CheckForBattleInvite(void)
{
    struct NetBattleInvite invite;
    u8 encoded[NET_NAME_LEN + 1];

    if (ArePlayerFieldControlsLocked() || ScriptContext_IsEnabled())
        return;
    if (!Net_PopBattleInvite(&invite))
        return;

    sChallengedBy = invite.from;
    MmoText_FromAscii(encoded, invite.fromName, sizeof(encoded));
    StringCopy(gStringVar1, encoded);

    LockPlayerFieldControls();
    ScriptContext_SetupScript(PokePlanet_EventScript_BattleInvite);
}

extern const u8 PokePlanet_EventScript_ChallengeAccepted[];
extern const u8 PokePlanet_EventScript_ChallengeDeclined[];
extern const u8 PokePlanet_EventScript_ChallengeFailed[];

// Tell the player how the challenge they sent turned out. Same idle-frame rule as above:
// waiting for a safe frame costs a moment, interrupting a running script corrupts it.
static void CheckForBattleOutcome(void)
{
    struct NetBattleInvite answer;
    bool8 accepted;
    char reason[NET_TEXT_LEN];
    u8 encoded[NET_TEXT_LEN];

    if (ArePlayerFieldControlsLocked() || ScriptContext_IsEnabled())
        return;

    if (Net_PopBattleAnswer(&answer, &accepted))
    {
        MmoText_FromAscii(encoded, answer.fromName, NET_NAME_LEN + 1);
        StringCopy(gStringVar1, encoded);
        LockPlayerFieldControls();
        ScriptContext_SetupScript(accepted ? PokePlanet_EventScript_ChallengeAccepted
                                           : PokePlanet_EventScript_ChallengeDeclined);
        return;
    }

    if (Net_PopBattleFailure(reason, sizeof(reason)))
    {
        MmoText_FromAscii(encoded, reason, sizeof(encoded));
        StringCopy(gStringVar1, encoded);
        LockPlayerFieldControls();
        ScriptContext_SetupScript(PokePlanet_EventScript_ChallengeFailed);
    }
}

const u8 *MmoPlayers_GetInteractionScript(u8 objectEventId)
{
    const char *name = MmoPlayers_GetRemoteName(objectEventId);
    u8 encoded[NET_NAME_LEN + 1];

    // They left between the A press and this call.
    if (name == NULL)
    {
        sInteractingWith = 0;
        return PokePlanet_EventScript_PlayerGone;
    }
    sInteractingWith = sSlots[gObjectEvents[objectEventId].localId - MMO_LOCAL_ID_BASE].playerId;

    // The script addresses them by name, which is also how a player tells another trainer
    // apart from one of the map's own NPCs.
    MmoText_FromAscii(encoded, name, sizeof(encoded));
    StringCopy(gStringVar1, encoded);
    return PokePlanet_EventScript_PlayerInteract;
}

void MmoPlayers_Update(void)
{
    struct NetRemotePlayer remotes[NET_MAX_REMOTE_PLAYERS];
    bool8 slotSeen[NET_MAX_REMOTE_PLAYERS];
    u8 count;
    u8 i;
    u8 slot;

    if (!Net_IsLinked() || Net_GetAuthState() != NET_AUTH_ONLINE)
        return;

    // A map change invalidates every object event, so start over.
    if (gSaveBlock1Ptr->location.mapGroup != sCurrentMapGroup
     || gSaveBlock1Ptr->location.mapNum != sCurrentMapNum)
    {
        MmoPlayers_Reset();
        // Every window went with the old map, including chat's.
        MmoChat_Reset();
        sCurrentMapGroup = gSaveBlock1Ptr->location.mapGroup;
        sCurrentMapNum = gSaveBlock1Ptr->location.mapNum;
    }

    ReportSelf();
    CheckForBattleInvite();
    CheckForBattleOutcome();

    count = Net_GetRemotePlayers(remotes);
    for (i = 0; i < NET_MAX_REMOTE_PLAYERS; i++)
        slotSeen[i] = FALSE;

    for (i = 0; i < count; i++)
    {
        const struct NetRemotePlayer *remote = &remotes[i];

        // The server scopes snapshots to our map, but a snapshot can arrive a frame after
        // we walk through a door. Ignore anyone who is not here.
        if (remote->mapGroup != sCurrentMapGroup || remote->mapNum != sCurrentMapNum)
            continue;

        // Reuse this player's existing slot if they already have one.
        for (slot = 0; slot < NET_MAX_REMOTE_PLAYERS; slot++)
        {
            if (sSlots[slot].playerId == remote->playerId)
                break;
        }
        if (slot == NET_MAX_REMOTE_PLAYERS)
        {
            for (slot = 0; slot < NET_MAX_REMOTE_PLAYERS; slot++)
            {
                if (sSlots[slot].playerId == 0)
                    break;
            }
            if (slot == NET_MAX_REMOTE_PLAYERS)
                continue; // Every slot taken; this player waits for one to free up.
            sSlots[slot].playerId = remote->playerId;
            sSlots[slot].spawned = FALSE;
        }

        slotSeen[slot] = TRUE;
        memcpy(sSlotNames[slot], remote->name, NET_NAME_LEN);
        sSlotNames[slot][NET_NAME_LEN - 1] = '\0';
        ApplyRemote(slot, remote);
    }

    // Anyone absent from this snapshot has left the map or disconnected.
    for (slot = 0; slot < NET_MAX_REMOTE_PLAYERS; slot++)
    {
        if (sSlots[slot].playerId != 0 && !slotSeen[slot])
            DespawnSlot(slot);
    }
}
