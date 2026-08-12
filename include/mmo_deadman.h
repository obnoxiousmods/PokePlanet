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

// How many gym badges the player has earned (0..8).
u8 MmoDeadman_BadgeCount(void);

// The Deadman level cap for the current badge count: no party mon may exceed the next gym leader's
// level. Mirrors the server's deadman::level_cap so the client stops levelling at the same wall the
// server would refuse to cross.
u8 MmoDeadman_LevelCap(void);

// The Deadman party-size cap for the current badge count (2..6), so a young character cannot field
// a deep bench. Mirrors the server's deadman::party_cap.
u8 MmoDeadman_PartyCap(void);

// The OSRS-style combat level (3..126) from the living party's levels and badge count, shown in the
// start menu. Mirrors the server's deadman::combat_level_from so the shown number matches the rank.
u16 MmoDeadman_CombatLevel(void);

// The PC bank balance (pokedollars) the server last reported, for the bank menu to display.
u32 MmoDeadman_BankBalance(void);

// Apply any pending server bank update to the wallet. Call every field frame; a no-op outside
// Deadman or when nothing is pending.
void MmoDeadman_PollBank(void);

// TRUE when the player already owns a LIVING copy of `species`: one in the party with HP left, or
// any in the PC boxes except the graveyard (a boxed mon is always alive; a graveyard corpse is
// not). Deadman forbids encountering a species you already hold, so a capture is a commitment --
// to catch another you must let the one you have die or release it.
bool8 MmoDeadman_OwnsSpecies(u16 species);

// If this is a Deadman world and the character has no living Pokemon left anywhere (party and every
// box except the graveyard), tell the server to hard-reset the character and return TRUE, so the
// caller can restart the game to load the wiped, fresh character. Otherwise FALSE. Checked where a
// whiteout would happen -- the moment a Deadman player would otherwise be sent to a Pokemon Center.
bool8 MmoDeadman_TryHardReset(void);

#endif // GUARD_MMO_DEADMAN_H
