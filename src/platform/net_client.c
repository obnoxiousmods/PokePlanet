// Loopback link to the PokePlanet network sidecar.
//
// A worker thread owns the socket and does all blocking work. Game code only ever
// touches the shared state below, guarded by a single mutex, so nothing here can stall
// a frame.
//
// Wire format is documented in server/proto/src/ipc.rs: a little-endian u32 length,
// then a type byte, then a fixed-size payload. Everything is fixed width on purpose so
// this side needs no parser and no allocator.

#ifdef PLATFORM_SDL2

#include <stdio.h>
#include <string.h>

#ifdef _WIN32
#include <winsock2.h>
#include <ws2tcpip.h>
#else
#include <errno.h>
#include <netinet/in.h>
#include <sys/select.h>
#include <sys/socket.h>
#include <unistd.h>
#endif

#include <SDL2/SDL.h>

#include "global.h"
#include "net_client.h"
#include "gba/flash_internal.h"
#include "platform.h"

// Sidecar -> game
#define MSG_STATUS       0x01
#define MSG_SNAPSHOT     0x02
#define MSG_CHAT         0x03
#define MSG_PROFILE      0x04
#define MSG_BATTLE_INVITE   0x05
#define MSG_BATTLE_ANSWERED 0x06
#define MSG_BATTLE_FAILED   0x07
#define MSG_SAVE_IMAGE      0x08
// Game -> sidecar
#define MSG_SELF_STATE   0x81
#define MSG_BEGIN_LOGIN  0x82
#define MSG_CANCEL_LOGIN 0x83
#define MSG_CHAT_SEND    0x84
#define MSG_LOGOUT       0x85
#define MSG_BATTLE_REQUEST 0x86
#define MSG_BATTLE_RESPOND 0x87
#define MSG_SAVE_CHUNK     0x88

// Big enough that the 128KB image is a few hundred frames rather than thousands, small
// enough to sit on the stack.
#define SAVE_CHUNK_BYTES   1024

// Must match REMOTE_PLAYER_SIZE in the Rust protocol crate.
#define REMOTE_PLAYER_STRIDE 32

#define DEFAULT_SIDECAR_PORT 38400
#define RECONNECT_DELAY_MS   1000
// Connection attempts to tolerate before concluding no sidecar is coming and starting one.
// The first few failures are the ordinary case of the game winning the race at startup.
#define RELAUNCH_AFTER_ATTEMPTS 5
#define RELAUNCH_BACKOFF_MAX    60
// Matches the sidecar's own upstream cadence; sending faster would be discarded anyway.
#define SELF_STATE_INTERVAL_MS 100

#define RX_BUFFER_SIZE   16384
#define TX_QUEUE_FRAMES  32
#define TX_FRAME_MAX     256
#define CHAT_INBOX_LINES 16

struct NetState
{
    SDL_mutex *lock;
    SDL_Thread *thread;
    bool8 running;
    bool8 linked;

    u8 authState;
    char playerName[NET_NAME_LEN];
    char loginUrl[NET_URL_LEN];

    struct NetRemotePlayer remotePlayers[NET_MAX_REMOTE_PLAYERS];
    u8 remoteCount;

    struct NetProfile profile;
    bool8 hasProfile;

    struct NetChatLine chatInbox[CHAT_INBOX_LINES];
    u8 chatHead;   // next slot to write
    u8 chatTail;   // next slot to read
    u8 chatCount;

    // Only the newest of each is kept. Interrupting the player once for the challenge in
    // front of them is right; queueing up a backlog to answer is not.
    struct NetBattleInvite invite;
    bool8 hasInvite;
    struct NetBattleInvite answer;
    bool8 answerAccepted;
    bool8 hasAnswer;
    char failedReason[NET_TEXT_LEN];
    bool8 hasFailed;

    // The server handed over a save and it is now sitting in the flash mirror.
    bool8 hasServerSave;

    // Frames waiting for the worker thread to push onto the socket.
    u8 txQueue[TX_QUEUE_FRAMES][TX_FRAME_MAX];
    u16 txLength[TX_QUEUE_FRAMES];
    u8 txHead;
    u8 txTail;
    u8 txCount;

