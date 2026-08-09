#ifndef GUARD_MMO_LINK_H
#define GUARD_MMO_LINK_H

// Link-battle blocks carried over the network instead of a cable. See mmo_link.c.

// A battle with another player has begun, or ended. Blocks are only exchanged in between.
void MmoLink_BeginBattle(void);
void MmoLink_EndBattle(void);
bool8 MmoLink_InBattle(void);

// Send one block to the opponent and echo it to ourselves, as the cable would.
bool8 MmoLink_SendBlock(const void *src, u16 size);

// Take delivery of whatever the opponent sent. Ticked once per frame during a battle.
void MmoLink_Update(void);

#endif // GUARD_MMO_LINK_H
