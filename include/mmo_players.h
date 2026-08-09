#ifndef GUARD_MMO_PLAYERS_H
#define GUARD_MMO_PLAYERS_H

// Other players, drawn as ordinary overworld object events.

// Called once per overworld frame. Reports where we are and reconciles everyone else
// against the server's latest snapshot.
void MmoPlayers_Update(void);

// Forget every remote player without touching object events. For use when the engine has
// already torn them down, such as after a map load.
void MmoPlayers_Reset(void);

// True if this object event is another player rather than one of the map's own NPCs.
bool8 MmoPlayers_IsRemoteObject(u8 objectEventId);

// Script to run when the player talks to another player. Remote players have no entry in
// the map's template list, so the engine's usual script lookup would dereference NULL.
const u8 *MmoPlayers_GetInteractionScript(u8 objectEventId);

// Display name of the player occupying an object event, or NULL if it is not a remote
// player. Valid until the next snapshot is applied.
const char *MmoPlayers_GetRemoteName(u8 objectEventId);

#endif // GUARD_MMO_PLAYERS_H
