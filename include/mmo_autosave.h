#ifndef GUARD_MMO_AUTOSAVE_H
#define GUARD_MMO_AUTOSAVE_H

// Saves the game by itself so the player never has to. See mmo_autosave.c.

// Something worth keeping changed. Cheap enough to call from the mutation funnels.
void MmoAutosave_NoteChange(void);
// Do not wait out the quiet period; save at the next safe frame.
void MmoAutosave_Flush(void);
// Ticked once per overworld frame.
void MmoAutosave_Update(void);

#endif // GUARD_MMO_AUTOSAVE_H