    u32 lastSelfStateMs;
};

static struct NetState sNet;
static bool8 sInitialised = FALSE;

static u16 GetSidecarPort(void);

// ---------------------------------------------------------------------------
// Small helpers for the fixed-layout wire format.
// ---------------------------------------------------------------------------

static void PutU32(u8 *p, u32 v)
{
    p[0] = (u8)(v & 0xFF);
    p[1] = (u8)((v >> 8) & 0xFF);
    p[2] = (u8)((v >> 16) & 0xFF);
    p[3] = (u8)((v >> 24) & 0xFF);
}

static void PutU16(u8 *p, u16 v)
{
    p[0] = (u8)(v & 0xFF);
    p[1] = (u8)(v >> 8);
}

static u16 ReadU16(const u8 *p)
{
    return (u16)(p[0] | ((u16)p[1] << 8));
}

static u32 ReadU32(const u8 *p)
{
    return (u32)p[0] | ((u32)p[1] << 8) | ((u32)p[2] << 16) | ((u32)p[3] << 24);
}

static s16 ReadS16(const u8 *p)
{
    return (s16)ReadU16(p);
}

// Copy a NUL-padded wire field into a NUL-terminated C string.
static void CopyField(char *dest, const u8 *src, int width)
{
    int i;
    for (i = 0; i < width - 1 && src[i] != 0; i++)
        dest[i] = (char)src[i];
    dest[i] = '\0';
}

// Write a C string into a fixed NUL-padded wire field.
static void WriteField(u8 *dest, const char *src, int width)
{
    int i;
    memset(dest, 0, width);
    if (src == NULL)
        return;
    for (i = 0; i < width - 1 && src[i] != '\0'; i++)
        dest[i] = (u8)src[i];
}

// Queue a frame body (type byte first) for the worker thread. Caller holds the lock.
static void EnqueueLocked(const u8 *body, u16 bodyLen)
{
    u8 *slot;

    if (bodyLen + 4 > TX_FRAME_MAX || sNet.txCount >= TX_QUEUE_FRAMES)
        return; // Nothing here is worth stalling the game for; drop it.

    slot = sNet.txQueue[sNet.txHead];
    slot[0] = (u8)(bodyLen & 0xFF);
    slot[1] = (u8)((bodyLen >> 8) & 0xFF);
    slot[2] = 0;
    slot[3] = 0;
    memcpy(slot + 4, body, bodyLen);
    sNet.txLength[sNet.txHead] = bodyLen + 4;
    sNet.txHead = (sNet.txHead + 1) % TX_QUEUE_FRAMES;
    sNet.txCount++;
}

static void Enqueue(const u8 *body, u16 bodyLen)
{
    SDL_LockMutex(sNet.lock);
    EnqueueLocked(body, bodyLen);
    SDL_UnlockMutex(sNet.lock);
}

// ---------------------------------------------------------------------------
// Decoding messages from the sidecar. Called on the worker thread.
// ---------------------------------------------------------------------------

static void HandleStatus(const u8 *payload, u32 len)
{
    if (len < 1 + NET_NAME_LEN + NET_URL_LEN)
        return;

    SDL_LockMutex(sNet.lock);
    sNet.authState = payload[0];
    CopyField(sNet.playerName, payload + 1, NET_NAME_LEN);
    CopyField(sNet.loginUrl, payload + 1 + NET_NAME_LEN, NET_URL_LEN);
    SDL_UnlockMutex(sNet.lock);

    // The character is being played somewhere else now. Nothing this copy does from here
    // could reach the world, and leaving it running invites the player to keep playing a
    // session whose progress is quietly going nowhere.
    if (sNet.authState == NET_AUTH_SUPERSEDED)
    {
        SDL_Log("net: signed in elsewhere; closing");
        Platform_RequestQuit();
    }
}

