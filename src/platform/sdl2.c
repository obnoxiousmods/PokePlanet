#ifdef PLATFORM_SDL2
#include <assert.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>

#ifdef _WIN32
#include <windows.h>
#include <xinput.h>
#endif

#ifdef __ANDROID__
#include <jni.h>
#include <SDL.h>
#else
#include <SDL2/SDL.h>
#endif
#if defined(NATIVE_LINUX) && !defined(NO_SDL_IMAGE)
// SDL_image draws the decorative border and background art around the game screen. A server
// running the game headlessly has no screen to put a border around, and lib32-SDL2_image is not
// packaged everywhere, so the dependency is optional rather than assumed.
#ifndef NO_SDL_IMAGE
#include <SDL2/SDL_image.h>
#endif
#endif

#ifdef NATIVE_LINUX
#include <unistd.h>
#include <sys/types.h>
#endif
#include "global.h"
#include "platform.h"
#include "rtc.h"
#include "gba/defines.h"
#include "gba/m4a_internal.h"
#include "cgb_audio.h"
#include "gba/flash_internal.h"
#include "platform/dma.h"
#include "platform/framedraw.h"
#include "net_client.h"

extern void (*const gIntrTable[])(void);

// SDL_Log output is mirrored to pokeplanet.log so diagnostics survive when the game
// owns its own console window (which vanishes with the process) or is launched
// with no console at all.
static FILE *sLogFile = NULL;

static void PokePlanetLogOutput(void *userdata, int category, SDL_LogPriority priority, const char *message)
{
    if (sLogFile != NULL)
    {
        fprintf(sLogFile, "%s\n", message);
        fflush(sLogFile);
    }
    fprintf(stderr, "%s\n", message);
    fflush(stderr);
}

SDL_Thread *mainLoopThread;
SDL_Window *sdlWindow;
SDL_Renderer *sdlRenderer;
SDL_Texture *sdlTexture;
#if defined(NATIVE_LINUX) || defined(_WIN32)
#define MAX_BORDER_BACKGROUNDS 15
SDL_Texture *sdlBackgroundTextures[MAX_BORDER_BACKGROUNDS];
SDL_Texture *sdlBorderTexture;
#endif
static u8 sBorderBackgroundCount = 1;
SDL_AudioDeviceID sdlAudioDevice;
SDL_sem *vBlankSemaphore;
SDL_atomic_t isFrameAvailable;
bool speedUp = false;
unsigned int videoScale = 1;
bool isRunning = true;
bool paused = false;
double simTime = 0;
double lastGameTime = 0;
double curGameTime = 0;
double fixedTimestep = 1.0 / 60.0; // 16.666667ms
double timeScale = 1.0;
struct SiiRtcInfo internalClock;

static FILE *sSaveFile = NULL;
static char sSavePath[1024] = "pokeemerald.sav";
static char sConfigPath[1024] = "pokeemerald.cfg";
static char sLogPath[1024] = "pokeplanet.log";
static char sTokenPath[1024] = "pokeplanet-auth.json";
static char sSidecarLogPath[1024] = "pokeplanet-net.log";

// Which instance this is, taken from the executable's own name: pokeplanet.exe runs the
// default profile and pokeplanet_tester.exe runs "tester".
//
// Two clients on one machine would otherwise fight over the same save, config, log, token
// cache and sidecar port, so each profile gets its own copy of all five. That makes testing
// multiplayer a matter of double-clicking two icons rather than juggling directories.
static char sProfile[64] = "";

// Multiplayer endpoint. The game itself only needs the sidecar port; the server address
// is kept here so the two processes read one config file and so StoreConfigFile can write
// it back untouched instead of silently dropping it.
#define DEFAULT_SERVER_HOST  "pokeplanet.obby.ca"
#define DEFAULT_SERVER_PORT  4433
#define DEFAULT_SIDECAR_PORT 38400
static char sServerHost[128] = DEFAULT_SERVER_HOST;
static unsigned int sServerPort = DEFAULT_SERVER_PORT;
static unsigned int sSidecarPort = DEFAULT_SIDECAR_PORT;

u16 Platform_GetSidecarPort(void)
{
    return (u16)sSidecarPort;
}

// Names this launch, so a game and the sidecar it starts can only pair with each other.
//
// A port was doing this job and is not up to it: two sidecars can be listening at once -- a
// player's and a developer's, or one a crash left behind -- and a game that merely connects
// to a port can be signed in as somebody else's character. Generated once, before anything
// connects, and never reused.
//
// Not a secret and does not need to be. Both ends are on loopback and anything able to read
// this process's memory has already won; the job is telling two local programs apart, not
// keeping anyone out.
static char sInstanceToken[33];

// Identifies this installation to its sidecar. Stable across restarts, by design.
//
// It used to be generated fresh every launch, which quietly broke restarting. A sidecar
// outlives the game briefly on purpose, so a relaunch can reuse it instead of signing in again
// -- but with a new token each time, the returning game was refused as a stranger. It then
// retried once a second, and every refusal pushed the sidecar's shutdown timer out again, so
// the sidecar never exited and never released the port. The player got a permanently offline
// game and a process that had to be killed by hand.
//
// Persisting it makes a relaunch what it looks like: the same game coming back. The file sits
// beside the save so a profile gets its own, matching how everything else here is separated.
const char *Platform_GetInstanceToken(void)
{
    if (sInstanceToken[0] == '\0')
    {
        static const char digits[] = "0123456789abcdef";
        char path[FILENAME_MAX];
        FILE *f;
        int i;

        snprintf(path, sizeof(path), "%s.instance", sSavePath);

        f = fopen(path, "rb");
        if (f != NULL)
        {
            size_t got = fread(sInstanceToken, 1, 32, f);
            fclose(f);
            if (got == 32)
            {
                // Only accept what we would have written. A truncated or edited file must not
                // become a token that half matches.
                for (i = 0; i < 32; i++)
                {
                    if (strchr(digits, sInstanceToken[i]) == NULL || sInstanceToken[i] == '\0')
                    {
                        sInstanceToken[0] = '\0';
                        break;
                    }
                }
                if (sInstanceToken[0] != '\0')
                {
                    sInstanceToken[32] = '\0';
                    return sInstanceToken;
                }
            }
            sInstanceToken[0] = '\0';
        }

        {
            Uint64 seed = SDL_GetPerformanceCounter() ^ ((Uint64)SDL_GetTicks() << 32);

            for (i = 0; i < 32; i++)
            {
                seed = seed * 6364136223846793005ULL + 1442695040888963407ULL;
                sInstanceToken[i] = digits[(seed >> 33) & 0xF];
            }
            sInstanceToken[32] = '\0';
        }

        // Best effort. If it cannot be written the game still works -- it just goes back to
        // being a stranger to its own sidecar after a restart, which is where this started.
        f = fopen(path, "wb");
        if (f != NULL)
        {
            fwrite(sInstanceToken, 1, 32, f);
            fclose(f);
        }
        else
        {
            SDL_Log("could not save the instance token to %s; restarts will not reuse the sidecar",
                    path);
        }
    }

    return sInstanceToken;
}

// Work out which instance we are from argv[0], and give it its own files.
//
// The name is everything after the first underscore in the executable's basename, so
// pokeplanet.exe is the default profile and pokeplanet_tester.exe is "tester". Naming the
// copy is the whole configuration step; there is nothing else to set up.
//
// A named profile also moves off the default sidecar port so two clients do not try to
// share one sidecar, which would sign them both in as the same account. Beyond a second
// instance, give each profile its own sidecarPort in its own config file.
static void DeriveProfile(const char *argv0)
{
    const char *base;
    const char *slash;
    const char *underscore;

    if (argv0 == NULL || *argv0 == '\0')
        return;

    base = argv0;
    for (slash = argv0; *slash != '\0'; slash++)
    {
        if (*slash == '/' || *slash == '\\')
            base = slash + 1;
    }

    underscore = SDL_strchr(base, '_');
    if (underscore == NULL || underscore[1] == '\0')
        return;

    // Truncates safely if someone names a copy something absurd.
    SDL_strlcpy(sProfile, underscore + 1, sizeof(sProfile));

    // Drop the extension, so "pokeplanet_tester.exe" yields "tester" rather than
    // "tester.exe" and the files it opens are not named after one.
    {
        char *dot = SDL_strrchr(sProfile, '.');
        if (dot != NULL)
            *dot = '\0';
    }
    if (sProfile[0] == '\0')
        return;

    SDL_snprintf(sSavePath, sizeof(sSavePath), "pokeemerald-%s.sav", sProfile);
    SDL_snprintf(sConfigPath, sizeof(sConfigPath), "pokeemerald-%s.cfg", sProfile);
    SDL_snprintf(sLogPath, sizeof(sLogPath), "pokeplanet-%s.log", sProfile);
    SDL_snprintf(sTokenPath, sizeof(sTokenPath), "pokeplanet-auth-%s.json", sProfile);
    SDL_snprintf(sSidecarLogPath, sizeof(sSidecarLogPath), "pokeplanet-net-%s.log", sProfile);
    sSidecarPort = DEFAULT_SIDECAR_PORT + 1;
}

// Game-side multiplayer diagnostics land in pokeplanet.log alongside the platform's own.
void Platform_LogMultiplayer(const char *line)
{
    SDL_Log("mmo: %s", line);
}

// Shut down as though the window had been closed, so the save is flushed and the sidecar
// is left in a sane state. Pushed as an event rather than setting a flag directly because
// the caller is usually the network thread, and SDL_PushEvent is the thread-safe way in.
void Platform_RequestQuit(void)
{
    SDL_Event quit;

    memset(&quit, 0, sizeof(quit));
    quit.type = SDL_QUIT;
    SDL_PushEvent(&quit);
}

