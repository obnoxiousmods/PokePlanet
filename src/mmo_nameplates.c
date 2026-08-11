// A floating name tag over every other player's head, showing their name and how many Pokemon
// they carry.
//
// The name is already known -- the snapshot carries it and mmo_players keeps it per slot -- so
// this is purely presentation: render the name into a sprite and pin that sprite above the
// player's object-event sprite every frame, the way a field effect (the "!" bubble) follows the
// object it belongs to.
//
// WIDTH is the whole point of this file's shape. A single OBJ sprite that is 64px wide costs
// 64x32 = thirty-two tiles, and eight of those (one per remote player) starved the shared OBJ
// tile pool until the remote players' own bodies stopped rendering -- the multiplayer regression.
// But 32px is too narrow for a name like "obnoxious" plus a count; both clipped. The GBA has no
// 64px-wide sprite smaller than 64x32, so the tag is instead built from TWO 32x16 sprites side by
// side: 64x16, sixteen tiles per tag, a hundred and twenty-eight for eight players -- half of what
// broke it. The name gets the full 64px top row and the count the bottom row, so neither clips.
//
// Every allocation here is checked and failure is silent. A name tag is the last thing that should
// be allowed to fail a map load or corrupt the player's own sprite: if a tile block or a sprite
// slot is not free, the tag simply does not appear that frame. It is a label, not a mechanic.

#include "global.h"
#include "main.h"
#include "malloc.h"
#include "menu.h"
#include "palette.h"
#include "sprite.h"
#include "string_util.h"
#include "text.h"
#include "window.h"
#include "mmo_nameplates.h"
#include "mmo_text.h"
#include "net_client.h"
#include "event_object_movement.h"
#include "constants/characters.h"
#include "constants/rgb.h"

// 'PN' -- a tag range the overworld does not use. Two gfx tags per slot (a left half and a right
// half) so each half owns its own tiles; one shared palette, since every tag is drawn in the same
// two colours. Slots run 0..NET_MAX_REMOTE_PLAYERS-1, halves 0..1, so the tags span this base
// through base + 2*NET_MAX_REMOTE_PLAYERS - 1 and must stay clear of any other overworld gfx tag.
#define NAMEPLATE_GFX_TAG_BASE 0x504E
#define NAMEPLATE_PAL_TAG      0x504E

// Two 32x16 halves make one 64x16 tag. Each half is four tiles across and two down, eight tiles;
// the pair is sixteen. See the file header for why this is two sprites and not one wide one.
#define NP_HALVES         2
#define NP_HALF_W_TILES   4
#define NP_H_TILES        2
#define NP_HALF_TILES     (NP_HALF_W_TILES * NP_H_TILES)
#define NP_FULL_W_TILES   (NP_HALF_W_TILES * NP_HALVES)
#define NP_WIDTH_PX       (NP_FULL_W_TILES * 8)
#define NP_HALF_WIDTH_PX  (NP_HALF_W_TILES * 8)

// How far above the object-event sprite's centre the tag floats.
#define NP_Y_OFFSET 20

// The old single 32x16 tag sat at the body's x, centred on body_x + 16. Keeping that same centre
// for the wider tag means no perceived horizontal shift: the left half sits 16px left of the body
// x and the right half 16px to its right, so the pair spans body_x - 16 .. body_x + 48 as before,
// only wider. Stored per-sprite so one callback can drive both halves.
#define NP_X_LEFT  (-16)
#define NP_X_RIGHT (NP_X_LEFT + NP_HALF_WIDTH_PX)

// Two sprites per remote-player slot, matched to mmo_players' own slot indexing.
static u8 sNameplateSpriteIds[NET_MAX_REMOTE_PLAYERS][NP_HALVES];
static u8 sRenderedFor[NET_MAX_REMOTE_PLAYERS][NET_NAME_LEN];
static u8 sRenderedCount[NET_MAX_REMOTE_PLAYERS];
// The arrays zero-initialise, but sprite id 0 is a real sprite, so "no tag here" has to be set
// explicitly to MAX_SPRITES before the first Set -- this guards that.
static bool8 sInitialized;