static void HandleSnapshot(const u8 *payload, u32 len)
{
    u16 count;
    u16 i;
    u8 stored = 0;

    if (len < 2)
        return;
    count = ReadU16(payload);
    if (2 + (u32)count * REMOTE_PLAYER_STRIDE > len)
        return; // truncated; ignore rather than read past the buffer

    SDL_LockMutex(sNet.lock);
    for (i = 0; i < count && stored < NET_MAX_REMOTE_PLAYERS; i++)
    {
        const u8 *e = payload + 2 + (u32)i * REMOTE_PLAYER_STRIDE;
        struct NetRemotePlayer *p = &sNet.remotePlayers[stored];

        p->playerId = ReadU32(e);
        p->mapGroup = e[4];
        p->mapNum = e[5];
        p->x = ReadS16(e + 6);
        p->y = ReadS16(e + 8);
        p->facing = e[10];
        p->graphicsId = e[11];
        p->elevation = e[12];
        p->moving = e[13] ? TRUE : FALSE;
        CopyField(p->name, e + 14, NET_NAME_LEN);
        stored++;
    }
    sNet.remoteCount = stored;
    SDL_UnlockMutex(sNet.lock);
}

static void HandleProfile(const u8 *payload, u32 len)
{
    // graphicsId, badges, caught, seen, playTime, money, name
    if (len < 1 + 1 + 2 + 2 + 4 + 4 + NET_NAME_LEN)
        return;

    SDL_LockMutex(sNet.lock);
    sNet.profile.graphicsId = payload[0];
    sNet.profile.badges = payload[1];
    sNet.profile.pokedexCaught = ReadU16(payload + 2);
    sNet.profile.pokedexSeen = ReadU16(payload + 4);
    sNet.profile.playTimeSeconds = ReadU32(payload + 6);
    sNet.profile.money = ReadU32(payload + 10);
    CopyField(sNet.profile.name, payload + 14, NET_NAME_LEN);
    sNet.hasProfile = TRUE;
    SDL_UnlockMutex(sNet.lock);
}

static void HandleChat(const u8 *payload, u32 len)
{
    struct NetChatLine *line;

    if (len < 1 + NET_SENDER_LEN + NET_TEXT_LEN)
        return;

    SDL_LockMutex(sNet.lock);
    line = &sNet.chatInbox[sNet.chatHead];
    line->kind = payload[0];
    CopyField(line->from, payload + 1, NET_SENDER_LEN);
    CopyField(line->text, payload + 1 + NET_SENDER_LEN, NET_TEXT_LEN);
    sNet.chatHead = (sNet.chatHead + 1) % CHAT_INBOX_LINES;
    if (sNet.chatCount < CHAT_INBOX_LINES)
        sNet.chatCount++;
    else
        sNet.chatTail = (sNet.chatTail + 1) % CHAT_INBOX_LINES; // oldest falls off
    SDL_UnlockMutex(sNet.lock);
}

static void HandleBattleInvite(const u8 *payload, u32 len)
{
    if (len < 4 + NET_NAME_LEN)
        return;

    SDL_LockMutex(sNet.lock);
    sNet.invite.from = ReadU32(payload);
    CopyField(sNet.invite.fromName, payload + 4, NET_NAME_LEN);
    sNet.hasInvite = TRUE;
    SDL_UnlockMutex(sNet.lock);
}

static void HandleBattleAnswered(const u8 *payload, u32 len)
{
    if (len < 1 + 4 + NET_NAME_LEN)
        return;

    SDL_LockMutex(sNet.lock);
    sNet.answerAccepted = payload[0] != 0;
    sNet.answer.from = ReadU32(payload + 1);
    CopyField(sNet.answer.fromName, payload + 5, NET_NAME_LEN);
    sNet.hasAnswer = TRUE;
    SDL_UnlockMutex(sNet.lock);
}

static void HandleBattleFailed(const u8 *payload, u32 len)
{
    if (len < NET_TEXT_LEN)
        return;

    SDL_LockMutex(sNet.lock);
    CopyField(sNet.failedReason, payload, NET_TEXT_LEN);
    sNet.hasFailed = TRUE;
    SDL_UnlockMutex(sNet.lock);
}