const char *Platform_GetServerHost(void)
{
    return sServerHost;
}

u16 Platform_GetServerPort(void)
{
    return (u16)sServerPort;
}
static u8 sBorderBackground;
static bool sHasBorderBackgroundConfig;
static u8 sBackgroundOrderVersion;
static u8 sPlatformSettings[PLATFORM_SETTING_COUNT] = {0, 4, 0, 1, 1, 10};
#ifdef __ANDROID__
static SDL_GameController *androidController;
#endif

extern void AgbMain(void);
extern void DoSoftReset(void);

int DoMain(void *param);
void ProcessEvents(void);
void VDraw(SDL_Texture *texture);
static void ReadConfigFile(void);

// Start pokeplanet-net.exe alongside the game.
//
// The sidecar is a separate process so QUIC and TLS stay out of this 32-bit binary. It
// is optional: if it is missing or fails to start, Net_Init simply never links and the
// game runs single-player.
void Platform_LaunchSidecar(void)
{
#ifdef _WIN32
    STARTUPINFOA startup;
    PROCESS_INFORMATION process;
    char commandLine[512];

    // A sidecar may already be running -- a second copy of the game on this machine, or
    // one started by hand for debugging. It owns the IPC port, so ours would just fail
    // to bind; connecting to the existing one is the correct behaviour either way.
    // The token cache is per profile too, so a second instance signs in as its own
    // account instead of silently reusing the first one's session.
    //
    // A named profile is pinned to whatever account its token names and never offered a
    // browser login. Otherwise signing in would resolve to whoever is at the keyboard --
    // the same person already playing the main client -- and the two would fight over one
    // identity rather than being two players who can see each other.
    snprintf(commandLine, sizeof(commandLine),
             "pokeplanet-net.exe --server %s --port %u --ipc-port %u --token %s --log %s"
             " --instance %s%s",
             sServerHost, sServerPort, sSidecarPort, sTokenPath, sSidecarLogPath,
             Platform_GetInstanceToken(),
             sProfile[0] != '\0' ? " --fixed-token" : "");

    memset(&startup, 0, sizeof(startup));
    startup.cb = sizeof(startup);
    memset(&process, 0, sizeof(process));

    if (CreateProcessA(NULL, commandLine, NULL, NULL, FALSE,
                       CREATE_NO_WINDOW, NULL, NULL, &startup, &process))
    {
        SDL_Log("net: started sidecar (pid %lu)", (unsigned long)process.dwProcessId);
        // We never wait on it; closing the handles just releases our references.
        CloseHandle(process.hThread);
        CloseHandle(process.hProcess);
    }
    else
    {
        SDL_Log("net: no sidecar started (error %lu); running offline",
                (unsigned long)GetLastError());
    }
#elif defined(NATIVE_LINUX)
    // The same thing for a POSIX host, which is what a server-run instance is.
    //
    // This whole function used to be inside #ifdef _WIN32, so on Linux it was an empty body:
    // the game logged "starting one" and then did nothing at all. That is invisible on a
    // desktop, where the sidecar is usually already there, and fatal for an instance meant to
    // run unattended on the server -- it could never bring up its own connection.
    {
        char port[16], ipcPort[16], serverPort[16];
        pid_t pid;

        snprintf(serverPort, sizeof(serverPort), "%u", sServerPort);
        snprintf(ipcPort, sizeof(ipcPort), "%u", sSidecarPort);
        (void)port;

        pid = fork();
        if (pid == 0)
        {
            // Child. Detach from the parent's terminal so it is not killed by a stray signal
            // sent to the process group, which is how the game's own test runs kept dying.
            setsid();

            if (sProfile[0] != '\0')
            {
                execlp("pokeplanet-net", "pokeplanet-net",
                       "--server", sServerHost, "--port", serverPort,
                       "--ipc-port", ipcPort, "--token", sTokenPath,
                       "--log", sSidecarLogPath,
                       "--instance", Platform_GetInstanceToken(),
                       "--fixed-token", (char *)NULL);
            }
            else
            {
                execlp("pokeplanet-net", "pokeplanet-net",
                       "--server", sServerHost, "--port", serverPort,
                       "--ipc-port", ipcPort, "--token", sTokenPath,
                       "--log", sSidecarLogPath,
                       "--instance", Platform_GetInstanceToken(), (char *)NULL);
            }

            // Only reached if exec failed. _exit rather than exit: this is a forked copy of a
            // process holding SDL and a game thread, and running atexit handlers here would
            // tear down state the parent still owns.
            _exit(127);
        }
        else if (pid > 0)
        {
            SDL_Log("net: started sidecar (pid %d)", (int)pid);
        }
        else
        {
            SDL_Log("net: could not fork a sidecar; running offline");
        }
    }
#endif
}

static void ReadSaveFile(const char *path);
static void ReadConfigFile(void);
static void StoreConfigFile(void);
static void ApplyPlatformSettings(void);
static void StoreSaveFile(void);
static void CloseSaveFile(void);

static void UpdateInternalClock(void);

#ifdef __ANDROID__
static void HandleTouchEvent(const SDL_TouchFingerEvent *event);
static void DrawTouchControls(void);
#endif

// Applies a pending display change; defined below, used by the main loop.
static void ApplyPendingDisplayChange(void);

