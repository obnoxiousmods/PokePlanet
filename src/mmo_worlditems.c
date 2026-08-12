// Dropped items on the ground, drawn as Poke Ball objects and picked up by walking onto them.
//
// The server owns every drop (see server/src/world_items.rs): it reports the drops on the current
// map, hands a pickup to exactly one taker, and tells that taker to add it to their bag. This file
// is only the picture and the "I'm standing on it" nudge -- it never decides who gets what, so a
// bug here cannot duplicate an item, only fail to draw one. Kept wholly separate from mmo_players so
// it cannot disturb how remote trainers are rendered.

#include "global.h"
#include "event_data.h"
#include "event_object_movement.h"
#include "field_player_avatar.h"
#include "item.h"
#include "net_client.h"
#include "mmo_worlditems.h"
#include "constants/event_objects.h"

// Object-event local ids for drops start just past the remote-player range (200 .. 200+8), so the
// two never collide. 32 drop slots fit under the u8 local-id ceiling.
#define MMO_DROP_LOCAL_ID_BASE (200 + NET_MAX_REMOTE_PLAYERS)

// Which server drop id occupies each drawn slot, 0 for an empty slot. Matching by id (not list
// order) keeps a drop pinned to its object as others come and go.
static u32 sSlotDropId[NET_MAX_DROPS];

// The last drop the player stepped onto and asked for, so the request is sent once per step rather
// than every frame. Reset when the player is not standing on any drop.
static u32 sPickupRequestId;

static struct ObjectEvent *FindDropObject(u8 slot)
{
    u8 objectEventId;

    // TryGetObjectEventIdByLocalIdAndMap returns TRUE when the object was NOT found (see the same
    // inverted convention in mmo_players).
    if (TryGetObjectEventIdByLocalIdAndMap(MMO_DROP_LOCAL_ID_BASE + slot,
                                           gSaveBlock1Ptr->location.mapNum,
                                           gSaveBlock1Ptr->location.mapGroup, &objectEventId))
        return NULL;
    return &gObjectEvents[objectEventId];
}

static void DespawnSlot(u8 slot)
{
    struct ObjectEvent *object = sSlotDropId[slot] != 0 ? FindDropObject(slot) : NULL;

    if (object != NULL)
    {
        // Tear the sprite down directly, not via RemoveObjectEventByLocalIdAndMap: that would set a
        // story flag from the uninitialised flagId of a special-spawned template (the mmo_players
        // note explains this in full). Range-check spriteId, which is MAX_SPRITES on a failed spawn.
        if (object->active && object->spriteId < MAX_SPRITES)
        {
            struct SpriteFrameImage image;

            image.size = GetObjectEventGraphicsInfo(object->graphicsId)->size;
            gSprites[object->spriteId].images = &image;
            DestroySprite(&gSprites[object->spriteId]);
        }
        object->active = FALSE;
    }
    sSlotDropId[slot] = 0;
}

void MmoWorldItems_Reset(void)
{
    u8 slot;

    for (slot = 0; slot < NET_MAX_DROPS; slot++)
        sSlotDropId[slot] = 0;
    sPickupRequestId = 0;
}

void MmoWorldItems_Update(void)
{
    struct NetDrop drops[NET_MAX_DROPS];
    u8 count;
    u8 slot;
    u8 i;
    bool8 onDrop;
    struct ObjectEvent *player;
    s16 px, py;
    u16 item, quantity;

    count = Net_GetMapDrops(drops, NET_MAX_DROPS);

    // Despawn slots whose drop the server no longer reports (taken, expired, or off this map).
    for (slot = 0; slot < NET_MAX_DROPS; slot++)
    {
        bool8 stillListed = FALSE;

        if (sSlotDropId[slot] == 0)
            continue;
        for (i = 0; i < count; i++)
        {
            if (drops[i].id == sSlotDropId[slot])
            {
                stillListed = TRUE;
                break;
            }
        }
        if (!stillListed)
            DespawnSlot(slot);
    }

    // Spawn a Poke Ball for each drop not already drawn.
    for (i = 0; i < count; i++)
    {
        bool8 drawn = FALSE;
        u8 free = NET_MAX_DROPS;
        u8 objectEventId;

        for (slot = 0; slot < NET_MAX_DROPS; slot++)
        {
            if (sSlotDropId[slot] == drops[i].id)
            {
                drawn = TRUE;
                break;
            }
            if (sSlotDropId[slot] == 0 && free == NET_MAX_DROPS)
                free = slot;
        }
        if (drawn || free == NET_MAX_DROPS)
            continue;

        objectEventId = SpawnSpecialObjectEventParameterized(
            OBJ_EVENT_GFX_ITEM_BALL, MOVEMENT_TYPE_NONE, MMO_DROP_LOCAL_ID_BASE + free,
            drops[i].x, drops[i].y, 3);
        // No free object-event slot this frame (the map's NPCs hold them all); try next frame.
        if (objectEventId == OBJECT_EVENTS_COUNT)
            continue;
        sSlotDropId[free] = drops[i].id;
    }

    // Pick up the drop under the player, once per time they step onto it. The server decides whether
    // they actually get it (owner window, expiry); a refusal just leaves the drop where it is.
    player = &gObjectEvents[gPlayerAvatar.objectEventId];
    px = player->currentCoords.x;
    py = player->currentCoords.y;
    onDrop = FALSE;
    for (i = 0; i < count; i++)
    {
        if (drops[i].x == px && drops[i].y == py)
        {
            onDrop = TRUE;
            if (drops[i].id != sPickupRequestId)
            {
                sPickupRequestId = drops[i].id;
                Net_PickUpItem(drops[i].id);
            }
            break;
        }
    }
    if (!onDrop)
        sPickupRequestId = 0;

    // Add a pickup the server confirmed. Only the taker it chose is ever told, so this is safe.
    if (Net_PopPickedUp(&item, &quantity))
        AddBagItem(item, quantity);
}
