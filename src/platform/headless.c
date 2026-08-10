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
//   - no input is injected, so the game sits wherever the title screen leaves it
//   - the server does not instantiate or talk to this
//   - there is no make target yet; a Linux one is needed for this to run on the server
//
// Those are the next three pieces, in that order.

// Compiled only for the headless target.
//
// The build globs src/*/*.c, so without this guard this file joins the SDL2 build and defines
// main and VBlankIntrWait a second time -- which is exactly what happened, and it broke the
// client link. A platform layer has to be chosen, never merely present.
#ifdef PLATFORM_HEADLESS

#include "global.h"
#include "platform.h"

#include <pthread.h>
#include <semaphore.h>
#include <time.h>

extern void AgbMain(void);

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

// The frame loop, with nothing to show for itself.
//
// VBlankIntrWait is the only point at which the game yields, so it is the only lever a host has
// over it: the game thread parks here every frame and cannot proceed until something releases
// it. On the SDL2 build that release is tied to presenting a frame, which is why the game runs
// at the speed of the display. Nothing here is drawing anything, so the release is on a timer
// instead -- and that timer is the whole reason this target is interesting.
//
// A server driving this does not have to run it at sixty frames a second. It can run a
// simulation as fast as the machine allows to catch up, or hold it still, because the pace is
// now a decision rather than a property of a monitor. That is the difference between the game
// being *shown* somewhere and the game being *run* somewhere.
//
// pthreads rather than SDL: this has to build for the server eventually, and pthreads is the
// one threading interface both a Linux host and the existing mingw toolchain already have.

static sem_t sVBlank;
static pthread_t sGameThread;

static void *GameThread(void *unused)
{
    (void)unused;
    AgbMain();
    return NULL;
}

void VBlankIntrWait(void)
{
    sem_wait(&sVBlank);
}

int main(int argc, char **argv)
{
    struct timespec frame;

    (void)argc;
    (void)argv;

    if (sem_init(&sVBlank, 0, 0) != 0)
        return 1;
    if (pthread_create(&sGameThread, NULL, GameThread, NULL) != 0)
        return 1;

    // A sixtieth of a second, matching the hardware this game was written for. Kept as a
    // constant rather than a setting because nothing here consumes the output yet; when the
    // server drives this, the pace becomes its decision and this loop goes away.
    frame.tv_sec = 0;
    frame.tv_nsec = 16666667L;

    for (;;)
    {
        sem_post(&sVBlank);
        nanosleep(&frame, NULL);
    }

    return 0;
}

#endif // PLATFORM_HEADLESS