// data[0] holds the object-event id the tag follows, so the callback can track the sprite as it
// walks and tear the tag down the moment the object is gone. data[1] holds this half's x offset.
#define spObjectEventId data[0]
#define spXOffset       data[1]

static const struct OamData sNameplateOam =
{
    .y = 0,
    .affineMode = ST_OAM_AFFINE_OFF,
    .objMode = ST_OAM_OBJ_NORMAL,
    .shape = SPRITE_SHAPE(32x16),
    .size = SPRITE_SIZE(32x16),
    .priority = 1, // above the map, with the object-event sprites
};

// bg transparent, the name in white with a dark outline, so it reads over any tileset.
static const u16 sNameplatePalette[16] =
{
    RGB_BLACK, RGB_WHITE, RGB(6, 6, 6), RGB_BLACK,
    RGB_BLACK, RGB_BLACK, RGB_BLACK, RGB_BLACK,
    RGB_BLACK, RGB_BLACK, RGB_BLACK, RGB_BLACK,
    RGB_BLACK, RGB_BLACK, RGB_BLACK, RGB_BLACK,
};
static const struct SpritePalette sNameplateSpritePalette =
{
    .data = sNameplatePalette,
    .tag = NAMEPLATE_PAL_TAG,
};

static void SpriteCB_Nameplate(struct Sprite *sprite)
{
    struct ObjectEvent *object;
    u8 objectEventId = sprite->spObjectEventId;

    // The object it follows may have been torn down (walked through a door, scrolled off, the
    // player left). Nothing else will free this sprite, so it frees itself.
    if (objectEventId >= OBJECT_EVENTS_COUNT
     || !gObjectEvents[objectEventId].active
     || gObjectEvents[objectEventId].spriteId >= MAX_SPRITES)
    {
        DestroySprite(sprite);
        return;
    }

    object = &gObjectEvents[objectEventId];
    {
        const struct Sprite *body = &gSprites[object->spriteId];
        // Follow the object-event sprite in camera space. An object-event sprite has
        // coordOffsetEnabled set, so the OAM builder adds the camera scroll (gSpriteCoordOffset)
        // to its x/y when it draws; without the same flag here, copying body->x put the tag at the
        // *unscrolled* position, which drifted off to a screen corner the further the camera had
        // moved from the origin. Match the flag and the copy tracks exactly, walk animation and all.
        sprite->coordOffsetEnabled = TRUE;
        sprite->x = body->x + sprite->spXOffset;
        sprite->y = body->y - NP_Y_OFFSET;
        sprite->x2 = body->x2;
        sprite->y2 = body->y2;
        sprite->invisible = body->invisible;
        sprite->subpriority = body->subpriority - 1;
    }
}

void MmoNameplates_Init(void)
{
    u8 slot;
    u8 half;

    for (slot = 0; slot < NET_MAX_REMOTE_PLAYERS; slot++)
    {
        for (half = 0; half < NP_HALVES; half++)
            sNameplateSpriteIds[slot][half] = MAX_SPRITES;
        sRenderedFor[slot][0] = '\0';
        sRenderedCount[slot] = 0xFF; // never a real party size, so the first Set always renders
    }
    sInitialized = TRUE;
}

// Forget every tag without touching sprites -- for a map change, where the sprites are already
// gone and clearing them here would reach into whatever took their place.
void MmoNameplates_Reset(void)
{
    MmoNameplates_Init();
}

static u16 NameplateGfxTag(u8 slot, u8 half)
{
    return NAMEPLATE_GFX_TAG_BASE + slot * NP_HALVES + half;
}

static void ClearNameplate(u8 slot)
{
    u8 half;

    for (half = 0; half < NP_HALVES; half++)
    {
        if (sNameplateSpriteIds[slot][half] < MAX_SPRITES)
        {
            DestroySprite(&gSprites[sNameplateSpriteIds[slot][half]]);
            sNameplateSpriteIds[slot][half] = MAX_SPRITES;
        }
    }
    sRenderedFor[slot][0] = '\0';
}