// The server's copy of the save, written straight into the flash mirror.
//
// This is the point of the whole exercise: the game's load path already reads from
// FLASH_BASE, so once this lands the game reads the server's save without any other part of
// it knowing the difference. Written from the worker thread, which is safe here because the
// only moment this arrives is at sign-in, while the player is still on the menu and nothing
// is reading flash.
static void HandleSaveImage(const u8 *payload, u32 len)
{
    u32 offset;
    u32 total;
    u16 length;

    if (len < 10)
        return;

    offset = ReadU32(payload);
    total = ReadU32(payload + 4);
    length = ReadU16(payload + 8);

    if (len < 10u + length)
        return;
    if (total != sizeof(FLASH_BASE))
    {
        SDL_Log("net: ignoring a save of %u bytes; this build expects %u",
                (unsigned)total, (unsigned)sizeof(FLASH_BASE));
        return;
    }
    if (offset > sizeof(FLASH_BASE) || length > sizeof(FLASH_BASE) - offset)
        return;

    memcpy(FLASH_BASE + offset, payload + 10, length);

    if (offset + length == total)
    {
        SDL_LockMutex(sNet.lock);
        sNet.hasServerSave = TRUE;
        SDL_UnlockMutex(sNet.lock);
        SDL_Log("net: loaded the server's save (%u bytes)", (unsigned)total);
    }
}

static void DispatchFrame(const u8 *body, u32 len)
{
    if (len < 1)
        return;
    switch (body[0])
    {
    case MSG_STATUS:
        HandleStatus(body + 1, len - 1);
        break;
    case MSG_BATTLE_INVITE:
        HandleBattleInvite(body + 1, len - 1);
        break;
    case MSG_BATTLE_ANSWERED:
        HandleBattleAnswered(body + 1, len - 1);
        break;
    case MSG_BATTLE_FAILED:
        HandleBattleFailed(body + 1, len - 1);
        break;
    case MSG_SAVE_IMAGE:
        HandleSaveImage(body + 1, len - 1);
        break;
    case MSG_SNAPSHOT:
        HandleSnapshot(body + 1, len - 1);
        break;
    case MSG_CHAT:
        HandleChat(body + 1, len - 1);
        break;
    case MSG_PROFILE:
        HandleProfile(body + 1, len - 1);
        break;
    default:
        break; // Unknown types are ignored so the sidecar can add messages freely.
    }
}

// ---------------------------------------------------------------------------
// Worker thread.
// ---------------------------------------------------------------------------

// The link is plain BSD sockets on both platforms; only the spellings differ. Keeping one
// implementation behind these shims means the headless Linux build talks to the sidecar
// exactly like the shipped Windows one, so multiplayer can be tested without a desktop.
#ifdef _WIN32
typedef SOCKET NetSocket;
#define NET_INVALID_SOCKET INVALID_SOCKET
#define NetCloseSocket(s)  closesocket(s)
// Winsock ignores nfds; POSIX requires the highest descriptor plus one.
#define NET_SELECT_NFDS(s) 0
#define NET_SEND_FLAGS     0
#define NetCallInterrupted() (WSAGetLastError() == WSAEINTR)
#else
typedef int NetSocket;
#define NET_INVALID_SOCKET (-1)
#define NetCloseSocket(s)  close(s)
#define NET_SELECT_NFDS(s) ((int)(s) + 1)
// Without this a send() to a sidecar that has exited raises SIGPIPE, whose default
// disposition kills the game. Windows has no such signal.
#define NET_SEND_FLAGS     MSG_NOSIGNAL
#define NetCallInterrupted() (errno == EINTR)
#endif

static void ResetLinkState(void)
{
    SDL_LockMutex(sNet.lock);
    sNet.linked = FALSE;
    sNet.authState = NET_AUTH_OFFLINE;
    sNet.remoteCount = 0;
    sNet.txHead = sNet.txTail = sNet.txCount = 0;
    SDL_UnlockMutex(sNet.lock);
}

static NetSocket ConnectToSidecar(void)
{
    NetSocket sock;
    struct sockaddr_in addr;

    sock = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if (sock == NET_INVALID_SOCKET)
        return NET_INVALID_SOCKET;

    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_port = htons(GetSidecarPort());
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);

    if (connect(sock, (struct sockaddr *)&addr, sizeof(addr)) != 0)
    {
        NetCloseSocket(sock);
        return NET_INVALID_SOCKET;
    }
    return sock;
}

