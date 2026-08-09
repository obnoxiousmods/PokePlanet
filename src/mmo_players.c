// Draws other PokePlanet players in the overworld.
//
// Remote players are ordinary object events, so they inherit the engine's sprite
// handling, elevation, reflections and animation for free. The only thing this file does
// is keep a small table of slots in sync with the server's snapshot: spawn when someone
// appears, drive their movement, and despawn when they leave.
//
// Ticked once per overworld frame from OverworldBasic().

#include <stdarg.h>
#include "global.h"
#include "event_object_movement.h"
#include "field_player_avatar.h"
#include "mmo_players.h"
#include "net_client.h"
#include "constants/event_object_movement.h"

// Local IDs for remote players. Real maps number their object events from 1 and never
// come close to this, so these cannot collide with a map's own NPCs.
#define MMO_LOCAL_ID_BASE 200

// Diagnostics for the multiplayer path, mirrored into pokeplanet.log. Rate limited by
// the caller; never called every frame.
extern void Platform_LogMultiplayer(const char *line);

static void MmoDebug(const char *format, ...)
{
    char line[160];
    va_list args;

    va_start(args, format);
    vsnprintf(line, sizeof(line), format, args);
    va_end(args);
    Platform_LogMultiplayer(line);
}

struct MmoSlot
{
    u32 playerId;      // 0 when the slot is free
    s16 x;             // last position we applied
    s16 y;
    u8 facing;
    bool8 spawned;
};

static struct MmoSlot sSlots[NET_MAX_REMOTE_PLAYERS];
static u8 sCurrentMapGroup = 0xFF;
static u8 sCurrentMapNum = 0xFF;
static u16 sDebugTimer;
static struct NetRemotePlayer sDebugScratch[NET_MAX_REMOTE_PLAYERS];

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

    if (!TryGetObjectEventIdByLocalIdAndMap(LocalIdForSlot(slot), sCurrentMapNum,
                                            sCurrentMapGroup, &objectEventId))
        return NULL;
    return &gObjectEvents[objectEventId];
}

static void DespawnSlot(u8 slot)
{
    if (sSlots[slot].spawned)
        RemoveObjectEventByLocalIdAndMap(LocalIdForSlot(slot), sCurrentMapNum, sCurrentMapGroup);
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

        if (objectEventId == OBJECT_EVENTS_COUNT)
        {
            MmoDebug("spawn FAILED slot=%u gfx=%u at %d,%d", slot,
                     remote->graphicsId, remote->x, remote->y);
            return; // No free object event slot this frame; try again next frame.
        }
        MmoDebug("spawned slot=%u obj=%u gfx=%u at %d,%d", slot, objectEventId,
                 remote->graphicsId, remote->x, remote->y);

        state->spawned = TRUE;
        state->x = remote->x;
        state->y = remote->y;
        state->facing = remote->facing;
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

    dx = remote->x - state->x;
    dy = remote->y - state->y;
    direction = StepDirection(dx, dy);

    if (direction != DIR_NONE)
    {
        // One tile: walk it, so observers see the same animation the owner does.
        if (ObjectEventSetHeldMovement(object, GetWalkNormalMovementAction(direction)))
        {
            state->x = remote->x;
            state->y = remote->y;
            state->facing = direction;
        }
    }
    else if (dx != 0 || dy != 0)
    {
        // Anything further is a warp, a dropped update, or a doorway. Snap.
        MoveObjectEventToMapCoords(object, remote->x, remote->y);
        state->x = remote->x;
        state->y = remote->y;
    }
    else if (remote->facing != state->facing && remote->facing != DIR_NONE)
    {
        ObjectEventTurn(object, remote->facing);
        state->facing = remote->facing;
    }
}

void MmoPlayers_Update(void)
{
    struct NetRemotePlayer remotes[NET_MAX_REMOTE_PLAYERS];
    bool8 slotSeen[NET_MAX_REMOTE_PLAYERS];
    u8 count;
    u8 i;
    u8 slot;

    // Once a second, record what this client believes about the world. Without it the
    // difference between "no snapshot arrived", "snapshot arrived but the map did not
    // match" and "spawn was refused" is invisible.
    if (++sDebugTimer >= 60)
    {
        sDebugTimer = 0;
        MmoDebug("tick linked=%u auth=%u map=%u:%u remotes=%u",
                 Net_IsLinked(), Net_GetAuthState(),
                 gSaveBlock1Ptr->location.mapGroup, gSaveBlock1Ptr->location.mapNum,
                 Net_GetRemotePlayers(sDebugScratch));
    }

    if (!Net_IsLinked() || Net_GetAuthState() != NET_AUTH_ONLINE)
        return;

    // A map change invalidates every object event, so start over.
    if (gSaveBlock1Ptr->location.mapGroup != sCurrentMapGroup
     || gSaveBlock1Ptr->location.mapNum != sCurrentMapNum)
    {
        MmoPlayers_Reset();
        sCurrentMapGroup = gSaveBlock1Ptr->location.mapGroup;
        sCurrentMapNum = gSaveBlock1Ptr->location.mapNum;
    }

    ReportSelf();

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
        ApplyRemote(slot, remote);
    }

    // Anyone absent from this snapshot has left the map or disconnected.
    for (slot = 0; slot < NET_MAX_REMOTE_PLAYERS; slot++)
    {
        if (sSlots[slot].playerId != 0 && !slotSeen[slot])
            DespawnSlot(slot);
    }
}