// Render "name" (top row, full 64px) and the party count (bottom row, centred) into the pair of
// sprites for `slot`. Uses one throwaway 64x16 window; on any failure the tag is left as it was.
static void DrawName(u8 slot, const char *name, u8 partyCount)
{
    struct WindowTemplate template;
    u8 encoded[NET_NAME_LEN + 1];
    u8 countStr[4]; // up to three digits and a terminator
    u8 colours[3] = { 0, 1, 2 }; // bg, text, shadow -- indices into sNameplatePalette
    const u8 *tileData;
    u8 windowId;
    u16 leftStart;
    u16 rightStart;
    s32 countWidth;
    u8 countX;

    leftStart = GetSpriteTileStartByTag(NameplateGfxTag(slot, 0));
    rightStart = GetSpriteTileStartByTag(NameplateGfxTag(slot, 1));
    if (leftStart == TAG_NONE || rightStart == TAG_NONE)
        return;

    template.bg = 0;
    template.tilemapLeft = 0;
    template.tilemapTop = 0;
    template.width = NP_FULL_W_TILES;
    template.height = NP_H_TILES;
    template.paletteNum = 15;
    template.baseBlock = 0; // an off-screen scratch window; only its tile data is used
    windowId = AddWindow(&template);
    if (windowId == WINDOW_NONE)
        return;

    FillWindowPixelBuffer(windowId, PIXEL_FILL(0));

    // Name across the full 64px top row, left-aligned: a long name clips from the right rather than
    // both ends, because its start is what identifies the player. The count sits centred on the
    // bottom row so a single digit does not float off in a corner. The narrowest font keeps the two
    // rows from touching.
    MmoText_FromAscii(encoded, name, sizeof(encoded));
    AddTextPrinterParameterized3(windowId, FONT_SMALL, 0, 0, colours, TEXT_SKIP_DRAW, encoded);
    ConvertIntToDecimalStringN(countStr, partyCount, STR_CONV_MODE_LEFT_ALIGN, 3);
    countWidth = GetStringWidth(FONT_SMALL, countStr, 0);
    countX = (countWidth < NP_WIDTH_PX) ? (u8)((NP_WIDTH_PX - countWidth) / 2) : 0;
    AddTextPrinterParameterized3(windowId, FONT_SMALL, countX, 8, colours, TEXT_SKIP_DRAW, countStr);

    // The window's tile data is a linear run of 8x8 tiles in reading order: the top row is tiles
    // 0..7 and the bottom row 8..15. Each 32x16 sprite wants its own four columns as [top four,
    // bottom four]. So the left half takes tiles {0,1,2,3} then {8,9,10,11}; the right half {4,5,6,7}
    // then {12,13,14,15} -- two contiguous copies per sprite.
    tileData = (const u8 *)GetWindowAttribute(windowId, WINDOW_TILE_DATA);
    {
        u8 *leftDest = (u8 *)OBJ_VRAM0 + leftStart * TILE_SIZE_4BPP;
        u8 *rightDest = (u8 *)OBJ_VRAM0 + rightStart * TILE_SIZE_4BPP;
        const u32 rowBytes = NP_HALF_W_TILES * TILE_SIZE_4BPP;
        const u8 *topRow = tileData;
        const u8 *botRow = tileData + NP_FULL_W_TILES * TILE_SIZE_4BPP;

        CpuCopy32(topRow, leftDest, rowBytes);
        CpuCopy32(botRow, leftDest + rowBytes, rowBytes);
        CpuCopy32(topRow + rowBytes, rightDest, rowBytes);
        CpuCopy32(botRow + rowBytes, rightDest + rowBytes, rowBytes);
    }

    RemoveWindow(windowId);
}