int main(int argc, char **argv)
{
    // Open an output console on Windows.
    // Detach from any inherited console first. When the game is launched from an
    // existing console (or a ConPTY), AllocConsole fails and we keep sharing the
    // launcher's console -- so when that launcher exits, its console teardown sends
    // us a Ctrl-Close event, SDL turns it into SDL_QUIT, and the game dies on its
    // own. FreeConsole guarantees we always own the console we log to.
#ifdef _WIN32
    FreeConsole();
    if (AllocConsole())
    {
        freopen("CONOUT$", "w", stdout);
        freopen("CONOUT$", "w", stderr);
    }
#endif

    // Before anything opens a file: this decides which set of files we use.
    DeriveProfile(argc > 0 ? argv[0] : NULL);

    sLogFile = fopen(sLogPath, "w");
    SDL_LogSetOutputFunction(PokePlanetLogOutput, NULL);
    SDL_LogSetAllPriority(SDL_LOG_PRIORITY_INFO);
    SDL_Log("PokePlanet starting up");
    if (sProfile[0] != '\0')
        SDL_Log("profile: %s (save %s, sidecar port %u)", sProfile, sSavePath, sSidecarPort);

    // ReadConfigFile is called later during video setup, but the sidecar needs the
    // server address before it launches, so read the file once up front.
    ReadConfigFile();

    // A port for tests to use, so a harness can never take a player's session.
    //
    // WSL forwards localhost, so a headless test binding the default port in Linux is
    // reachable from a real client running on Windows -- which attached to it, signed in as
    // whatever character the test was using, and left the player being someone else in their
    // own game. The bug was mine and the fix belongs here rather than in a note telling
    // future harnesses to be careful.
    {
        const char *port = SDL_getenv("POKEPLANET_SIDECAR_PORT");

        if (port != NULL && *port != '\0')
        {
            int value = SDL_atoi(port);

            if (value > 0 && value < 65536)
            {
                sSidecarPort = (unsigned int)value;
                SDL_Log("sidecar port overridden to %u", sSidecarPort);
            }
        }
    }
    Platform_LaunchSidecar();
    Net_Init();

#ifdef __ANDROID__
    SDL_setenv("SDL_AUDIODRIVER", "openslES", 1);
    SDL_SetHint(SDL_HINT_TOUCH_MOUSE_EVENTS, "0");
    SDL_SetHint(SDL_HINT_MOUSE_TOUCH_EVENTS, "0");
#endif
    if(SDL_Init(SDL_INIT_VIDEO | SDL_INIT_AUDIO
#ifdef __ANDROID__
                | SDL_INIT_GAMECONTROLLER
#endif
                ) < 0)
    {
        SDL_Log("SDL could not initialize! SDL_Error: %s", SDL_GetError());
        return 1;
    }

#ifdef __ANDROID__
    for (int i = 0; i < SDL_NumJoysticks() && androidController == NULL; i++)
    {
        if (SDL_IsGameController(i))
            androidController = SDL_GameControllerOpen(i);
    }
#endif

#ifdef __ANDROID__
    char *prefPath = SDL_GetPrefPath("pokeemerald", "pokeemerald");
    if (prefPath != NULL)
    {
        SDL_snprintf(sSavePath, sizeof(sSavePath), "%spokeemerald.sav", prefPath);
        SDL_snprintf(sConfigPath, sizeof(sConfigPath), "%spokeemerald.cfg", prefPath);
        SDL_free(prefPath);
    }
#endif
    ReadSaveFile(sSavePath);
    ReadConfigFile();

#ifdef __ANDROID__
    SDL_SetHint(SDL_HINT_ORIENTATIONS, "LandscapeLeft LandscapeRight");
#endif
#if defined(NATIVE_LINUX) || defined(_WIN32)
    sdlWindow = SDL_CreateWindow("Pokemon Emerald", SDL_WINDOWPOS_CENTERED, SDL_WINDOWPOS_CENTERED, 1280, 720, SDL_WINDOW_SHOWN | SDL_WINDOW_RESIZABLE);
#else
    sdlWindow = SDL_CreateWindow("pokeemerald", SDL_WINDOWPOS_CENTERED, SDL_WINDOWPOS_CENTERED, DISPLAY_WIDTH * videoScale, DISPLAY_HEIGHT * videoScale, SDL_WINDOW_SHOWN | SDL_WINDOW_RESIZABLE);
#endif
    if (sdlWindow == NULL)
    {
        SDL_Log("Window could not be created! SDL_Error: %s", SDL_GetError());
        return 1;
    }

#ifdef __ANDROID__
    sdlRenderer = SDL_CreateRenderer(sdlWindow, -1, SDL_RENDERER_ACCELERATED);
#else
    sdlRenderer = SDL_CreateRenderer(sdlWindow, -1, SDL_RENDERER_PRESENTVSYNC);
#endif
    if (sdlRenderer == NULL)
    {
        SDL_Log("Renderer could not be created! SDL_Error: %s", SDL_GetError());
        return 1;
    }

    SDL_SetRenderDrawColor(sdlRenderer, 0, 0, 0, 255);
    SDL_RenderClear(sdlRenderer);
    SDL_SetHint(SDL_HINT_RENDER_SCALE_QUALITY, "0");

    for (int i = 1; i < 15; i++)
    {
        char filename[16];
#ifdef _WIN32
        snprintf(filename, sizeof(filename), "BG%d.bmp", i);
#else
        snprintf(filename, sizeof(filename), "BG%d.png", i);
#endif
        SDL_RWops *backgroundFile = SDL_RWFromFile(filename, "rb");
        if (backgroundFile == NULL)
            break;
        SDL_RWclose(backgroundFile);
        sBorderBackgroundCount++;
    }
    if (sBackgroundOrderVersion < 2)
    {
        if (sHasBorderBackgroundConfig)
        {
            if (sBorderBackground == 1)
                sBorderBackground = sBorderBackgroundCount;
            else if (sBorderBackground >= 2)
                sBorderBackground--;
        }
        sBackgroundOrderVersion = 2;
        StoreConfigFile();
    }
#if defined(NATIVE_LINUX) && defined(NO_SDL_IMAGE)
    // Debug build without SDL2_image: no border or background artwork, plain black.
    SDL_RenderSetLogicalSize(sdlRenderer, 0, 0);
#elif defined(NATIVE_LINUX)
    SDL_RenderSetLogicalSize(sdlRenderer, 0, 0);
#ifndef NO_SDL_IMAGE
    if ((IMG_Init(IMG_INIT_PNG) & IMG_INIT_PNG) == 0)
    {
        SDL_Log("SDL_image could not initialize: %s", IMG_GetError());
    }
    else
    {
        for (int i = 0; i < sBorderBackgroundCount; i++)
        {
            char filename[16];
            snprintf(filename, sizeof(filename), i == 0 ? "BG.png" : "BG%d.png", i);
            sdlBackgroundTextures[i] = IMG_LoadTexture(sdlRenderer, filename);
        }
        sdlBorderTexture = IMG_LoadTexture(sdlRenderer, "Border.png");
        if (sdlBackgroundTextures[0] == NULL)
            SDL_Log("Background image could not be loaded: %s", IMG_GetError());
        if (sdlBorderTexture == NULL)
            SDL_Log("Border image could not be loaded: %s", IMG_GetError());
#endif // NO_SDL_IMAGE
    }
#elif defined(_WIN32)
    SDL_RenderSetLogicalSize(sdlRenderer, 0, 0);
    SDL_Surface *borderSurface = SDL_LoadBMP("Border.bmp");
    for (int i = 0; i < sBorderBackgroundCount; i++)
    {
        char filename[16];
        snprintf(filename, sizeof(filename), i == 0 ? "BG.bmp" : "BG%d.bmp", i);
        SDL_Surface *backgroundSurface = SDL_LoadBMP(filename);
        if (backgroundSurface == NULL)
            continue;
        sdlBackgroundTextures[i] = SDL_CreateTextureFromSurface(sdlRenderer, backgroundSurface);
        SDL_FreeSurface(backgroundSurface);
    }
    if (sdlBackgroundTextures[0] == NULL)
        SDL_Log("Background image could not be loaded: %s", SDL_GetError());
    if (borderSurface == NULL)
    {
        SDL_Log("Border image could not be loaded: %s", SDL_GetError());
    }
    else
    {
        sdlBorderTexture = SDL_CreateTextureFromSurface(sdlRenderer, borderSurface);
        SDL_FreeSurface(borderSurface);
    }
#else
    SDL_RenderSetLogicalSize(sdlRenderer, DISPLAY_WIDTH, DISPLAY_HEIGHT);
    SDL_RenderSetIntegerScale(sdlRenderer, SDL_TRUE);
#endif
    ApplyPlatformSettings();

    sdlTexture = SDL_CreateTexture(sdlRenderer,
                                   SDL_PIXELFORMAT_ARGB8888,
                                   SDL_TEXTUREACCESS_STREAMING,
                                   DISPLAY_WIDTH, DISPLAY_HEIGHT);
    if (sdlTexture == NULL)
    {
        SDL_Log("Texture could not be created! SDL_Error: %s", SDL_GetError());
        return 1;
    }
    SDL_SetTextureBlendMode(sdlTexture, SDL_BLENDMODE_NONE);

    simTime = curGameTime = lastGameTime = SDL_GetPerformanceCounter();

    isFrameAvailable.value = 0;
    vBlankSemaphore = SDL_CreateSemaphore(0);

    SDL_AudioSpec want;

    SDL_memset(&want, 0, sizeof(want)); /* or SDL_zero(want) */
    want.freq = 42060;
    want.format = AUDIO_F32;
    want.channels = 2;
    want.samples = 1024;
    cgb_audio_init(want.freq);


    sdlAudioDevice = SDL_OpenAudioDevice(NULL, 0, &want, NULL, 0);
    if (sdlAudioDevice == 0)
        SDL_Log("Failed to open audio: %s", SDL_GetError());
    else
    {
        if (want.format != AUDIO_F32) /* we let this one thing change. */
            SDL_Log("We didn't get Float32 audio format.");
        SDL_PauseAudioDevice(sdlAudioDevice, 0);
    }
#ifndef __ANDROID__
    VDraw(sdlTexture);
#endif
    mainLoopThread = SDL_CreateThread(DoMain, "AgbMain", NULL);

    double accumulator = 0.0;

    memset(&internalClock, 0, sizeof(internalClock));
    internalClock.status = SIIRTCINFO_24HOUR;
    UpdateInternalClock();

    while (isRunning)
    {
        ProcessEvents();
        // Window and renderer changes belong to this thread; see Platform_SetSetting.
        ApplyPendingDisplayChange();

        if (!paused)
        {
            double dt = fixedTimestep / timeScale; // TODO: Fix speedup

            curGameTime = SDL_GetPerformanceCounter();
            double deltaTime = (double)((curGameTime - lastGameTime) / (double)SDL_GetPerformanceFrequency());
            if (deltaTime > (dt * 5))
                deltaTime = dt;
            lastGameTime = curGameTime;

            accumulator += deltaTime;

            while (accumulator >= dt)
            {
                // Drained unconditionally. This used to sit inside the branch below, so a
                // frame the game thread had not produced yet meant the accumulator never went
                // down and this loop spun forever -- never returning to ProcessEvents().
                //
                // That turned any pause of the game thread into a permanent hang of both, and
                // the display menu made it reachable: toggling fullscreen calls window
                // functions from the game thread, those block until the window's owning thread
                // pumps messages, and the owning thread was stuck right here waiting for a
                // frame that could not come. Each waiting on the other.
                //
                // Falling behind is now what it should be -- dropped frames -- rather than a
                // freeze.
                accumulator -= dt;

                if (SDL_AtomicGet(&isFrameAvailable))
                {
                    VDraw(sdlTexture);
                    SDL_RenderClear(sdlRenderer);
#if defined(NATIVE_LINUX) || defined(_WIN32)
                    u8 backgroundOption = Platform_GetBorderBackground();
                    if (backgroundOption < sBorderBackgroundCount
                     && sdlBackgroundTextures[backgroundOption] != NULL)
                        SDL_RenderCopy(sdlRenderer, sdlBackgroundTextures[backgroundOption], NULL, NULL);
                    int outputWidth;
                    int outputHeight;
                    SDL_GetRendererOutputSize(sdlRenderer, &outputWidth, &outputHeight);
                    int gameHeight;
                    int gameWidth;
                    if (sPlatformSettings[PLATFORM_SETTING_INTEGER_SCALE])
                    {
                        int scale = outputWidth / DISPLAY_WIDTH;
                        if (outputHeight / DISPLAY_HEIGHT < scale)
                            scale = outputHeight / DISPLAY_HEIGHT;
                        if (scale < 1)
                            scale = 1;
                        gameWidth = DISPLAY_WIDTH * scale;
                        gameHeight = DISPLAY_HEIGHT * scale;
                    }
                    else
                    {
                        gameHeight = outputHeight * 8 / 9;
                        gameWidth = gameHeight * 3 / 2;
                    }
                    SDL_Rect gameViewport = {(outputWidth - gameWidth) / 2,
                                             (outputHeight - gameHeight) / 2,
                                             gameWidth, gameHeight};
                    SDL_RenderCopy(sdlRenderer, sdlTexture, NULL, &gameViewport);
                    if (sPlatformSettings[PLATFORM_SETTING_BORDER] && sdlBorderTexture != NULL)
                    {
                        SDL_Rect borderSource = {141, 18, 1000, 683};
                        int innerWidth = gameViewport.w - 2;
                        int innerHeight = gameViewport.h - 2;
                        SDL_Rect borderViewport = {
                            gameViewport.x + 1 - innerWidth * 19 / 961,
                            gameViewport.y + 1 - innerHeight * 20 / 643,
                            innerWidth * 1000 / 961,
                            innerHeight * 683 / 643
                        };
                        SDL_RenderCopy(sdlRenderer, sdlBorderTexture, &borderSource, &borderViewport);
                    }
#else
                    SDL_RenderCopy(sdlRenderer, sdlTexture, NULL, NULL);
#endif
#ifdef __ANDROID__
                    SDL_RenderPresent(sdlRenderer);
#endif
                    SDL_AtomicSet(&isFrameAvailable, 0);

                    REG_DISPSTAT |= INTR_FLAG_VBLANK;

                    RunDMAs(DMA_HBLANK);

#ifdef __ANDROID__
                    if (REG_IE & INTR_FLAG_VBLANK)
#else
                    if (REG_DISPSTAT & DISPSTAT_VBLANK_INTR)
#endif
                        gIntrTable[4]();
                    REG_DISPSTAT &= ~INTR_FLAG_VBLANK;

                    SDL_SemPost(vBlankSemaphore);
                }
                else
                {
                    // Nothing to draw this tick. Give the game thread the CPU rather than
                    // spinning on it; without this a stalled game thread pins a core.
                    break;
                }
            }
        }

#ifndef __ANDROID__
        SDL_RenderPresent(sdlRenderer);
#endif
    }

    SDL_Log("main loop exited, closing save file");
    //StoreSaveFile();
    CloseSaveFile();

#if defined(NATIVE_LINUX) || defined(_WIN32)
    for (int i = 0; i < sBorderBackgroundCount; i++)
        SDL_DestroyTexture(sdlBackgroundTextures[i]);
    SDL_DestroyTexture(sdlBorderTexture);
#endif
#if defined(NATIVE_LINUX) && !defined(NO_SDL_IMAGE)
#ifndef NO_SDL_IMAGE
    IMG_Quit();
#endif
#endif
    SDL_DestroyWindow(sdlWindow);
    SDL_Quit();

    // The AgbMain worker thread is never joined; it parks indefinitely in
    // VBlankIntrWait waiting on vBlankSemaphore, which nothing will post again.
    // Returning from main here runs the CRT exit path while that thread still holds
    // SDL/CRT state, which deadlocks and leaves a running, windowless process behind.
    // The save file is flushed and closed above, so terminating outright is safe.
    _Exit(0);
}

