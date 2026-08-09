// Saving, without the player ever asking for it.
//
// In a single-player Pokemon game the save is something you choose to do, and forgetting
// costs you the afternoon. That bargain makes no sense here: the character belongs to the
// server, other people can see it, and there is no reason a player should ever think about
// it. So the game saves itself, and the result goes straight to the server.
//
// What counts as a change is deliberately narrow. Flags, variables and the bag cover
// essentially all progression: the script engine reaches them through FlagSet, VarSet,
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
void MmoAutosave_Update(void)
{
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