// Push everything queued. Returns FALSE if the socket died.
static bool8 FlushTxQueue(NetSocket sock)
{
    u8 frame[TX_FRAME_MAX];
    u16 length;

    for (;;)
    {
        SDL_LockMutex(sNet.lock);
        if (sNet.txCount == 0)
        {
            SDL_UnlockMutex(sNet.lock);
            return TRUE;
        }
        length = sNet.txLength[sNet.txTail];
        memcpy(frame, sNet.txQueue[sNet.txTail], length);
        sNet.txTail = (sNet.txTail + 1) % TX_QUEUE_FRAMES;
        sNet.txCount--;
        SDL_UnlockMutex(sNet.lock);

        {
            int sent = 0;
            while (sent < (int)length)
            {
                int n = send(sock, (const char *)frame + sent, length - sent, NET_SEND_FLAGS);
                if (n < 0 && NetCallInterrupted())
                    continue;
                if (n <= 0)
                    return FALSE;
                sent += n;
            }
        }
    }
}

// Ship the whole flash image to the sidecar, in slices.
//
// Sent straight down the socket rather than through the outbound queue: the queue holds 32
// small frames and this is 128KB, so it would overflow immediately. The worker owns the
// socket, so writing from here is safe, and loopback makes it quick.
//
// The game may write more sectors while this is in flight. That is why the flag is taken
// before reading rather than after: a write during the send raises it again and the next
// pass sends a consistent image over the top.
static bool8 SendSaveImage(NetSocket sock)
{
    u8 frame[10 + SAVE_CHUNK_BYTES];
    u32 offset;

    for (offset = 0; offset < sizeof(FLASH_BASE); offset += SAVE_CHUNK_BYTES)
    {
        u32 remaining = sizeof(FLASH_BASE) - offset;
        u16 length = remaining < SAVE_CHUNK_BYTES ? (u16)remaining : SAVE_CHUNK_BYTES;
        u32 bodyLen = 1 + 10 + length;
        u8 header[4];
        int sent;

        frame[0] = MSG_SAVE_CHUNK;
        PutU32(frame + 1, offset);
        PutU32(frame + 5, sizeof(FLASH_BASE));
        PutU16(frame + 9, length);
        memcpy(frame + 11, FLASH_BASE + offset, length);

        PutU32(header, bodyLen);
        for (sent = 0; sent < (int)sizeof(header); )
        {
            int n = send(sock, (const char *)header + sent, sizeof(header) - sent, NET_SEND_FLAGS);
            if (n < 0 && NetCallInterrupted())
                continue;
            if (n <= 0)
                return FALSE;
            sent += n;
        }
        for (sent = 0; sent < (int)bodyLen; )
        {
            int n = send(sock, (const char *)frame + sent, bodyLen - sent, NET_SEND_FLAGS);
            if (n < 0 && NetCallInterrupted())
                continue;
            if (n <= 0)
                return FALSE;
            sent += n;
        }
    }

    SDL_Log("net: sent the save (%u bytes)", (unsigned)sizeof(FLASH_BASE));
    return TRUE;
}

