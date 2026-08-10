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
#include "save.h"
#include "script.h"
#include "pokemon.h"

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

    sLast = sum;
    sHaveSent = TRUE;
    Net_SendParty(gPlayerPartyCount, gPlayerParty, size);

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
static const struct { u32 offset; u32 size; } sReportable[] = {
    { 0x00,   0x24 },  // position, warps, last heal location
    { 0x988,  52   },  // Pokedex seen
    { 0x9C8,  0x66 },  // trainer rematch state
    { 0x1270, 300  },  // story flags
    { 0x139C, 512  },  // story variables
    { 0x159C, 256  },  // the sixty-four counters
    { 0x169C, 0x400 }, // berry trees
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

    sNext = (sNext + 1) % ARRAY_COUNT(sReportable);

    if (base == NULL)
        return;

    for (i = 0; i < sReportable[which].size; i++)
    {
        sum ^= base[sReportable[which].offset + i];
        sum *= 16777619u;
    }

    if (sHaveSent[which] && sum == sLast[which])
        return;

    sLast[which] = sum;
    sHaveSent[which] = TRUE;
    Net_SendRegion(sReportable[which].offset,
                   base + sReportable[which].offset,
                   sReportable[which].size);
}

void MmoAutosave_Update(void)
{
    ReportRegionsIfChanged();

    ReportPartyIfChanged();

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
