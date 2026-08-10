// The platform layer for a build with no screen, no speakers and no keyboard.
//
// The point of this target is not to play the game -- it is to be able to *run* it somewhere the
// player cannot reach. Everything the server currently checks is a rule about a result: a level
// it will accept, a rate of money it will not. Rules about results can only ever catch a forgery
// that is careless. Running the logic itself is what makes a careful one impossible, because
// then the server is not checking the answer, it has the answer.
//
// The stubs below are generated from include/platform.h so they cannot drift from the contract
// they stand in for. They are deliberately inert rather than clever: this target exists to prove
// the game's logic will build and run detached from its presentation, which is the thing that
// was never true before and the precondition for everything else.
//
// What is NOT here yet, stated plainly so nobody mistakes this for the finished article:
//   - nothing drives the frame loop; AgbMain is never entered
//   - no input is injected, so the game would sit at the title screen if it were
//   - the server does not instantiate or talk to this
//
// Those are the next three pieces, in that order. This one is the foundation they need: a build
// of the game with the platform sawn off.

#include "global.h"
#include "platform.h"

void Platform_RequestQuit(void)
{
}

void Platform_LaunchSidecar(void)
{
}

void Platform_BeginTextInput(void)
{
}

void Platform_EndTextInput(void)
{
}

u8 Platform_PollTextInput(char *out, u8 outSize)
{
    return 0;
}

bool8 Platform_IsTextInputActive(void)
{
    return FALSE;
}

void Platform_StoreSaveFile(void)
{
}

void Platform_ReadFlash(u16 sectorNum, u32 offset, u8 *dest, u32 size)
{
}

void Platform_QueueAudio(float *audioBuffer, s32 samplesPerFrame)
{
}

u16 Platform_GetKeyInput(void)
{
    return 0;
}

u8 Platform_GetBorderBackgroundCount(void)
{
    return 0;
}

u8 Platform_GetBorderBackground(void)
{
    return 0;
}

void Platform_SetBorderBackground(u8 selection)
{
}

u8 Platform_GetSetting(enum PlatformSetting setting)
{
    return 0;
}

void Platform_SetSetting(enum PlatformSetting setting, u8 value)
{
}

void Platform_GetStatus(struct SiiRtcInfo *rtc)
{
}

void Platform_SetStatus(struct SiiRtcInfo *rtc)
{
}

void Platform_GetDateTime(struct SiiRtcInfo *rtc)
{
}

void Platform_SetDateTime(struct SiiRtcInfo *rtc)
{
}

void Platform_GetTime(struct SiiRtcInfo *rtc)
{
}

void Platform_SetTime(struct SiiRtcInfo *rtc)
{
}

void Platform_SetAlarm(u8 *alarmData)
{
}