static void ReadSaveFile(const char *path)
{
    // Check whether the saveFile exists, and create it if not
    sSaveFile = fopen(path, "r+b");
    if (sSaveFile == NULL)
    {
        sSaveFile = fopen(path, "w+b");
    }

    if (sSaveFile == NULL)
    {
        memset(FLASH_BASE, 0xFF, sizeof(FLASH_BASE));
        SDL_Log("Unable to open save file: %s", path);
        return;
    }

    fseek(sSaveFile, 0, SEEK_END);
    int fileSize = ftell(sSaveFile);
    fseek(sSaveFile, 0, SEEK_SET);

    // Only read as many bytes as fit inside the buffer
    // or as many bytes as are in the file
    int bytesToRead = (fileSize < sizeof(FLASH_BASE)) ? fileSize : sizeof(FLASH_BASE);

    int bytesRead = fread(FLASH_BASE, 1, bytesToRead, sSaveFile);

    // Fill the buffer if the savefile was just created or smaller than the buffer itself
    for (int i = bytesRead; i < sizeof(FLASH_BASE); i++)
    {
        FLASH_BASE[i] = 0xFF;
    }
}

static void ReadConfigFile(void)
{
    FILE *configFile = fopen(sConfigPath, "r");
    char line[64];
    unsigned int value;

    if (configFile == NULL)
        return;
    while (fgets(line, sizeof(line), configFile) != NULL)
    {
        if (sscanf(line, "borderBackground=%u", &value) == 1 && value < 16)
        {
            sBorderBackground = value;
            sHasBorderBackgroundConfig = true;
        }
        else if (sscanf(line, "backgroundOrder=%u", &value) == 1)
            sBackgroundOrderVersion = value;
        else if (sscanf(line, "fullscreen=%u", &value) == 1)
            sPlatformSettings[PLATFORM_SETTING_FULLSCREEN] = value != 0;
        else if (sscanf(line, "windowScale=%u", &value) == 1 && value >= 2 && value <= 5)
            sPlatformSettings[PLATFORM_SETTING_WINDOW_SCALE] = value;
        else if (sscanf(line, "integerScale=%u", &value) == 1)
            sPlatformSettings[PLATFORM_SETTING_INTEGER_SCALE] = value != 0;
        else if (sscanf(line, "vsync=%u", &value) == 1)
            sPlatformSettings[PLATFORM_SETTING_VSYNC] = value != 0;
        else if (sscanf(line, "border=%u", &value) == 1)
            sPlatformSettings[PLATFORM_SETTING_BORDER] = value != 0;
        else if (sscanf(line, "volume=%u", &value) == 1 && value <= 10)
            sPlatformSettings[PLATFORM_SETTING_VOLUME] = value;
        else if (sscanf(line, "server=%127s", sServerHost) == 1)
            ; // handled by the scanf itself
        else if (sscanf(line, "serverPort=%u", &value) == 1 && value > 0 && value < 65536)
            sServerPort = value;
        else if (sscanf(line, "sidecarPort=%u", &value) == 1 && value > 0 && value < 65536)
            sSidecarPort = value;
    }
    fclose(configFile);
}

static void StoreConfigFile(void)
{
    FILE *configFile = fopen(sConfigPath, "w");

    if (configFile == NULL)
        return;
    fprintf(configFile, "borderBackground=%u\n", sBorderBackground);
    fprintf(configFile, "backgroundOrder=2\n");
    fprintf(configFile, "fullscreen=%u\n", sPlatformSettings[PLATFORM_SETTING_FULLSCREEN]);
    fprintf(configFile, "windowScale=%u\n", sPlatformSettings[PLATFORM_SETTING_WINDOW_SCALE]);
    fprintf(configFile, "integerScale=%u\n", sPlatformSettings[PLATFORM_SETTING_INTEGER_SCALE]);
    fprintf(configFile, "vsync=%u\n", sPlatformSettings[PLATFORM_SETTING_VSYNC]);
    fprintf(configFile, "border=%u\n", sPlatformSettings[PLATFORM_SETTING_BORDER]);
    fprintf(configFile, "volume=%u\n", sPlatformSettings[PLATFORM_SETTING_VOLUME]);
    // Written back so changing a display setting does not wipe the player's server choice.
    fprintf(configFile, "server=%s\n", sServerHost);
    fprintf(configFile, "serverPort=%u\n", sServerPort);
    fprintf(configFile, "sidecarPort=%u\n", sSidecarPort);
    fclose(configFile);
}

static void ApplyPlatformSettings(void)
{
    SDL_RenderSetVSync(sdlRenderer, sPlatformSettings[PLATFORM_SETTING_VSYNC]);
#if defined(NATIVE_LINUX) || defined(_WIN32)
    SDL_SetWindowFullscreen(sdlWindow, sPlatformSettings[PLATFORM_SETTING_FULLSCREEN]
                                      ? SDL_WINDOW_FULLSCREEN_DESKTOP : 0);
    if (!sPlatformSettings[PLATFORM_SETTING_FULLSCREEN])
    {
        int scale = sPlatformSettings[PLATFORM_SETTING_WINDOW_SCALE];
        SDL_SetWindowSize(sdlWindow, 320 * scale, 180 * scale);
        SDL_SetWindowPosition(sdlWindow, SDL_WINDOWPOS_CENTERED, SDL_WINDOWPOS_CENTERED);
    }
#endif
}

static void StoreSaveFile()
{
    if (sSaveFile != NULL)
    {
        fseek(sSaveFile, 0, SEEK_SET);
        fwrite(FLASH_BASE, 1, sizeof(FLASH_BASE), sSaveFile);
    }
}

void Platform_StoreSaveFile(void)
{
    // Signed in, the character lives on the server and nowhere else.
    //
    // Writing a copy here would not make it safer -- the server's copy overwrites this one
    // at every sign-in, so a file edited between sessions is discarded rather than obeyed --
    // but it would leave a stale character on the disk that looks authoritative and is not.
    // The save still exists in FLASH_BASE and still goes to the server; it simply stops
    // being written to a file that nothing should ever read again.
    //
    // Offline play is the exception and keeps the old behaviour, because there is no server
    // to hold anything and a file is all there is.
    if (Net_GetAuthState() == NET_AUTH_ONLINE)
        return;

    StoreSaveFile();
}

void Platform_ReadFlash(u16 sectorNum, u32 offset, u8 *dest, u32 size)
{
    // Serve reads from the RAM mirror rather than the file on disk.
    //
    // Writes land in FLASH_BASE and only reach the file when something calls
    // Platform_StoreSaveFile, and several paths never do -- the incremental link saves
    // behind trades, record mixing and Berry Crush all write sectors and leave them
    // unflushed. Reading the file could therefore hand back bytes older than what the game
    // had already written, which ReloadSave turns into progress that silently reverts.
    //
    // It also stops reopening the save file for every sector read during a load, and it is
    // the seam a server-held save hydrates into: fill FLASH_BASE at sign-in and the whole
    // load path reads from the server's copy without knowing anything changed.
    u32 start = (sectorNum << gFlash->sector.shift) + offset;

    DBGPRINTF("ReadFlash(sectorNum=0x%04X,offset=0x%08X,size=0x%02X)\n",sectorNum,offset,size);

    if (dest == NULL || size == 0)
        return;

    if (start >= sizeof(FLASH_BASE) || size > sizeof(FLASH_BASE) - start)
    {
        SDL_Log("flash read out of range: sector %u offset %u size %u",
                (unsigned)sectorNum, (unsigned)offset, (unsigned)size);
        return;
    }

    memcpy(dest, FLASH_BASE + start, size);
}