static int NetThreadMain(void *unused)
{
    u8 rx[RX_BUFFER_SIZE];
    u32 rxUsed = 0;
    u32 failedConnects = 0;
    u32 relaunchAfter = RELAUNCH_AFTER_ATTEMPTS;

#ifdef _WIN32
    WSADATA wsa;
    if (WSAStartup(MAKEWORD(2, 2), &wsa) != 0)
    {
        SDL_Log("net: WSAStartup failed; multiplayer disabled");
        return 0;
    }
#endif

    while (sNet.running)
    {
        NetSocket sock = ConnectToSidecar();
        if (sock == NET_INVALID_SOCKET)
        {
            // The sidecar is started once at boot, so if it dies -- it lost a race for the
            // IPC port, crashed, or was killed -- the game would otherwise spend the rest
            // of the session connecting to nothing and silently stay offline. Start
            // another, backing off so a sidecar that cannot run is not respawned forever.
            if (++failedConnects >= relaunchAfter)
            {
                SDL_Log("net: no sidecar after %u attempts; starting one",
                        (unsigned)failedConnects);
                Platform_LaunchSidecar();
                failedConnects = 0;
                if (relaunchAfter < RELAUNCH_BACKOFF_MAX)
                    relaunchAfter *= 2;
            }
            SDL_Delay(RECONNECT_DELAY_MS);
            continue;
        }

        SDL_Log("net: linked to sidecar on port %u", (unsigned)GetSidecarPort());
        // A link that came up resets the backoff, so a later death is handled promptly
        // rather than inheriting the patience earned by an earlier failure.
        failedConnects = 0;
        relaunchAfter = RELAUNCH_AFTER_ATTEMPTS;
        SDL_LockMutex(sNet.lock);
        sNet.linked = TRUE;
        SDL_UnlockMutex(sNet.lock);
        rxUsed = 0;

        while (sNet.running)
        {
            fd_set readable;
            struct timeval timeout;
            int ready;

            if (!FlushTxQueue(sock))
                break;

            // A save happened; the server's copy is now out of date.
            if (Net_TakeSaveChanged() && !SendSaveImage(sock))
                break;

            FD_ZERO(&readable);
            FD_SET(sock, &readable);
            // Short timeout so queued frames go out promptly and shutdown is responsive.
            timeout.tv_sec = 0;
            timeout.tv_usec = 20000;

            ready = select(NET_SELECT_NFDS(sock), &readable, NULL, NULL, &timeout);
            if (ready < 0)
            {
                if (NetCallInterrupted())
                    continue;
                break;
            }
            if (ready == 0)
                continue;

            {
                int n = recv(sock, (char *)rx + rxUsed, RX_BUFFER_SIZE - rxUsed, 0);
                if (n < 0 && NetCallInterrupted())
                    continue;
                if (n <= 0)
                    break; // sidecar closed or errored
                rxUsed += n;
            }

            // Drain every complete frame sitting in the buffer.
            for (;;)
            {
                u32 bodyLen;
                if (rxUsed < 4)
                    break;
                bodyLen = ReadU32(rx);
                if (bodyLen == 0 || bodyLen > RX_BUFFER_SIZE - 4)
                {
                    // The stream is out of sync and cannot be recovered; drop the link
                    // and let the reconnect loop start clean.
                    SDL_Log("net: bad frame length %u, resetting link", (unsigned)bodyLen);
                    rxUsed = 0;
                    goto linkBroken;
                }
                if (rxUsed < 4 + bodyLen)
                    break;
                DispatchFrame(rx + 4, bodyLen);
                memmove(rx, rx + 4 + bodyLen, rxUsed - (4 + bodyLen));
                rxUsed -= 4 + bodyLen;
            }
        }

    linkBroken:
        NetCloseSocket(sock);
        ResetLinkState();
        SDL_Log("net: sidecar link lost");
        if (sNet.running)
            SDL_Delay(RECONNECT_DELAY_MS);
    }

#ifdef _WIN32
    WSACleanup();
#endif
    return 0;
}

// ---------------------------------------------------------------------------
// Public API.
// ---------------------------------------------------------------------------

void Net_Init(void)
{
    if (sInitialised)
        return;

    memset(&sNet, 0, sizeof(sNet));
    sNet.lock = SDL_CreateMutex();
    if (sNet.lock == NULL)
    {
        SDL_Log("net: could not create mutex; multiplayer disabled");
        return;
    }
    sNet.running = TRUE;
    sNet.authState = NET_AUTH_OFFLINE;
    sNet.thread = SDL_CreateThread(NetThreadMain, "PokePlanetNet", NULL);
    if (sNet.thread == NULL)
    {
        SDL_Log("net: could not start network thread; multiplayer disabled");
        sNet.running = FALSE;
        return;
    }
    sInitialised = TRUE;
}

void Net_Shutdown(void)
{
    if (!sInitialised)
        return;
    sNet.running = FALSE;
    // The worker wakes at least every 20ms, so this returns promptly.
    SDL_WaitThread(sNet.thread, NULL);
    SDL_DestroyMutex(sNet.lock);
    sNet.lock = NULL;
    sInitialised = FALSE;
}

