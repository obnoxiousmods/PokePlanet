#ifndef GUARD_MMO_CHAT_H
#define GUARD_MMO_CHAT_H

// Shows arriving chat over the overworld. Ticked once per frame from OverworldBasic.
void MmoChat_Update(void);

// Forget the window without clearing it, for a map change that tears every window down.
void MmoChat_Reset(void);

#endif // GUARD_MMO_CHAT_H