void Platform_QueueAudio(float *audioBuffer, s32 samplesPerFrame)
{
    if (sdlAudioDevice != 0)
    {
        int floatCount = samplesPerFrame / sizeof(float);
        float adjustedAudio[floatCount];
        float volume = sPlatformSettings[PLATFORM_SETTING_VOLUME] / 10.0f;
        for (int i = 0; i < floatCount; i++)
            adjustedAudio[i] = audioBuffer[i] * volume;
        if (SDL_QueueAudio(sdlAudioDevice, adjustedAudio, samplesPerFrame) < 0)
            SDL_Log("Failed to queue audio: %s", SDL_GetError());
    }
}

u8 Platform_GetBorderBackgroundCount(void)
{
    return sBorderBackgroundCount + 1;
}

u8 Platform_GetBorderBackground(void)
{
    if (sHasBorderBackgroundConfig)
        return sBorderBackground;
    if (gSaveBlock2Ptr != NULL)
    {
        u8 legacySelection = gSaveBlock2Ptr->optionsBorderBackground;
        if (legacySelection == 1)
            return sBorderBackgroundCount;
        if (legacySelection >= 2)
            return legacySelection - 1;
    }
    return 0;
}

// Display changes are requested here and applied by the thread that owns the window.
//
// SDL window and renderer calls have to come from the thread that created them. Calling them
// from the game thread blocks until the owning thread pumps its message queue, which is a
// deadlock waiting to happen and did happen: fullscreen off froze the game outright. The frame
// loop no longer spins in that situation, but the ordering rule is the actual fix -- the
// accumulator change stops it hanging, this stops it blocking at all.
static SDL_atomic_t sDisplayChangePending;
static SDL_atomic_t sVsyncChangePending;

// Called from ProcessEvents, on the thread that made the window.
static void ApplyPendingDisplayChange(void)
{
    if (SDL_AtomicSet(&sVsyncChangePending, 0))
        SDL_RenderSetVSync(sdlRenderer, sPlatformSettings[PLATFORM_SETTING_VSYNC]);

    if (!SDL_AtomicSet(&sDisplayChangePending, 0))
        return;

#if defined(NATIVE_LINUX) || defined(_WIN32)
    {
        u8 fullscreen = sPlatformSettings[PLATFORM_SETTING_FULLSCREEN];
        int scale = sPlatformSettings[PLATFORM_SETTING_WINDOW_SCALE];

        SDL_SetWindowFullscreen(sdlWindow, fullscreen ? SDL_WINDOW_FULLSCREEN_DESKTOP : 0);
        if (!fullscreen)
        {
            if (scale < 1)
                scale = 1;
            SDL_SetWindowSize(sdlWindow, 320 * scale, 180 * scale);
            SDL_SetWindowPosition(sdlWindow, SDL_WINDOWPOS_CENTERED, SDL_WINDOWPOS_CENTERED);
        }
    }
#endif
}

void Platform_SetBorderBackground(u8 selection)
{
    sBorderBackground = selection;
    sHasBorderBackgroundConfig = true;
    StoreConfigFile();
}

u8 Platform_GetSetting(enum PlatformSetting setting)
{
    return sPlatformSettings[setting];
}

void Platform_SetSetting(enum PlatformSetting setting, u8 value)
{
    sPlatformSettings[setting] = value;

    // Record what is wanted; the owning thread does it. See ApplyPendingDisplayChange.
    if (setting == PLATFORM_SETTING_VSYNC)
        SDL_AtomicSet(&sVsyncChangePending, 1);
    else if (setting == PLATFORM_SETTING_FULLSCREEN
             || setting == PLATFORM_SETTING_WINDOW_SCALE)
        SDL_AtomicSet(&sDisplayChangePending, 1);

    StoreConfigFile();
}

#ifdef __ANDROID__
JNIEXPORT jint JNICALL Java_com_pokeemerald_experimental_GbaControlsView_getBorderBackground(JNIEnv *env, jclass clazz)
{
    return Platform_GetBorderBackground();
}

JNIEXPORT jint JNICALL Java_com_pokeemerald_experimental_GbaControlsView_getPlatformSetting(JNIEnv *env, jclass clazz, jint setting)
{
    if (setting < 0 || setting >= PLATFORM_SETTING_COUNT)
        return 0;
    return Platform_GetSetting(setting);
}
#endif


static void CloseSaveFile()
{
    if (sSaveFile != NULL)
    {
        fclose(sSaveFile);
    }
}

// Key mappings
#define KEY_A_BUTTON      SDLK_z
#define KEY_B_BUTTON      SDLK_x
#define KEY_START_BUTTON  SDLK_RETURN
#define KEY_SELECT_BUTTON SDLK_BACKSLASH
#define KEY_L_BUTTON      SDLK_a
#define KEY_R_BUTTON      SDLK_s
#define KEY_DPAD_UP       SDLK_UP
#define KEY_DPAD_DOWN     SDLK_DOWN
#define KEY_DPAD_LEFT     SDLK_LEFT
#define KEY_DPAD_RIGHT    SDLK_RIGHT

#define HANDLE_KEYUP(key) \
case KEY_##key:  keyboardKeys &= ~key; break;

#define HANDLE_KEYDOWN(key) \
case KEY_##key:  keyboardKeys |= key; break;

static u16 keyboardKeys;

#ifdef __ANDROID__
#define MAX_TOUCH_FINGERS 10

struct TouchFinger
{
    SDL_FingerID id;
    float x;
    float y;
    bool active;
};

static struct TouchFinger touchFingers[MAX_TOUCH_FINGERS];
static u16 touchKeys;
static u16 controllerKeys;
static u16 controllerAxisKeys;
static Sint16 controllerAxisX;
static Sint16 controllerAxisY;

static bool IsInsideRect(int x, int y, SDL_Rect rect)
{
    SDL_Point point = {x, y};
    return SDL_PointInRect(&point, &rect);
}

static int MinInt(int a, int b)
{
    return a < b ? a : b;
}

static int GetControlSideWidth(int windowWidth, int windowHeight)
{
    int sideWidth = (windowWidth - windowHeight * 3 / 2) / 2;
    int minimumWidth = windowWidth * 14 / 100;
    return sideWidth > minimumWidth ? sideWidth : minimumWidth;
}

static void UpdateTouchKeys(void)
{
    int windowWidth;
    int windowHeight;
    SDL_GetWindowSize(sdlWindow, &windowWidth, &windowHeight);
    int sideWidth = GetControlSideWidth(windowWidth, windowHeight);
    int buttonSize = MinInt(sideWidth * 2 / 5, windowHeight / 6);
    int dpadUnit = MinInt(sideWidth / 3, windowHeight / 8);
    int dpadX = sideWidth * 2 / 3;
    int dpadY = windowHeight * 7 / 10;
    SDL_Rect dpadUp = {dpadX - dpadUnit / 2, dpadY - dpadUnit * 3 / 2,
                       dpadUnit, dpadUnit};
    SDL_Rect dpadDown = {dpadX - dpadUnit / 2, dpadY + dpadUnit / 2,
                         dpadUnit, dpadUnit};
    SDL_Rect dpadLeft = {dpadX - dpadUnit * 3 / 2, dpadY - dpadUnit / 2,
                         dpadUnit, dpadUnit};
    SDL_Rect dpadRight = {dpadX + dpadUnit / 2, dpadY - dpadUnit / 2,
                          dpadUnit, dpadUnit};
    SDL_Rect aButton = {windowWidth - sideWidth / 4 - buttonSize,
                        windowHeight * 58 / 100, buttonSize, buttonSize};
    SDL_Rect bButton = {windowWidth - sideWidth + sideWidth / 4,
                        windowHeight * 76 / 100, buttonSize, buttonSize};
    SDL_Rect selectButton = {sideWidth / 4, windowHeight / 4,
                             sideWidth / 2, windowHeight / 10};
    SDL_Rect startButton = {windowWidth - sideWidth * 3 / 4, windowHeight / 4,
                            sideWidth / 2, windowHeight / 10};
    SDL_Rect lButton = {sideWidth / 4, windowHeight / 20,
                        sideWidth / 2, windowHeight / 10};
    SDL_Rect rButton = {windowWidth - sideWidth * 3 / 4, windowHeight / 20,
                        sideWidth / 2, windowHeight / 10};

    touchKeys = 0;

    for (int i = 0; i < MAX_TOUCH_FINGERS; i++)
    {
        if (!touchFingers[i].active)
            continue;

        int x = touchFingers[i].x * windowWidth;
        int y = touchFingers[i].y * windowHeight;

        if (IsInsideRect(x, y, dpadUp)) touchKeys |= DPAD_UP;
        if (IsInsideRect(x, y, dpadDown)) touchKeys |= DPAD_DOWN;
        if (IsInsideRect(x, y, dpadLeft)) touchKeys |= DPAD_LEFT;
        if (IsInsideRect(x, y, dpadRight)) touchKeys |= DPAD_RIGHT;

        if (IsInsideRect(x, y, aButton)) touchKeys |= A_BUTTON;
        if (IsInsideRect(x, y, bButton)) touchKeys |= B_BUTTON;
        if (IsInsideRect(x, y, startButton)) touchKeys |= START_BUTTON;
        if (IsInsideRect(x, y, selectButton)) touchKeys |= SELECT_BUTTON;
        if (IsInsideRect(x, y, lButton)) touchKeys |= L_BUTTON;
        if (IsInsideRect(x, y, rButton)) touchKeys |= R_BUTTON;
    }
}

static void HandleTouchEvent(const SDL_TouchFingerEvent *event)
{
    int slot = -1;
    for (int i = 0; i < MAX_TOUCH_FINGERS; i++)
    {
        if (touchFingers[i].active && touchFingers[i].id == event->fingerId)
        {
            slot = i;
            break;
        }
        if (slot < 0 && !touchFingers[i].active)
            slot = i;
    }

    if (slot < 0)
        return;

    if (event->type == SDL_FINGERUP)
    {
        touchFingers[slot].active = false;
    }
    else
    {
        touchFingers[slot].id = event->fingerId;
        touchFingers[slot].x = event->x;
        touchFingers[slot].y = event->y;
        touchFingers[slot].active = true;
    }

    UpdateTouchKeys();
}