bool8 Net_IsLinked(void)
{
    bool8 linked;
    if (!sInitialised)
        return FALSE;
    SDL_LockMutex(sNet.lock);
    linked = sNet.linked;
    SDL_UnlockMutex(sNet.lock);
    return linked;
}

u8 Net_GetAuthState(void)
{
    u8 state;
    if (!sInitialised)
        return NET_AUTH_OFFLINE;
    SDL_LockMutex(sNet.lock);
    state = sNet.authState;
    SDL_UnlockMutex(sNet.lock);
    return state;
}

const char *Net_GetPlayerName(void)
{
    return sInitialised ? sNet.playerName : "";
}

const char *Net_GetLoginUrl(void)
{
    return sInitialised ? sNet.loginUrl : "";
}

bool8 Net_GetProfile(struct NetProfile *out)
{
    bool8 have;

    if (!sInitialised || out == NULL)
        return FALSE;

    SDL_LockMutex(sNet.lock);
    have = sNet.hasProfile;
    if (have)
        *out = sNet.profile;
    SDL_UnlockMutex(sNet.lock);
    return have;
}

void Net_BeginLogin(void)
{
    u8 body[1];
    if (!sInitialised)
        return;
    body[0] = MSG_BEGIN_LOGIN;
    Enqueue(body, 1);
}

void Net_CancelLogin(void)
{
    u8 body[1];
    if (!sInitialised)
        return;
    body[0] = MSG_CANCEL_LOGIN;
    Enqueue(body, 1);
}

void Net_Logout(void)
{
    u8 body[1];
    if (!sInitialised)
        return;
    body[0] = MSG_LOGOUT;
    Enqueue(body, 1);
}

void Net_SendSelf(u8 mapGroup, u8 mapNum, s16 x, s16 y, u8 facing, bool8 moving,
                  u8 graphicsId, u8 elevation)
{
    u8 body[11];
    u32 now;

    if (!sInitialised)
        return;

    // Callers may run this every frame; only one report per interval reaches the wire.
    now = SDL_GetTicks();
    SDL_LockMutex(sNet.lock);
    if (!sNet.linked || (u32)(now - sNet.lastSelfStateMs) < SELF_STATE_INTERVAL_MS)
    {
        SDL_UnlockMutex(sNet.lock);
        return;
    }
    sNet.lastSelfStateMs = now;
    SDL_UnlockMutex(sNet.lock);

    body[0] = MSG_SELF_STATE;
    body[1] = mapGroup;
    body[2] = mapNum;
    PutU16(body + 3, (u16)x);
    PutU16(body + 5, (u16)y);
    body[7] = facing;
    body[8] = moving ? 1 : 0;
    body[9] = graphicsId;
    body[10] = elevation;
    Enqueue(body, sizeof(body));
}

u8 Net_GetRemotePlayers(struct NetRemotePlayer *out)
{
    u8 count;
    if (!sInitialised || out == NULL)
        return 0;
    SDL_LockMutex(sNet.lock);
    count = sNet.remoteCount;
    memcpy(out, sNet.remotePlayers, sizeof(struct NetRemotePlayer) * count);
    SDL_UnlockMutex(sNet.lock);
    return count;
}

void Net_RequestBattle(u32 playerId)
{
    u8 body[5];

    if (!sInitialised)
        return;
    body[0] = MSG_BATTLE_REQUEST;
    body[1] = (u8)(playerId & 0xFF);
    body[2] = (u8)((playerId >> 8) & 0xFF);
    body[3] = (u8)((playerId >> 16) & 0xFF);
    body[4] = (u8)((playerId >> 24) & 0xFF);
    Enqueue(body, sizeof(body));
}

void Net_RespondToBattle(u32 playerId, bool8 accepted)
{
    u8 body[6];

    if (!sInitialised)
        return;
    body[0] = MSG_BATTLE_RESPOND;
    body[1] = (u8)(playerId & 0xFF);
    body[2] = (u8)((playerId >> 8) & 0xFF);
    body[3] = (u8)((playerId >> 16) & 0xFF);
    body[4] = (u8)((playerId >> 24) & 0xFF);
    body[5] = accepted ? 1 : 0;
    Enqueue(body, sizeof(body));
}

