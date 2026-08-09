#ifndef GUARD_MMO_BATTLE_H
#define GUARD_MMO_BATTLE_H

#include "net_client.h"

// Both players agreed and the server assigned the slots: enter the battle. See mmo_battle.c.
void MmoBattle_Start(const struct NetBattleStart *start);

#endif // GUARD_MMO_BATTLE_H