static const Uint8 *GetGlyph(char character)
{
    static const Uint8 glyphA[7] = {14, 17, 17, 31, 17, 17, 17};
    static const Uint8 glyphB[7] = {30, 17, 17, 30, 17, 17, 30};
    static const Uint8 glyphC[7] = {15, 16, 16, 16, 16, 16, 15};
    static const Uint8 glyphE[7] = {31, 16, 16, 30, 16, 16, 31};
    static const Uint8 glyphL[7] = {16, 16, 16, 16, 16, 16, 31};
    static const Uint8 glyphR[7] = {30, 17, 17, 30, 20, 18, 17};
    static const Uint8 glyphS[7] = {15, 16, 16, 14, 1, 1, 30};
    static const Uint8 glyphT[7] = {31, 4, 4, 4, 4, 4, 4};

    switch (character)
    {
    case 'A': return glyphA;
    case 'B': return glyphB;
    case 'C': return glyphC;
    case 'E': return glyphE;
    case 'L': return glyphL;
    case 'R': return glyphR;
    case 'S': return glyphS;
    case 'T': return glyphT;
    default:  return NULL;
    }
}

static void DrawControlLabel(SDL_Rect rect, const char *label)
{
    int length = SDL_strlen(label);
    int scale = MinInt(rect.h / 9, rect.w / (length * 6));
    if (scale < 1)
        scale = 1;
    int startX = rect.x + (rect.w - (length * 6 - 1) * scale) / 2;
    int startY = rect.y + (rect.h - 7 * scale) / 2;

    SDL_SetRenderDrawColor(sdlRenderer, 255, 255, 255, 230);
    for (int character = 0; character < length; character++)
    {
        const Uint8 *glyph = GetGlyph(label[character]);
        if (glyph == NULL)
            continue;
        for (int row = 0; row < 7; row++)
        {
            for (int column = 0; column < 5; column++)
            {
                if (glyph[row] & (1 << (4 - column)))
                {
                    SDL_Rect pixel = {startX + (character * 6 + column) * scale,
                                      startY + row * scale, scale, scale};
                    SDL_RenderFillRect(sdlRenderer, &pixel);
                }
            }
        }
    }
}

static void DrawControlRect(SDL_Rect rect, bool pressed, const char *label)
{
    SDL_SetRenderDrawColor(sdlRenderer, 255, 255, 255, pressed ? 150 : 65);
    SDL_RenderFillRect(sdlRenderer, &rect);
    SDL_SetRenderDrawColor(sdlRenderer, 255, 255, 255, pressed ? 230 : 130);
    SDL_RenderDrawRect(sdlRenderer, &rect);
    if (label != NULL)
        DrawControlLabel(rect, label);
}

static void DrawTouchControls(void)
{
    int windowWidth;
    int windowHeight;
    SDL_GetWindowSize(sdlWindow, &windowWidth, &windowHeight);
    int sideWidth = GetControlSideWidth(windowWidth, windowHeight);
    int buttonSize = MinInt(sideWidth * 2 / 5, windowHeight / 6);
    int dpadUnit = MinInt(sideWidth / 3, windowHeight / 8);
    int dpadX = sideWidth * 2 / 3;
    int dpadY = windowHeight * 7 / 10;

    SDL_RenderSetLogicalSize(sdlRenderer, 0, 0);
    SDL_SetRenderDrawBlendMode(sdlRenderer, SDL_BLENDMODE_BLEND);

    DrawControlRect((SDL_Rect){dpadX - dpadUnit / 2, dpadY - dpadUnit * 3 / 2,
                               dpadUnit, dpadUnit}, touchKeys & DPAD_UP, NULL);
    DrawControlRect((SDL_Rect){dpadX - dpadUnit / 2, dpadY + dpadUnit / 2,
                               dpadUnit, dpadUnit}, touchKeys & DPAD_DOWN, NULL);
    DrawControlRect((SDL_Rect){dpadX - dpadUnit * 3 / 2, dpadY - dpadUnit / 2,
                               dpadUnit, dpadUnit}, touchKeys & DPAD_LEFT, NULL);
    DrawControlRect((SDL_Rect){dpadX + dpadUnit / 2, dpadY - dpadUnit / 2,
                               dpadUnit, dpadUnit}, touchKeys & DPAD_RIGHT, NULL);
    DrawControlRect((SDL_Rect){windowWidth - sideWidth / 4 - buttonSize,
                               windowHeight * 58 / 100, buttonSize, buttonSize}, touchKeys & A_BUTTON, "A");
    DrawControlRect((SDL_Rect){windowWidth - sideWidth + sideWidth / 4,
                               windowHeight * 76 / 100, buttonSize, buttonSize}, touchKeys & B_BUTTON, "B");
    DrawControlRect((SDL_Rect){windowWidth - sideWidth * 3 / 4, windowHeight / 4,
                               sideWidth / 2, windowHeight / 10}, touchKeys & START_BUTTON, "START");
    DrawControlRect((SDL_Rect){sideWidth / 4, windowHeight / 4,
                               sideWidth / 2, windowHeight / 10}, touchKeys & SELECT_BUTTON, "SELECT");
    DrawControlRect((SDL_Rect){sideWidth / 4, windowHeight / 20,
                               sideWidth / 2, windowHeight / 10}, touchKeys & L_BUTTON, "L");
    DrawControlRect((SDL_Rect){windowWidth - sideWidth * 3 / 4, windowHeight / 20,
                               sideWidth / 2, windowHeight / 10}, touchKeys & R_BUTTON, "R");

    SDL_SetRenderDrawColor(sdlRenderer, 0, 0, 0, 255);
    SDL_SetRenderDrawBlendMode(sdlRenderer, SDL_BLENDMODE_NONE);
    SDL_RenderSetLogicalSize(sdlRenderer, DISPLAY_WIDTH, DISPLAY_HEIGHT);
    SDL_RenderSetIntegerScale(sdlRenderer, SDL_TRUE);
}

static u16 ControllerButtonMask(Uint8 button)
{
    switch (button)
    {
    case SDL_CONTROLLER_BUTTON_A:             return A_BUTTON;
    case SDL_CONTROLLER_BUTTON_B:             return B_BUTTON;
    case SDL_CONTROLLER_BUTTON_BACK:          return SELECT_BUTTON;
    case SDL_CONTROLLER_BUTTON_START:         return START_BUTTON;
    case SDL_CONTROLLER_BUTTON_LEFTSHOULDER:  return L_BUTTON;
    case SDL_CONTROLLER_BUTTON_RIGHTSHOULDER: return R_BUTTON;
    case SDL_CONTROLLER_BUTTON_DPAD_UP:       return DPAD_UP;
    case SDL_CONTROLLER_BUTTON_DPAD_DOWN:     return DPAD_DOWN;
    case SDL_CONTROLLER_BUTTON_DPAD_LEFT:     return DPAD_LEFT;
    case SDL_CONTROLLER_BUTTON_DPAD_RIGHT:    return DPAD_RIGHT;
    default:                                  return 0;
    }
}
#endif

// Scripted input for automated testing.
//
// A headless run has no keyboard, so nothing drives the game past the title screen and
// any code path behind a menu is unreachable. POKEPLANET_AUTOKEYS makes the build
// self-driving: a comma-separated key list, one press every POKEPLANET_AUTOKEY_FRAMES
// frames (default 45). That is what lets the whole client be exercised under gdb in a
// terminal instead of by hand on a desktop.
//
//   POKEPLANET_AUTOKEYS=enter,enter,z,z,down:20,z ./pokeemerald
//
// Recognised: a b start select l r up down left right, and the key names z x enter.
//
// A token may carry a hold length in game frames, as in "down:20". The default is a short
// tap, which is all a menu needs. Walking is different: tapping a direction only turns the
// player, and actually stepping a tile needs the direction held for the length of the step,
// so anything that has to move must ask for it.
//
// This runs on the game thread, from Platform_GetKeyInput, so "frame" here means a game
// frame. It used to run on the SDL event thread inside ProcessEvents, which iterates at its
// own rate entirely independently of the game thread; a press was set and cleared within one
// event-loop pass and the game frequently never sampled it in between. That is why scripted
// runs never got past the sign-in gate and why the harness had never actually proven
// anything end to end.
#define AUTOKEY_DEFAULT_HOLD 2