// Create one half-sprite for `slot`, at x offset `xOffset`, following `objectEventId`. Returns the
// sprite id, or MAX_SPRITES on any failure (no free tile block or sprite slot). Frees its own tile
// block on a create failure so a half-built pair leaves nothing reserved.
static u8 CreateHalf(u8 slot, u8 half, u8 objectEventId, s16 xOffset)
{
    static const u8 sBlankTiles[NP_HALF_TILES * TILE_SIZE_4BPP];
    struct SpriteSheet sheet;
    struct SpriteTemplate template;
    u16 tag = NameplateGfxTag(slot, half);
    u8 spriteId;

    sheet.data = sBlankTiles;
    sheet.size = sizeof(sBlankTiles);
    sheet.tag = tag;
    if (GetSpriteTileStartByTag(tag) == TAG_NONE)
        LoadSpriteSheet(&sheet);
    // Checked after loading rather than on the return value: a tile start of 0 is a valid block but
    // also LoadSpriteSheet's failure return, so the tag itself is the honest signal.
    if (GetSpriteTileStartByTag(tag) == TAG_NONE)
        return MAX_SPRITES; // no free OBJ tile block; skip the tag, never the player

    template.tileTag = tag;
    template.paletteTag = NAMEPLATE_PAL_TAG;
    template.oam = &sNameplateOam;
    template.anims = gDummySpriteAnimTable;
    template.images = NULL;
    template.affineAnims = gDummySpriteAffineAnimTable;
    template.callback = SpriteCB_Nameplate;

    spriteId = CreateSprite(&template, 0, 0, 0);
    if (spriteId >= MAX_SPRITES)
    {
        FreeSpriteTilesByTag(tag);
        return MAX_SPRITES;
    }
    gSprites[spriteId].spObjectEventId = objectEventId;
    gSprites[spriteId].spXOffset = xOffset;
    return spriteId;
}

// Ensure a tag exists for `slot`, following `objectEventId`, showing `name` and `partyCount`.
// Called once a frame per live remote player from mmo_players.
void MmoNameplates_Set(u8 slot, u8 objectEventId, const char *name, u8 partyCount)
{
    u8 half;

    if (!sInitialized)
        MmoNameplates_Init();
    if (slot >= NET_MAX_REMOTE_PLAYERS || name == NULL)
        return;

    if (sNameplateSpriteIds[slot][0] >= MAX_SPRITES || sNameplateSpriteIds[slot][1] >= MAX_SPRITES)
    {
        // Either half missing means the pair is not up. Rebuild both so their tiles stay a matched
        // set; the palette loads once and is shared. If either half cannot be created, tear the
        // pair back down and skip the tag this frame -- a lone half is never shown.
        LoadSpritePalette(&sNameplateSpritePalette);
        ClearNameplate(slot);
        sNameplateSpriteIds[slot][0] = CreateHalf(slot, 0, objectEventId, NP_X_LEFT);
        sNameplateSpriteIds[slot][1] = CreateHalf(slot, 1, objectEventId, NP_X_RIGHT);
        if (sNameplateSpriteIds[slot][0] >= MAX_SPRITES || sNameplateSpriteIds[slot][1] >= MAX_SPRITES)
        {
            ClearNameplate(slot);
            return;
        }
        sRenderedFor[slot][0] = '\0';
        sRenderedCount[slot] = 0xFF;
    }

    for (half = 0; half < NP_HALVES; half++)
        gSprites[sNameplateSpriteIds[slot][half]].spObjectEventId = objectEventId;

    // Re-render only when the name or the count actually changed -- most frames neither has.
    if (sRenderedCount[slot] != partyCount || StringCompare(sRenderedFor[slot], name) != 0)
    {
        DrawName(slot, name, partyCount);
        StringCopyN(sRenderedFor[slot], name, NET_NAME_LEN);
        sRenderedCount[slot] = partyCount;
    }
}

// Drop the tag for a slot whose player left. Safe to call for a slot that has none.
void MmoNameplates_Clear(u8 slot)
{
    if (!sInitialized)
    {
        // Nothing has been created yet, so there is nothing to clear -- and the sprite ids are
        // still zero, which would otherwise be mistaken for "sprite 0 is this tag".
        MmoNameplates_Init();
        return;
    }
    if (slot < NET_MAX_REMOTE_PLAYERS)
        ClearNameplate(slot);
}