void Net_SendChat(u8 kind, const char *target, const char *text)
{
    u8 body[2 + NET_SENDER_LEN + NET_TEXT_LEN];

    if (!sInitialised || text == NULL || text[0] == '\0')
        return;

    body[0] = MSG_CHAT_SEND;
    body[1] = kind;
    WriteField(body + 2, target, NET_SENDER_LEN);
    WriteField(body + 2 + NET_SENDER_LEN, text, NET_TEXT_LEN);
    Enqueue(body, sizeof(body));
}

bool8 Net_PopBattleInvite(struct NetBattleInvite *out)
{
    if (!sInitialised || out == NULL)
        return FALSE;

    SDL_LockMutex(sNet.lock);
    if (!sNet.hasInvite)
    {
        SDL_UnlockMutex(sNet.lock);
        return FALSE;
    }
    *out = sNet.invite;
    sNet.hasInvite = FALSE;
    SDL_UnlockMutex(sNet.lock);
    return TRUE;
}

bool8 Net_PopBattleAnswer(struct NetBattleInvite *out, bool8 *accepted)
{
    if (!sInitialised || out == NULL || accepted == NULL)
        return FALSE;

    SDL_LockMutex(sNet.lock);
    if (!sNet.hasAnswer)
    {
        SDL_UnlockMutex(sNet.lock);
        return FALSE;
    }
    *out = sNet.answer;
    *accepted = sNet.answerAccepted;
    sNet.hasAnswer = FALSE;
    SDL_UnlockMutex(sNet.lock);
    return TRUE;
}

// Raised on the game thread from inside a sector write, cleared by whoever ships it.
// Deliberately a plain flag rather than a queue of sectors: a save writes fourteen of them
// in a burst, and the only thing worth knowing afterwards is that the save is no longer
// what the server has.
static volatile bool8 sSaveChanged;

void Net_NoteSaveChanged(void)
{
    sSaveChanged = TRUE;
}

bool8 Net_TakeSaveChanged(void)
{
    if (!sSaveChanged)
        return FALSE;
    sSaveChanged = FALSE;
    return TRUE;
}

bool8 Net_TakeServerSave(void)
{
    bool8 had;

    if (!sInitialised)
        return FALSE;

    SDL_LockMutex(sNet.lock);
    had = sNet.hasServerSave;
    sNet.hasServerSave = FALSE;
    SDL_UnlockMutex(sNet.lock);
    return had;
}

bool8 Net_PopBattleFailure(char *out, u8 outSize)
{
    if (!sInitialised || out == NULL || outSize == 0)
        return FALSE;

    SDL_LockMutex(sNet.lock);
    if (!sNet.hasFailed)
    {
        SDL_UnlockMutex(sNet.lock);
        return FALSE;
    }
    {
        u8 i;

        for (i = 0; i < outSize - 1 && sNet.failedReason[i] != '\0'; i++)
            out[i] = sNet.failedReason[i];
        out[i] = '\0';
    }
    sNet.hasFailed = FALSE;
    SDL_UnlockMutex(sNet.lock);
    return TRUE;
}

bool8 Net_PopChatLine(struct NetChatLine *out)
{
    if (!sInitialised || out == NULL)
        return FALSE;

    SDL_LockMutex(sNet.lock);
    if (sNet.chatCount == 0)
    {
        SDL_UnlockMutex(sNet.lock);
        return FALSE;
    }
    *out = sNet.chatInbox[sNet.chatTail];
    sNet.chatTail = (sNet.chatTail + 1) % CHAT_INBOX_LINES;
    sNet.chatCount--;
    SDL_UnlockMutex(sNet.lock);
    return TRUE;
}

// The sidecar port is configurable so two clients can run on one machine for testing.
// Platform_GetSidecarPort lives in the SDL backend alongside the rest of the config.
extern u16 Platform_GetSidecarPort(void);

static u16 GetSidecarPort(void)
{
    u16 port = Platform_GetSidecarPort();
    return port != 0 ? port : DEFAULT_SIDECAR_PORT;
}

#endif // PLATFORM_SDL2
