// Saving, without the player ever asking for it.
//
// In a single-player Pokemon game the save is something you choose to do, and forgetting
// costs you the afternoon. That bargain makes no sense here: the character belongs to the
// server, other people can see it, and there is no reason a player should ever think about
// it. So the game saves itself, and the result goes straight to the server.
//
// What counts as a change: flags, variables, the bag, money, and any change to the party.
// The first four are funnels the script engine reaches through; the script engine reaches them through FlagSet, VarSet,
// AddBagItem and RemoveBagItem, which is where roughly 2,700 script-driven mutations across
// the game funnel through. Walking does not go through any of them, which is fine -- the
// server already receives the player's position ten times a second and stores it, so a step
// is durable without writing 128KB for it.
//
// Two rules keep this from being ruinous:
//
//   - It only runs on a frame where the field is idle. Saving copies the party out of
//     gPlayerParty and writes fourteen sectors; doing that underneath a running script is
//     asking for a half-written state.
//   - It waits out a short quiet period first. A single script can set dozens of flags in a
//     few frames, and each one is not worth a save; what matters is that the save happens
//     shortly after the player does something, not instantly.

#include "global.h"
#include "field_player_avatar.h"
#include "mmo_autosave.h"
#include "net_client.h"
#include "platform.h"
#include "save.h"
#include "script.h"
#include "pokemon.h"
#include "pokemon_storage_system.h"

// Frames of quiet before a change is written out. Long enough that a script setting a run
// of flags produces one save rather than a dozen; short enough that a player who changes
// something and immediately closes the game keeps it.
#define AUTOSAVE_QUIET_FRAMES 90

static bool8 sDirty;
static u16 sQuietFrames;

// Something worth keeping happened. Called from the mutation funnels, so it must stay
// trivial: these run constantly, including from script bytecode.
void MmoAutosave_NoteChange(void)
{
    // Deliberately does not restart the timer. Resetting on every change sounds like the
    // right debounce and is not: changes arrive in a steady trickle while the player is
    // doing anything at all, so the quiet moment never comes and the save never happens.
    // The timer measures time since the *first* unsaved change, which bounds how long
    // progress can sit unwritten no matter how busy the game is.
    sDirty = TRUE;
}

// Save now, whatever the timer says. For moments where waiting is wrong: leaving a map,
// where the next thing that happens might be a door and a fresh set of object events.
void MmoAutosave_Flush(void)
{
    if (sDirty)
        sQuietFrames = AUTOSAVE_QUIET_FRAMES;
}

// Ticked once per overworld frame.

// Report the party when it changes.
//
// Unlike money and items there is no single chokepoint to hook: a Pokemon changes from levelling,
// evolving, learning a move, taking damage, holding an item, being caught, being healed, being
// swapped with the PC. Hooking each would mean finding all of them and staying right as more are
// added, which is exactly the kind of coverage that quietly rots.
//
// So this compares the bytes instead. A summary cannot be out of date in a way nobody notices,
// because it is derived from the same memory the game is playing from.
static void ReportPartyIfChanged(void)
{
    static u32 sLast;
    static bool8 sHaveSent;
    const u8 *bytes = (const u8 *)gPlayerParty;
    u32 size = sizeof(gPlayerParty);
    u32 sum = 2166136261u;
    u32 i;

    if (size != 600)
        return;

    // FNV-1a. Cheap enough to run every frame and good enough that a real change slipping
    // through unnoticed is not something worth planning around.
    for (i = 0; i < size; i++)
    {
        sum ^= bytes[i];
        sum *= 16777619u;
    }
    sum ^= gPlayerPartyCount;

    if (sHaveSent && sum == sLast)
        return;

    // A supervising server gets the same news, over its own channel. Sent before the network
    // report and regardless of whether that succeeds: the two are answering different questions,
    // and a full network queue is no reason to stop telling the supervisor what happened.
    {
        u8 report[4 + 600];

        report[0] = (u8)(gSaveBlock1Ptr->money & 0xFF);
        report[1] = (u8)((gSaveBlock1Ptr->money >> 8) & 0xFF);
        report[2] = (u8)((gSaveBlock1Ptr->money >> 16) & 0xFF);
        report[3] = (u8)((gSaveBlock1Ptr->money >> 24) & 0xFF);
        memcpy(report + 4, gPlayerParty, size);
        Platform_ReportState(report, 4 + size);
    }

    // Only remember this party as reported once it is actually on the queue.
    //
    // The old order set the hash first and sent afterwards, ignoring the result. Any failed
    // send was then invisible and permanent: the next tick computed the same hash, matched it,
    // and returned -- so the change was never retried and never reached the server. Committing
    // after success means a full queue costs one frame instead of the progress.
    if (!Net_SendParty(gPlayerPartyCount, gPlayerParty, size))
        return;

    sLast = sum;
    sHaveSent = TRUE;

    // A changed party is a change worth saving.
    //
    // Without this, levelling up does not autosave at all. The dirty flag is raised by
    // FlagSet, VarSet, the bag and money, and a Pokemon gaining a level goes through none of
    // them -- so the comment at the top of this file claiming those cover "essentially all
    // progression" was wrong about the one kind of progress the game is mostly made of.
    //
    // Raised here rather than by hunting down every function that can alter a Pokemon,
    // because that list is long and gets longer: levelling, evolving, learning a move,
    // gaining EVs, taking damage, being healed, holding an item, being renamed. Comparing the
    // bytes catches all of them, including the ones nobody has written yet.
    MmoAutosave_NoteChange();
}


