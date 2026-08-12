#ifndef GUARD_MMO_DEADMAN_H
#define GUARD_MMO_DEADMAN_H

// Deadman Mode: a separate, permadeath world. A fainted Pokemon dies forever; progression is
// capped to the next gym leader; players can only fight others near their badge count. The server
// enforces every rule from the character's mode -- this is the client half that makes the world
// behave, gated on Platform_IsDeadman() (read from pokeemerald.cfg, agreed with the sidecar).

// TRUE when this launch is a Deadman world.
bool8 MmoDeadman_IsActive(void);

// TRUE when the player is currently in a safezone (a Pokemon Center), where battles are safe and
// nothing dies.
bool8 MmoDeadman_InSafezone(void);

// Called at the end of every battle. In a Deadman world, if the battle was fought outside a
// safezone, every party Pokemon that fainted is moved to the read-only Graveyard box and cleared
// from the party -- it can never be used again. A no-op outside Deadman or inside a safezone.
void MmoDeadman_OnBattleEnd(void);

#endif // GUARD_MMO_DEADMAN_H
