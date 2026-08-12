#ifndef GUARD_MMO_WORLDITEMS_H
#define GUARD_MMO_WORLDITEMS_H

// Items dropped on the ground by other players, drawn as Poke Ball objects. The server owns the
// drops; this renders whatever it reports on the current map, picks one up when the player steps
// onto it, and adds a confirmed pickup to the bag. A parallel to mmo_players, kept separate so a
// bug here can never disturb how remote players are drawn.

// Called every field frame: sync the drawn item balls to the server's list, pick up under the
// player, and apply any confirmed pickup. A no-op when nothing is dropped.
void MmoWorldItems_Update(void);

// Forget every drawn drop, on a map change (the engine has already torn the objects down).
void MmoWorldItems_Reset(void);

#endif // GUARD_MMO_WORLDITEMS_H