// The regions of SaveBlock1 the server accepts directly. Must match REPORTABLE in
// server/src/save_parse.rs -- the server refuses anything not on its own list exactly, so a
// disagreement here shows up as a region that is simply never stored rather than as a crash.
//
// Money, the bag and the party are absent deliberately: they have their own messages, which
// carry checks a raw region write would walk straight past.
// Must match REPORTABLE on the server exactly, offset for offset: the server accepts a region
// only at a length and offset on its own copy of this list. The front chunk starts at 0x34, not
// 0, so it excludes the player's position and every WarpData at the top of SaveBlock1 -- those
// are the server's to set from the pose path, and reporting them here would be a way to teleport.
static const struct { u32 offset; u32 size; } sReportable[] = {
    { 0x34, 0x200 },
    { 0x848, 0x400 },
    { 0xC48, 0x400 },
    { 0x1048, 0x400 },
    { 0x1448, 0x400 },
    { 0x1848, 0x400 },
    { 0x1C48, 0x400 },
    { 0x2048, 0x400 },
    { 0x2448, 0x400 },
    { 0x2848, 0x400 },
    { 0x2C48, 0x400 },
    { 0x3048, 0x400 },
    { 0x3448, 0x400 },
    { 0x3848, 0x400 },
    { 0x3C48, 0x1B8 },
};

// One region reported per tick at most.
//
// Berry trees alone are a kilobyte, and sending every changed region in the same frame would
// put several kilobytes through the pipe at once for what is usually a single flag flipping.
// Round-robin instead: each region is checked in turn, so a burst of changes costs a few
// frames rather than one large stall, and nothing is ever skipped.
static void ReportRegionsIfChanged(void)
{
    static u32 sLast[ARRAY_COUNT(sReportable)];
    static bool8 sHaveSent[ARRAY_COUNT(sReportable)];
    static u32 sNext;

    const u8 *base = (const u8 *)gSaveBlock1Ptr;
    u32 which = sNext;
    u32 sum = 2166136261u;
    u32 i;

    // The chunks tile the four sectors SaveBlock1 occupies, which is more than the struct
    // itself: sizeof(struct SaveBlock1) is smaller than 4 * 3968, and the difference is
    // padding the game never writes. Reading it would be reading past the end of the object,
    // so the copy stops at the struct and the rest of the chunk goes out as zeroes.
    //
    // Zeroes are safe there precisely because it is padding -- no field lives in it, and the
    // server recomputes the sector checksum over whatever it ends up holding. Sending the
    // chunk at its declared length keeps the allowlist an exact match, which is what stops it
    // becoming a write at an offset of the caller's choosing.
    u8 chunk[0x400];
    u32 offset = sReportable[which].offset;
    u32 size = sReportable[which].size;
    u32 readable = 0;

    sNext = (sNext + 1) % ARRAY_COUNT(sReportable);

    if (base == NULL)
        return;

    if (offset < sizeof(struct SaveBlock1))
    {
        readable = sizeof(struct SaveBlock1) - offset;
        if (readable > size)
            readable = size;
    }

    for (i = 0; i < readable; i++)
        chunk[i] = base[offset + i];
    for (; i < size; i++)
        chunk[i] = 0;

    for (i = 0; i < size; i++)
    {
        sum ^= chunk[i];
        sum *= 16777619u;
    }

    if (sHaveSent[which] && sum == sLast[which])
        return;

    // Committed only on success, for the same reason as the party above.
    if (!Net_SendRegion(offset, chunk, size))
        return;

    sLast[which] = sum;
    sHaveSent[which] = TRUE;
}


// Whole blocks that are not SaveBlock1: SaveBlock2, and the PC boxes.
//
// Ids match reportable_block() on the server. SaveBlock1 is deliberately not here -- it holds
// money, the bag and the party, which report through their own messages so they meet caps and
// rate ceilings a wholesale write would skip.
#define REPORT_BLOCK_SAVEBLOCK2 0
#define REPORT_BLOCK_STORAGE    1
// Hall of Fame, Trainer Hill and the recorded battle: sectors 28 to 31, which sit outside the
// two save slots. Reported as whole sectors straight out of the flash mirror, footers included,
// because they are not a struct anybody here models -- and this is the last thing the save
// image carried that nothing else did.
#define REPORT_BLOCK_TAIL       2
#define TAIL_SECTOR_FIRST       28
#define TAIL_SECTOR_COUNT       4
#define SAVE_SECTOR_BYTES       4096

extern unsigned char FLASH_BASE[131072];

#define BLOCK_CHUNK_BYTES 0x400

