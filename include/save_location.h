#ifndef GUARD_SAVE_LOCATION_H
#define GUARD_SAVE_LOCATION_H

// specialSaveWarp flags
#define CONTINUE_GAME_WARP           (1 << 0)
#define POKECENTER_SAVEWARP          (1 << 1)
#define LOBBY_SAVEWARP               (1 << 2)
#define UNK_SPECIAL_SAVE_WARP_FLAG_3 (1 << 3)
//
//
//
//
#define CHAMPION_SAVEWARP            (1 << 7)

void TrySetMapSaveWarpStatus(void);
void SetChampionSaveWarp(void);
// TRUE when the player is currently inside a Pokemon Center. Deadman Mode's safezone: battles
// fought here do not permakill.
bool32 IsCurMapPokeCenter(void);

#endif // GUARD_SAVE_LOCATION_H
