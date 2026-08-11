// A small name tag over every other player's head.
//
// The name is already known -- the snapshot carries it and mmo_players keeps it per slot -- so
// this is purely presentation: render the name into a sprite and pin that sprite above the
// player's object-event sprite every frame, the way a field effect (the "!" bubble) follows the
// object it belongs to.
//
// Every allocation here is checked and failure is silent. Text sprites want their own OBJ VRAM
// tiles, and a name tag is the last thing that should be allowed to fail a map load or corrupt
// the player's own sprite: if a tile block or a sprite slot is not free, the tag simply does not
// appear that frame. It is a label, not a mechanic.

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

// 'PN' — a tag range the overworld does not use. One gfx tag per slot so each name owns its own
// tiles; one shared palette, since every tag is drawn in the same two colours.
#define NAMEPLATE_GFX_TAG_BASE 0x504E
#define NAMEPLATE_PAL_TAG      0x504E

// 32x16 -- four tiles across, two down, eight in all. Deliberately small: a name tag draws from
// the same OBJ tile pool the remote-player sprites do, and a 64x32 tag (thirty-two tiles) times
// eight players exhausted it, so the players themselves stopped rendering. Eight tiles times eight
// players is sixty-four, a quarter of what broke it. The name goes on the top row, the party count
// on the bottom.
#define NP_W_TILES 4
#define NP_H_TILES 2
#define NP_TILE_COUNT (NP_W_TILES * NP_H_TILES)
#define NP_WIDTH_PX  (NP_W_TILES * 8)

// How far above the object-event sprite's centre the tag floats.
#define NP_Y_OFFSET 20

// One sprite per remote-player slot, matched to mmo_players' own slot indexing.
static u8 sNameplateSpriteIds[NET_MAX_REMOTE_PLAYERS];
static u8 sRenderedFor[NET_MAX_REMOTE_PLAYERS][NET_NAME_LEN];
static u8 sRenderedCount[NET_MAX_REMOTE_PLAYERS];
// The arrays zero-initialise, but sprite id 0 is a real sprite, so "no tag here" has to be set
// explicitly to MAX_SPRITES before the first Set -- this guards that.
static bool8 sInitialized;

// data[0] holds the object-event id the tag follows, so the callback can track the sprite as it
// walks and tear the tag down the moment the object is gone.
#define spObjectEventId data[0]

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
        sprite->x = body->x;
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

    for (slot = 0; slot < NET_MAX_REMOTE_PLAYERS; slot++)
    {
        sNameplateSpriteIds[slot] = MAX_SPRITES;
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

static void ClearNameplate(u8 slot)
{
    if (sNameplateSpriteIds[slot] < MAX_SPRITES)
    {
        DestroySprite(&gSprites[sNameplateSpriteIds[slot]]);
        sNameplateSpriteIds[slot] = MAX_SPRITES;
    }
    sRenderedFor[slot][0] = '\0';
}

// Render "name  N" into the sprite's tiles, N being the party count. Uses one throwaway window;
// on any failure the tag is simply left as it was.
static void DrawName(u8 slot, const char *name, u8 partyCount)
{
    struct WindowTemplate template;
    u8 encoded[NET_NAME_LEN + 1];
    u8 countStr[4]; // up to three digits and a terminator
    u8 colours[3] = { 0, 1, 2 }; // bg, text, shadow -- indices into sNameplatePalette
    const u8 *tileData;
    u8 windowId;
    u16 tileStart;

    if (sNameplateSpriteIds[slot] >= MAX_SPRITES)
        return;
    tileStart = GetSpriteTileStartByTag(NAMEPLATE_GFX_TAG_BASE + slot);
    if (tileStart == TAG_NONE)
        return;

    template.bg = 0;
    template.tilemapLeft = 0;
    template.tilemapTop = 0;
    template.width = NP_W_TILES;
    template.height = NP_H_TILES;
    template.paletteNum = 15;
    template.baseBlock = 0; // an off-screen scratch window; only its tile data is used
    windowId = AddWindow(&template);
    if (windowId == WINDOW_NONE)
        return;

    FillWindowPixelBuffer(windowId, PIXEL_FILL(0));

    // Two rows: the name on top, the party count under it. The tag is only 32px wide, so a long
    // name is left-aligned and clipped from the right rather than centred and clipped both ends --
    // its start is what identifies the player. The narrowest font keeps the two rows from touching.
    MmoText_FromAscii(encoded, name, sizeof(encoded));
    AddTextPrinterParameterized3(windowId, FONT_SMALL, 0, 0, colours, TEXT_SKIP_DRAW, encoded);
    ConvertIntToDecimalStringN(countStr, partyCount, STR_CONV_MODE_LEFT_ALIGN, 3);
    AddTextPrinterParameterized3(windowId, FONT_SMALL, 0, 8, colours, TEXT_SKIP_DRAW, countStr);

    tileData = (const u8 *)GetWindowAttribute(windowId, WINDOW_TILE_DATA);
    CpuCopy32(tileData, (void *)OBJ_VRAM0 + tileStart * TILE_SIZE_4BPP, NP_TILE_COUNT * TILE_SIZE_4BPP);

    RemoveWindow(windowId);
}

// Ensure a tag exists for `slot`, following `objectEventId`, showing `name`. Called once a frame
// per live remote player from mmo_players.
void MmoNameplates_Set(u8 slot, u8 objectEventId, const char *name, u8 partyCount)
{
    struct Sprite *sprite;

    if (!sInitialized)
        MmoNameplates_Init();
    if (slot >= NET_MAX_REMOTE_PLAYERS || name == NULL)
        return;

    if (sNameplateSpriteIds[slot] >= MAX_SPRITES)
    {
        // A block of zero tiles to reserve VRAM with; DrawName fills them once the sprite is up.
        // Static rather than on the stack: this is a kilobyte, and it never changes.
        static const u8 sBlankTiles[NP_TILE_COUNT * TILE_SIZE_4BPP];
        struct SpriteSheet sheet;
        struct SpriteTemplate template;
        u8 spriteId;

        sheet.data = sBlankTiles;
        sheet.size = sizeof(sBlankTiles);
        sheet.tag = NAMEPLATE_GFX_TAG_BASE + slot;
        if (GetSpriteTileStartByTag(sheet.tag) == TAG_NONE)
            LoadSpriteSheet(&sheet);
        // Checked after loading rather than on the return value: a tile start of 0 is a valid
        // block but also LoadSpriteSheet's failure return, so the tag itself is the honest signal.
        if (GetSpriteTileStartByTag(sheet.tag) == TAG_NONE)
            return; // no free OBJ tile block; skip the tag, never the player
        LoadSpritePalette(&sNameplateSpritePalette);

        template.tileTag = NAMEPLATE_GFX_TAG_BASE + slot;
        template.paletteTag = NAMEPLATE_PAL_TAG;
        template.oam = &sNameplateOam;
        template.anims = gDummySpriteAnimTable;
        template.images = NULL;
        template.affineAnims = gDummySpriteAffineAnimTable;
        template.callback = SpriteCB_Nameplate;

        spriteId = CreateSprite(&template, 0, 0, 0);
        if (spriteId >= MAX_SPRITES)
        {
            FreeSpriteTilesByTag(NAMEPLATE_GFX_TAG_BASE + slot);
            return;
        }
        sNameplateSpriteIds[slot] = spriteId;
        sRenderedFor[slot][0] = '\0';
    }

    sprite = &gSprites[sNameplateSpriteIds[slot]];
    sprite->spObjectEventId = objectEventId;

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