// A chunk per tick, and only one block in flight.
//
// The boxes are around thirty-five kilobytes. Sending that in one frame would put the whole
// lot through the pipe at once for what is often a single Pokemon being deposited, so it goes
// a kilobyte at a time -- about thirty-five frames, half a second, and no visible hitch.
//
// Only the *size the struct actually is* is ever read. The sectors that carry these blocks are
// larger than the structs themselves, and reading the difference would be reading past the end
// of the object; the server keeps whatever was already in that tail.
static void ReportBlocksIfChanged(void)
{
    static u32 sLast[3];
    static bool8 sHaveSent[3];
    static u32 sSending;      // index + 1, or 0 for idle
    static u32 sOffset;

    const void *base[3];
    u32 size[3];
    u32 i;

    base[REPORT_BLOCK_SAVEBLOCK2] = gSaveBlock2Ptr;
    size[REPORT_BLOCK_SAVEBLOCK2] = sizeof(struct SaveBlock2);
    base[REPORT_BLOCK_STORAGE] = gPokemonStoragePtr;
    size[REPORT_BLOCK_STORAGE] = sizeof(struct PokemonStorage);
    base[REPORT_BLOCK_TAIL] = FLASH_BASE + TAIL_SECTOR_FIRST * SAVE_SECTOR_BYTES;
    size[REPORT_BLOCK_TAIL] = TAIL_SECTOR_COUNT * SAVE_SECTOR_BYTES;

    if (sSending != 0)
    {
        u32 which = sSending - 1;
        u32 left = size[which] - sOffset;
        u32 take = left < BLOCK_CHUNK_BYTES ? left : BLOCK_CHUNK_BYTES;

        if (base[which] == NULL)
        {
            // Abandoned before finishing: forget the hash so the whole block is sent again,
            // rather than being remembered as reported when only part of it went.
            sHaveSent[which] = FALSE;
            sSending = 0;
            return;
        }

        // Advance only when the chunk was queued. Advancing regardless would leave a hole in
        // the middle of the block: the server reassembles by offset and refuses anything that
        // does not arrive contiguously, so a single dropped chunk would discard the whole
        // transfer -- and the block would still be marked as reported, so it would never be
        // retried. A failed chunk simply waits for the next tick.
        if (!Net_SendBlockChunk((u8)which, sOffset, size[which],
                                (const u8 *)base[which] + sOffset, take))
            return;

        sOffset += take;
        if (sOffset >= size[which])
            sSending = 0;
        return;
    }

    for (i = 0; i < 3; i++)
    {
        u32 sum = 2166136261u;
        const u8 *bytes = (const u8 *)base[i];
        u32 j;

        if (bytes == NULL)
            continue;

        for (j = 0; j < size[i]; j++)
        {
            // Play time lives at the front of SaveBlock2 and its vblank counter ticks every
            // frame, so hashing it made SaveBlock2 look changed on every single frame -- the
            // block was re-sent about twelve times a second for nothing but a clock. Skip it in
            // the change check, so SaveBlock2 is reported when something worth saving moves
            // (options, the Pokedex) and play time simply rides along whenever that happens.
            if (i == REPORT_BLOCK_SAVEBLOCK2
                && j >= offsetof(struct SaveBlock2, playTimeHours)
                && j < offsetof(struct SaveBlock2, optionsButtonMode))
                continue;

            sum ^= bytes[j];
            sum *= 16777619u;
        }

        if (sHaveSent[i] && sum == sLast[i])
            continue;

        sLast[i] = sum;
        sHaveSent[i] = TRUE;
        sSending = i + 1;
        sOffset = 0;
        // A box change is worth saving, the same as a party change.
        MmoAutosave_NoteChange();
        return;
    }
}

// Send whatever has changed, without saving anything.
//
// Separate from MmoAutosave_Update because the two have different safety requirements. Writing a
// save copies the party out of gPlayerParty and rewrites fourteen sectors, which must not happen
// underneath a running script or a battle. *Reporting* only reads memory and hands bytes to a
// queue, so it is safe anywhere -- and it needs to run in more places than the save does.
//
// Called from the overworld and from the battle loop. Previously reporting happened only in
// OverworldBasic, so a whole battle's worth of experience, levels and learned moves was reported
// in one go on return to the field, and quitting from the battle summary lost all of it.
void MmoAutosave_Report(void)
{
    ReportBlocksIfChanged();

    ReportRegionsIfChanged();

    ReportPartyIfChanged();
}

void MmoAutosave_Update(void)
{
    MmoAutosave_Report();

    // Offline play keeps the old bargain: there is no server to hold the save, so nothing
    // here should be writing one behind the player's back.
    if (Net_GetAuthState() != NET_AUTH_ONLINE)
        return;

    if (!sDirty)
        return;

    // A script owns the party and the flags while it runs.
    if (ScriptContext_IsEnabled() || ArePlayerFieldControlsLocked())
        return;

    if (++sQuietFrames < AUTOSAVE_QUIET_FRAMES)
        return;

    sDirty = FALSE;
    sQuietFrames = 0;

    // The ordinary save path, so the image on the server is exactly the one the game would
    // have written itself. Every sector it touches raises the upload flag on the way past.
    TrySavingData(SAVE_NORMAL);
}