static u16 PumpScriptedInput(void)
{
    static const char *sScript = NULL;
    static bool8 sChecked = FALSE;
    static u32 sFrame = 0;
    static u32 sInterval = 45;
    const char *cursor;
    const char *colon;
    const char *comma;
    u32 token;
    u32 phase;
    u32 hold;
    u32 i;
    u16 step;

    if (!sChecked)
    {
        const char *frames = SDL_getenv("POKEPLANET_AUTOKEY_FRAMES");
        sScript = SDL_getenv("POKEPLANET_AUTOKEYS");
        if (frames != NULL && SDL_atoi(frames) > 0)
            sInterval = SDL_atoi(frames);
        sChecked = TRUE;
        if (sScript != NULL)
            SDL_Log("autokeys: '%s' every %u frames", sScript, (unsigned)sInterval);
    }
    if (sScript == NULL)
        return 0;

    // Which token this frame belongs to, and how far into its window we are. Deriving both
    // from the frame counter keeps the whole thing stateless, so a token is held for a
    // definite number of frames rather than depending on when it was last advanced.
    token = sFrame / sInterval;
    phase = sFrame % sInterval;
    sFrame++;

    cursor = sScript;
    for (i = 0; i < token && cursor != NULL; i++)
    {
        cursor = SDL_strchr(cursor, ',');
        if (cursor != NULL)
            cursor++;
    }
    if (cursor == NULL || *cursor == '\0')
        return 0; // script exhausted; leave the game running for inspection

    step = 0;
    if      (SDL_strncasecmp(cursor, "a", 1) == 0 || SDL_strncasecmp(cursor, "z", 1) == 0) step = A_BUTTON;
    else if (SDL_strncasecmp(cursor, "b", 1) == 0 || SDL_strncasecmp(cursor, "x", 1) == 0) step = B_BUTTON;
    else if (SDL_strncasecmp(cursor, "start", 5) == 0 || SDL_strncasecmp(cursor, "enter", 5) == 0) step = START_BUTTON;
    else if (SDL_strncasecmp(cursor, "select", 6) == 0) step = SELECT_BUTTON;
    else if (SDL_strncasecmp(cursor, "up", 2) == 0)    step = DPAD_UP;
    else if (SDL_strncasecmp(cursor, "down", 4) == 0)  step = DPAD_DOWN;
    else if (SDL_strncasecmp(cursor, "left", 4) == 0)  step = DPAD_LEFT;
    else if (SDL_strncasecmp(cursor, "right", 5) == 0) step = DPAD_RIGHT;
    else if (SDL_strncasecmp(cursor, "l", 1) == 0)     step = L_BUTTON;
    else if (SDL_strncasecmp(cursor, "r", 1) == 0)     step = R_BUTTON;

    hold = AUTOKEY_DEFAULT_HOLD;
    colon = SDL_strchr(cursor, ':');
    comma = SDL_strchr(cursor, ',');
    if (colon != NULL && (comma == NULL || colon < comma))
    {
        int parsed = SDL_atoi(colon + 1);
        if (parsed > 0)
            hold = (u32)parsed;
    }
    // Holding past the end of the window would run into the next token's press.
    if (hold > sInterval)
        hold = sInterval;

    return phase < hold ? step : 0;
}

// Typing, for chat.
//
// The naming screen is the only text entry the original game has, and it tops out at about
// ten characters chosen from a grid with the d-pad. That is fine for naming a Pokemon and
// useless for talking to someone. This is a PC port and there is a real keyboard attached,
// so chat uses it.
//
// The buffer is written here, on the SDL event thread, and read by game code on its own
// thread, so it is guarded like every other shared piece of state. While typing is active
// the key mapping is suppressed, otherwise typing "a" would also press the A button.
#define TEXT_INPUT_MAX 120

static SDL_mutex *sTextInputLock;
static char sTextInput[TEXT_INPUT_MAX];
static u8 sTextInputLength;
static bool8 sTextInputActive;

// Set when typing ends, cleared once nothing is held. See Platform_GetKeyInput.
static bool8 sSwallowKeysUntilRelease;
static u8 sTextInputResult; // 0 none, 1 submitted, 2 cancelled

void Platform_BeginTextInput(void)
{
    if (sTextInputLock == NULL)
        sTextInputLock = SDL_CreateMutex();
    SDL_LockMutex(sTextInputLock);
    sTextInput[0] = '\0';
    sTextInputLength = 0;
    sTextInputResult = 0;
    sTextInputActive = TRUE;
    SDL_UnlockMutex(sTextInputLock);
    SDL_StartTextInput();
}

void Platform_EndTextInput(void)
{
    SDL_StopTextInput();
    if (sTextInputLock == NULL)
        return;
    SDL_LockMutex(sTextInputLock);
    sTextInputActive = FALSE;
    SDL_UnlockMutex(sTextInputLock);
    // Whatever was held while typing must not read as a fresh press now. See below.
    sSwallowKeysUntilRelease = TRUE;
}

// Copies what has been typed so far. Returns 0 while still typing, 1 once the player has
// pressed Enter, 2 if they backed out.
u8 Platform_PollTextInput(char *out, u8 outSize)
{
    u8 result;

    if (sTextInputLock == NULL || out == NULL || outSize == 0)
        return 0;

    SDL_LockMutex(sTextInputLock);
    {
        u8 i;

        for (i = 0; i < outSize - 1 && sTextInput[i] != '\0'; i++)
            out[i] = sTextInput[i];
        out[i] = '\0';
    }
    result = sTextInputResult;
    sTextInputResult = 0;
    SDL_UnlockMutex(sTextInputLock);
    return result;
}

// TRUE while the player is typing, so the rest of the game ignores the keyboard.
bool8 Platform_IsTextInputActive(void)
{
    return sTextInputActive;
}

// Returns TRUE if the event was consumed by the text field.
static bool8 HandleTextInputEvent(const SDL_Event *event)
{
    if (!sTextInputActive)
        return FALSE;

    SDL_LockMutex(sTextInputLock);
    if (event->type == SDL_TEXTINPUT)
    {
        const char *typed = event->text.text;
        u8 i;

        // Only what the game's font can draw; everything else is dropped rather than
        // silently becoming a space in the middle of a sentence.
        for (i = 0; typed[i] != '\0' && sTextInputLength < TEXT_INPUT_MAX - 1; i++)
        {
            if ((unsigned char)typed[i] >= ' ' && (unsigned char)typed[i] < 0x7F)
                sTextInput[sTextInputLength++] = typed[i];
        }
        sTextInput[sTextInputLength] = '\0';
    }
    else if (event->type == SDL_KEYDOWN)
    {
        switch (event->key.keysym.sym)
        {
        case SDLK_BACKSPACE:
            if (sTextInputLength > 0)
                sTextInput[--sTextInputLength] = '\0';
            break;
        case SDLK_RETURN:
        case SDLK_KP_ENTER:
            sTextInputResult = 1;
            break;
        case SDLK_ESCAPE:
            sTextInputResult = 2;
            break;
        default:
            break;
        }
    }
    SDL_UnlockMutex(sTextInputLock);

    // Swallow key events while typing so they never reach the button mapping.
    return event->type == SDL_TEXTINPUT || event->type == SDL_KEYDOWN
        || event->type == SDL_KEYUP;
}

void ProcessEvents(void)
{
    SDL_Event event;

    while (SDL_PollEvent(&event))
    {
        if (HandleTextInputEvent(&event))
            continue;

        switch (event.type)
        {
        case SDL_QUIT:
            SDL_Log("SDL_QUIT received, shutting down");
            isRunning = false;
            break;
        case SDL_WINDOWEVENT:
            if (event.window.event == SDL_WINDOWEVENT_CLOSE)
                SDL_Log("SDL_WINDOWEVENT_CLOSE for window %u", event.window.windowID);
            break;
#ifdef __ANDROID__
        case SDL_CONTROLLERDEVICEADDED:
            if (androidController == NULL && SDL_IsGameController(event.cdevice.which))
                androidController = SDL_GameControllerOpen(event.cdevice.which);
            break;
        case SDL_CONTROLLERDEVICEREMOVED:
            if (androidController != NULL
             && SDL_JoystickInstanceID(SDL_GameControllerGetJoystick(androidController)) == event.cdevice.which)
            {
                SDL_GameControllerClose(androidController);
                androidController = NULL;
                controllerKeys = 0;
                controllerAxisKeys = 0;
                controllerAxisX = 0;
                controllerAxisY = 0;
            }
            break;
        case SDL_CONTROLLERBUTTONDOWN:
            controllerKeys |= ControllerButtonMask(event.cbutton.button);
            break;
        case SDL_CONTROLLERBUTTONUP:
            controllerKeys &= ~ControllerButtonMask(event.cbutton.button);
            break;
        case SDL_CONTROLLERAXISMOTION:
            if (event.caxis.axis == SDL_CONTROLLER_AXIS_LEFTX)
                controllerAxisX = event.caxis.value;
            else if (event.caxis.axis == SDL_CONTROLLER_AXIS_LEFTY)
                controllerAxisY = event.caxis.value;

            controllerAxisKeys = 0;
            if (controllerAxisX < -16000) controllerAxisKeys |= DPAD_LEFT;
            if (controllerAxisX >  16000) controllerAxisKeys |= DPAD_RIGHT;
            if (controllerAxisY < -16000) controllerAxisKeys |= DPAD_UP;
            if (controllerAxisY >  16000) controllerAxisKeys |= DPAD_DOWN;
            break;
#endif
        case SDL_KEYUP:
            switch (event.key.keysym.sym)
            {
            HANDLE_KEYUP(A_BUTTON)
            HANDLE_KEYUP(B_BUTTON)
            HANDLE_KEYUP(START_BUTTON)
            HANDLE_KEYUP(SELECT_BUTTON)
            HANDLE_KEYUP(L_BUTTON)
            HANDLE_KEYUP(R_BUTTON)
            HANDLE_KEYUP(DPAD_UP)
            HANDLE_KEYUP(DPAD_DOWN)
            HANDLE_KEYUP(DPAD_LEFT)
            HANDLE_KEYUP(DPAD_RIGHT)
            case SDLK_SPACE:
                if (speedUp)
                {
                    speedUp = false;
                    timeScale = 1.0;
                    SDL_ClearQueuedAudio(sdlAudioDevice);
                    SDL_PauseAudioDevice(sdlAudioDevice, 0);
                }
                break;
            }
            break;
        case SDL_KEYDOWN:
            switch (event.key.keysym.sym)
            {
            HANDLE_KEYDOWN(A_BUTTON)
            HANDLE_KEYDOWN(B_BUTTON)
            HANDLE_KEYDOWN(START_BUTTON)
            HANDLE_KEYDOWN(SELECT_BUTTON)
            HANDLE_KEYDOWN(L_BUTTON)
            HANDLE_KEYDOWN(R_BUTTON)
            HANDLE_KEYDOWN(DPAD_UP)
            HANDLE_KEYDOWN(DPAD_DOWN)
            HANDLE_KEYDOWN(DPAD_LEFT)
            HANDLE_KEYDOWN(DPAD_RIGHT)
            case SDLK_r:
                if (event.key.keysym.mod & (KMOD_LCTRL | KMOD_RCTRL))
                {
                    DoSoftReset();
                }
                break;
            case SDLK_p:
                if (event.key.keysym.mod & (KMOD_LCTRL | KMOD_RCTRL))
                {
                    paused = !paused;
                }
                break;
            case SDLK_SPACE:
                if (!speedUp)
                {
                    speedUp = true;
                    timeScale = 5.0;
                    SDL_PauseAudioDevice(sdlAudioDevice, 1);
                }
                break;
            }
            break;
        }
    }
}

