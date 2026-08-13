#ifndef GUARD_PLATFORM_H
#define GUARD_PLATFORM_H

#include "global.h"
#include "siirtc.h"

// Ask the game to shut down cleanly. Safe to call from any thread.
void Platform_RequestQuit(void);
// Start the network sidecar. Harmless if one is already running: the newcomer finds the
// IPC port taken and exits, leaving the existing one serving.
void Platform_LaunchSidecar(void);

// TRUE when this launch is a Deadman world (permadeath, badge caps, safezones, forced PvP). Read
// from pokeemerald.cfg (mode=deadman), the same value the sidecar reads and the server enforces.
// Game code branches its Deadman rules on this; the server independently enforces them, so this
// only decides what the client shows itself.
bool8 Platform_IsDeadman(void);

// Commit the world the player picked at the main-menu world select (see Net_GetModeProfiles).
void Platform_SetMode(const char *mode);

// Typing with the real keyboard, for chat. While active the button mapping is suppressed,
// so the game sees no input at all until the player finishes.
void Platform_BeginTextInput(void);
void Platform_EndTextInput(void);
// Copies what has been typed. 0 while still typing, 1 on Enter, 2 on Escape.
u8 Platform_PollTextInput(char *out, u8 outSize);
bool8 Platform_IsTextInputActive(void);
// TRUE once per Shift+Enter press: the dedicated "open chat" key, kept off plain Enter so it
// does not collide with the START button.
bool8 Platform_ConsumeChatOpen(void);
void Platform_StoreSaveFile(void);
void Platform_ReadFlash(u16 sectorNum, u32 offset, u8 *dest, u32 size);
void Platform_QueueAudio(float *audioBuffer, s32 samplesPerFrame);
u16 Platform_GetKeyInput(void);
u8 Platform_GetBorderBackgroundCount(void);
u8 Platform_GetBorderBackground(void);
void Platform_SetBorderBackground(u8 selection);

enum PlatformSetting
{
    PLATFORM_SETTING_FULLSCREEN,
    PLATFORM_SETTING_WINDOW_SCALE,
    PLATFORM_SETTING_INTEGER_SCALE,
    PLATFORM_SETTING_VSYNC,
    PLATFORM_SETTING_BORDER,
    PLATFORM_SETTING_VOLUME,
    PLATFORM_SETTING_COUNT,
};

u8 Platform_GetSetting(enum PlatformSetting setting);
void Platform_SetSetting(enum PlatformSetting setting, u8 value);
void Platform_GetStatus(struct SiiRtcInfo *rtc);
void Platform_SetStatus(struct SiiRtcInfo *rtc);
static void UpdateInternalClock(void);
void Platform_GetDateTime(struct SiiRtcInfo *rtc);
void Platform_SetDateTime(struct SiiRtcInfo *rtc);
void Platform_GetTime(struct SiiRtcInfo *rtc);
void Platform_SetTime(struct SiiRtcInfo *rtc);
void Platform_SetAlarm(u8 *alarmData);

#endif

// Report state to a supervising process, when one is driving this instance. A no-op otherwise.
void Platform_ReportState(const void *bytes, u32 size);
