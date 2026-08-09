#ifndef GUARD_MMO_PLAYERS_H
#define GUARD_MMO_PLAYERS_H

// Other players, drawn as ordinary overworld object events.

// Called once per overworld frame. Reports where we are and reconciles everyone else
// against the server's latest snapshot.
void MmoPlayers_Update(void);

// Forget every remote player without touching object events. For use when the engine has
// already torn them down, such as after a map load.
void MmoPlayers_Reset(void);

#endif // GUARD_MMO_PLAYERS_H