#ifdef _WIN32
#define STICK_THRESHOLD 0.5f
u16 GetXInputKeys()
{
    XINPUT_STATE state;
    ZeroMemory(&state, sizeof(XINPUT_STATE));

    DWORD dwResult = XInputGetState(0, &state);
    u16 xinputKeys = 0;

    if (dwResult == ERROR_SUCCESS)
    {
        /* A */      xinputKeys |= (state.Gamepad.wButtons & XINPUT_GAMEPAD_A) >> 12;
        /* B */      xinputKeys |= (state.Gamepad.wButtons & XINPUT_GAMEPAD_X) >> 13;
        /* Start */  xinputKeys |= (state.Gamepad.wButtons & XINPUT_GAMEPAD_START) >> 1;
        /* Select */ xinputKeys |= (state.Gamepad.wButtons & XINPUT_GAMEPAD_BACK) >> 3;
        /* L */      xinputKeys |= (state.Gamepad.wButtons & XINPUT_GAMEPAD_LEFT_SHOULDER) << 1;
        /* R */      xinputKeys |= (state.Gamepad.wButtons & XINPUT_GAMEPAD_RIGHT_SHOULDER) >> 1;
        /* Up */     xinputKeys |= (state.Gamepad.wButtons & XINPUT_GAMEPAD_DPAD_UP) << 6;
        /* Down */   xinputKeys |= (state.Gamepad.wButtons & XINPUT_GAMEPAD_DPAD_DOWN) << 6;
        /* Left */   xinputKeys |= (state.Gamepad.wButtons & XINPUT_GAMEPAD_DPAD_LEFT) << 3;
        /* Right */  xinputKeys |= (state.Gamepad.wButtons & XINPUT_GAMEPAD_DPAD_RIGHT) << 1;


        /* Control Stick */
        float xAxis = (float)state.Gamepad.sThumbLX / (float)SHRT_MAX;
        float yAxis = (float)state.Gamepad.sThumbLY / (float)SHRT_MAX;

        if (xAxis < -STICK_THRESHOLD) xinputKeys |= DPAD_LEFT;
        if (xAxis >  STICK_THRESHOLD) xinputKeys |= DPAD_RIGHT;
        if (yAxis < -STICK_THRESHOLD) xinputKeys |= DPAD_DOWN;
        if (yAxis >  STICK_THRESHOLD) xinputKeys |= DPAD_UP;


        /* Speedup */
        // Note: 'speedup' variable is only (un)set on keyboard input
        double oldTimeScale = timeScale;
        timeScale = (state.Gamepad.bRightTrigger > 0x80 || speedUp) ? 5.0 : 1.0;

        if (oldTimeScale != timeScale)
        {
            if (timeScale > 1.0)
            {
                SDL_PauseAudioDevice(sdlAudioDevice, 1);
            }
            else
            {
                SDL_ClearQueuedAudio(sdlAudioDevice);
                SDL_PauseAudioDevice(sdlAudioDevice, 0);
            }
        }
    }

    return xinputKeys;
}
#endif // _WIN32

u16 Platform_GetKeyInput(void)
{
    // Called once per game frame from ReadKeys, which is what makes it the right place to
    // step the test script: the game cannot miss a press it is itself sampling.
    u16 scripted = PumpScriptedInput();

    // While the player is typing, the keyboard belongs to the text field. Without this,
    // composing "start again" would press START, SELECT and A along the way.
    if (sTextInputActive)
        return 0;

    // And for a moment afterwards, until everything is released.
    //
    // The game works out new presses by comparing this frame's keys against last frame's, and
    // last frame's were zero because typing was swallowing them. So every key still physically
    // down when the player finishes typing arrives as a brand new press: the key that opened
    // the chat box reopens it immediately, and a direction still held walks the player away.
    // That is the "it closes and instantly comes back, and I am stuck walking" report.
    //
    // Waiting for a clean release rather than swallowing a fixed number of frames, because how
    // long a person keeps a key down is not a number this can guess.
    {
        u16 physical;

#ifdef _WIN32
        physical = GetXInputKeys() | keyboardKeys;
#elif defined(__ANDROID__)
        physical = keyboardKeys | controllerKeys | controllerAxisKeys;
#else
        physical = keyboardKeys;
#endif

        if (sSwallowKeysUntilRelease)
        {
            if (physical != 0)
                return 0;
            sSwallowKeysUntilRelease = FALSE;
        }
    }

#ifdef _WIN32
    u16 gamepadKeys = GetXInputKeys();
    return gamepadKeys | keyboardKeys | scripted;
#elif defined(__ANDROID__)
    return keyboardKeys | controllerKeys | controllerAxisKeys | scripted;
#endif

    return keyboardKeys | scripted;
}

void VDraw(SDL_Texture *texture)
{
    static uint16_t gbaImage[DISPLAY_WIDTH * DISPLAY_HEIGHT];
    static uint32_t image[DISPLAY_WIDTH * DISPLAY_HEIGHT];

    memset(gbaImage, 0, sizeof(gbaImage));
    DrawFrame(gbaImage);
    for (int i = 0; i < DISPLAY_WIDTH * DISPLAY_HEIGHT; i++)
    {
        uint16_t color = gbaImage[i];
        uint32_t r = (color & 0x1F) * 255 / 31;
        uint32_t g = ((color >> 5) & 0x1F) * 255 / 31;
        uint32_t b = ((color >> 10) & 0x1F) * 255 / 31;
        image[i] = 0xFF000000 | (r << 16) | (g << 8) | b;
    }
    SDL_UpdateTexture(texture, NULL, image, DISPLAY_WIDTH * sizeof(Uint32));
    REG_VCOUNT = 161; // prep for being in VBlank period
}

int DoMain(void *data)
{
    AgbMain();
    return 0;
}

void VBlankIntrWait(void)
{
    SDL_AtomicSet(&isFrameAvailable, 1);
    SDL_SemWait(vBlankSemaphore);
}

u8 BinToBcd(u8 bin)
{
    int placeCounter = 1;
    u8 out = 0;
    do
    {
        out |= (bin % 10) * placeCounter;
        placeCounter *= 16;
    }
    while ((bin /= 10) > 0);

    return out;
}

void Platform_GetStatus(struct SiiRtcInfo *rtc)
{
    rtc->status = internalClock.status;
}

void Platform_SetStatus(struct SiiRtcInfo *rtc)
{
    internalClock.status = rtc->status;
}

static void UpdateInternalClock(void)
{
    time_t rawTime = time(NULL);
    struct tm *time = localtime(&rawTime);

    internalClock.year = BinToBcd(time->tm_year - 100);
    internalClock.month = BinToBcd(time->tm_mon + 1);
    internalClock.day = BinToBcd(time->tm_mday);
    internalClock.dayOfWeek = BinToBcd(time->tm_wday);
    internalClock.hour = BinToBcd(time->tm_hour);
    internalClock.minute = BinToBcd(time->tm_min);
    internalClock.second = BinToBcd(time->tm_sec);
}

void Platform_GetDateTime(struct SiiRtcInfo *rtc)
{
    UpdateInternalClock();

    rtc->year = internalClock.year;
    rtc->month = internalClock.month;
    rtc->day = internalClock.day;
    rtc->dayOfWeek = internalClock.dayOfWeek;
    rtc->hour = internalClock.hour;
    rtc->minute = internalClock.minute;
    rtc->second = internalClock.second;
    DBGPRINTF("GetDateTime: %d-%02d-%02d %02d:%02d:%02d\n", ConvertBcdToBinary(rtc->year),
                                                         ConvertBcdToBinary(rtc->month),
                                                         ConvertBcdToBinary(rtc->day),
                                                         ConvertBcdToBinary(rtc->hour),
                                                         ConvertBcdToBinary(rtc->minute),
                                                         ConvertBcdToBinary(rtc->second));
}

void Platform_SetDateTime(struct SiiRtcInfo *rtc)
{
    internalClock.month = rtc->month;
    internalClock.day = rtc->day;
    internalClock.dayOfWeek = rtc->dayOfWeek;
    internalClock.hour = rtc->hour;
    internalClock.minute = rtc->minute;
    internalClock.second = rtc->second;
}

void Platform_GetTime(struct SiiRtcInfo *rtc)
{
    UpdateInternalClock();

    rtc->hour = internalClock.hour;
    rtc->minute = internalClock.minute;
    rtc->second = internalClock.second;
    DBGPRINTF("GetTime: %02d:%02d:%02d\n", ConvertBcdToBinary(rtc->hour),
                                        ConvertBcdToBinary(rtc->minute),
                                        ConvertBcdToBinary(rtc->second));
}

void Platform_SetTime(struct SiiRtcInfo *rtc)
{
    internalClock.hour = rtc->hour;
    internalClock.minute = rtc->minute;
    internalClock.second = rtc->second;
}

void Platform_SetAlarm(u8 *alarmData)
{
    // TODO
}

void SoftReset(u32 resetFlags)
{
    puts("Soft Reset called. Exiting.");
    exit(0);
}

#endif
