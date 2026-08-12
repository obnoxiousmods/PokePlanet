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
#define MSG_BATTLE_STARTING 0x09
#define MSG_CORRECTION      0x0A
#define MSG_LINK_BLOCK      0x0B
#define MSG_RATES           0x0C
#define MSG_BANK_STATE      0x0D
#define MSG_MAP_DROPS       0x0E
#define MSG_PICKED_UP       0x0F
// Game -> sidecar
#define MSG_SELF_STATE   0x81
#define MSG_BEGIN_LOGIN  0x82
#define MSG_CANCEL_LOGIN 0x83
#define MSG_CHAT_SEND    0x84
#define MSG_LOGOUT       0x85
#define MSG_BATTLE_REQUEST 0x86
#define MSG_BATTLE_RESPOND 0x87
#define MSG_SAVE_CHUNK     0x88
#define MSG_LINK_BLOCK_SEND 0x89
#define MSG_BATTLE_ENDED    0x8A
#define MSG_HELLO           0x8B
#define MSG_MONEY           0x8C
#define MSG_ITEM            0x8D
#define MSG_PARTY           0x8E
#define MSG_REGION          0x8F
#define MSG_BLOCK           0x90
#define MSG_KEYS            0x91
#define MSG_HARD_RESET      0x92
#define MSG_BANK_DEPOSIT    0x93
#define MSG_BANK_WITHDRAW   0x94
#define MSG_DROP_ITEM       0x95
#define MSG_PICKUP_ITEM     0x96
#define MSG_FORCE_BATTLE    0x97

// Lives in the SDL backend, like the sidecar port beside it.
extern const char *Platform_GetInstanceToken(void);

// Big enough that the 128KB image is a few hundred frames rather than thousands, small
// enough to sit on the stack.
#define SAVE_CHUNK_BYTES   1024
// Type byte, then offset, total and length. Named so the buffer below and the offsets
// written into it cannot drift apart: they did, by one byte, and every save smashed the
// stack on the way out of the function that sent it.
#define SAVE_CHUNK_HEADER  (1 + 4 + 4 + 2)

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
// Wide enough for the largest message any Net_Send* function produces, which is a block chunk
// at 10 + 1024 = 1034 bytes plus the 4-byte length header.
//
// This was 256 for a long time, when every message was a handful of bytes -- movement, chat, a
// battle block. The party (606 with header), region (1033) and block-chunk (1038) reports were
// added later and every single one of them was silently dropped by EnqueueLocked below, which
// is why levels, moves and experience never persisted while money and items did. Raising this
// is the fix; making the drop loud is what stops it happening again.
//
// Costs TX_QUEUE_FRAMES * TX_FRAME_MAX = ~35KB of static buffer, next to the 128KB already held
// for FLASH_BASE. Not a meaningful amount of memory for the class of bug it removes.
#define TX_FRAME_MAX     1088
#define CHAT_INBOX_LINES 16

// Blocks arrive faster than the game reads them: the battle engine sends one and then waits
// several frames before looking, and the handshake alone is a short burst. Deep enough that
// a burst is never dropped, which would hang the battle waiting for something that came and
// went.
#define LINK_BLOCK_INBOX 8

struct NetState
{
    SDL_mutex *lock;
    SDL_Thread *thread;
    bool8 running;
    bool8 linked;

    u8 authState;
    // Whether this session has ever reached ONLINE. See Net_WasOnline.
    bool8 wasOnline;
    char playerName[NET_NAME_LEN];
    char loginUrl[NET_URL_LEN];

    struct NetRemotePlayer remotePlayers[NET_MAX_REMOTE_PLAYERS];
    u8 remoteCount;

    struct NetProfile profile;
    bool8 hasProfile;

    struct NetRates rates;
    bool8 hasRates; // whether a rates frame has actually arrived; 0 is a valid experience rate
    struct NetLinkBlock blockInbox[LINK_BLOCK_INBOX];
    u8 blockHead;
    u8 blockTail;
    u8 blockCount;
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
    bool8 serverOwnsSave;
    // Whether the server has said either "here is your save" or "you have none".
    bool8 saveDecided;

    // The server refused where we said we were. Only the newest matters: it is the truth.
    struct NetCorrection correction;
    bool8 hasCorrection;

    // The latest PC bank balance and authoritative carried money from the server, pending until the
    // game thread takes it (and adopts the wallet).
    struct NetBankState bankState;
    bool8 hasBankState;

    // Items lying on the current map, from the server. The field renderer draws these and the
    // player picks them up. Replaced wholesale by each MSG_MAP_DROPS.
    struct NetDrop drops[NET_MAX_DROPS];
    u8 dropCount;
    // A pickup the server confirmed, waiting for the game thread to add it to the bag.
    u16 pickedItem;
    u16 pickedQuantity;
    bool8 hasPicked;

    // Both sides agreed to battle, and this is the slot the server gave us.
    struct NetBattleStart battleStart;
    bool8 hasBattleStart;

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
static bool8 EnqueueLocked(const u8 *body, u16 bodyLen)
{
    u8 *slot;

    // Oversize is a programming error, not a runtime condition: some Net_Send* function is
    // producing a message this queue was never sized for. It used to return silently here,
    // which is how three whole categories of save data went missing without a single log line.
    // Complain loudly -- a caller that trips this needs fixing, and the only thing worse than
    // the bug is not being able to see it.
    if (bodyLen + 4 > TX_FRAME_MAX)
    {
        SDL_Log("net: BUG: message 0x%02X is %u bytes, over the %u-byte frame limit; dropped",
                bodyLen > 0 ? body[0] : 0, (unsigned)(bodyLen + 4), (unsigned)TX_FRAME_MAX);
        return FALSE;
    }

    // A full queue is ordinary backpressure rather than a mistake, so it stays quiet. The
    // caller is told, and callers that care retry on the next tick.
    if (sNet.txCount >= TX_QUEUE_FRAMES)
        return FALSE;

    slot = sNet.txQueue[sNet.txHead];
    slot[0] = (u8)(bodyLen & 0xFF);
    slot[1] = (u8)((bodyLen >> 8) & 0xFF);
    slot[2] = 0;
    slot[3] = 0;
    memcpy(slot + 4, body, bodyLen);
    sNet.txLength[sNet.txHead] = bodyLen + 4;
    sNet.txHead = (sNet.txHead + 1) % TX_QUEUE_FRAMES;
    sNet.txCount++;
    return TRUE;
}

static bool8 Enqueue(const u8 *body, u16 bodyLen)
{
    bool8 ok;

    SDL_LockMutex(sNet.lock);
    ok = EnqueueLocked(body, bodyLen);
    SDL_UnlockMutex(sNet.lock);
    return ok;
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
    if (sNet.authState == NET_AUTH_ONLINE)
        sNet.wasOnline = TRUE;
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
        // Party count rides at offset 30, in what the stride leaves as padding after the name.
        p->partyCount = e[14 + NET_NAME_LEN];
        stored++;
    }
    sNet.remoteCount = stored;
    SDL_UnlockMutex(sNet.lock);
}

static void HandleProfile(const u8 *payload, u32 len)
{
    // graphicsId, badges, caught, seen, playTime, money, mapGroup, mapNum, x, y, name, and the
    // playerId appended after the name. The last four bytes were added but this length check was
    // not, so a short frame passed it and ReadU32(payload + 20 + NET_NAME_LEN) read past the body.
    if (len < 1 + 1 + 2 + 2 + 4 + 4 + 1 + 1 + 2 + 2 + NET_NAME_LEN + 4)
        return;

    SDL_LockMutex(sNet.lock);
    sNet.profile.graphicsId = payload[0];
    sNet.profile.badges = payload[1];
    sNet.profile.pokedexCaught = ReadU16(payload + 2);
    sNet.profile.pokedexSeen = ReadU16(payload + 4);
    sNet.profile.playTimeSeconds = ReadU32(payload + 6);
    sNet.profile.money = ReadU32(payload + 10);
    sNet.profile.mapGroup = payload[14];
    sNet.profile.mapNum = payload[15];
    sNet.profile.x = ReadS16(payload + 16);
    sNet.profile.y = ReadS16(payload + 18);
    CopyField(sNet.profile.name, payload + 20, NET_NAME_LEN);
    // Appended after the name on the wire, so everything before it keeps its offset.
    sNet.profile.playerId = ReadU32(payload + 20 + NET_NAME_LEN);
    sNet.hasProfile = TRUE;
    SDL_UnlockMutex(sNet.lock);
}

static void HandleRates(const u8 *payload, u32 len)
{
    if (len < 12)
        return;

    SDL_LockMutex(sNet.lock);
    sNet.rates.experience = (u16)(payload[0] | (payload[1] << 8));
    sNet.rates.encounter  = (u16)(payload[2] | (payload[3] << 8));
    sNet.rates.money      = (u16)(payload[4] | (payload[5] << 8));
    sNet.rates.items      = (u16)(payload[6] | (payload[7] << 8));
    sNet.rates.catch      = (u16)(payload[8] | (payload[9] << 8));
    sNet.rates.shopPrice  = (u16)(payload[10] | (payload[11] << 8));
    // Appended after the six scalars: a count byte then (species u16, multiplier u16) pairs. Older
    // servers send only the twelve scalar bytes, so a missing table just means no overrides.
    sNet.rates.speciesCount = 0;
    if (len >= 13)
    {
        u8 count = payload[12];
        u32 i;
        for (i = 0; i < count && i < NET_MAX_SPECIES_RATES; i++)
        {
            u32 at = 13 + i * 4;
            if (at + 4 > len)
                break;
            sNet.rates.speciesId[i]   = (u16)(payload[at] | (payload[at + 1] << 8));
            sNet.rates.speciesRate[i] = (u16)(payload[at + 2] | (payload[at + 3] << 8));
            sNet.rates.speciesCount++;
        }
    }
    sNet.hasRates = TRUE;
    SDL_UnlockMutex(sNet.lock);
    SDL_Log("net: rates x%u.%02u exp, x%u.%02u encounter, x%u.%02u money",
            sNet.rates.experience / 100, sNet.rates.experience % 100,
            sNet.rates.encounter / 100, sNet.rates.encounter % 100,
            sNet.rates.money / 100, sNet.rates.money % 100);
}

void Net_GetRates(struct NetRates *out)
{
    if (out == NULL)
        return;

    // The original game, until the server says otherwise. Callers multiply by these without
    // asking whether they arrived, so the default has to be the identity rather than zero --
    // which would quietly stop all experience rather than leaving it alone.
    out->experience = 100;
    out->encounter = 100;
    out->money = 100;
    out->items = 100;
    out->catch = 100;
    out->shopPrice = 100;

    if (!sInitialised)
        return;

    SDL_LockMutex(sNet.lock);
    if (sNet.hasRates)
        *out = sNet.rates;
    SDL_UnlockMutex(sNet.lock);
}

u16 Net_GetSpeciesEncounter(u16 species)
{
    u16 mult = 100; // unchanged unless the server sent an override for this species
    u8 i;

    if (!sInitialised)
        return mult;

    SDL_LockMutex(sNet.lock);
    if (sNet.hasRates)
    {
        for (i = 0; i < sNet.rates.speciesCount; i++)
        {
            if (sNet.rates.speciesId[i] == species)
            {
                mult = sNet.rates.speciesRate[i];
                break;
            }
        }
    }
    SDL_UnlockMutex(sNet.lock);
    return mult;
}

static void HandleLinkBlock(const u8 *payload, u32 len)
{
    struct NetLinkBlock *slot;
    u16 size;

    if (len < 3)
        return;

    size = (u16)(payload[1] | (payload[2] << 8));
    if (size > NET_LINK_BLOCK_MAX || len < 3u + size)
        return;

    SDL_LockMutex(sNet.lock);
    slot = &sNet.blockInbox[sNet.blockHead];
    slot->fromSlot = payload[0];
    slot->len = size;
    if (size != 0)
        memcpy(slot->bytes, payload + 3, size);
    sNet.blockHead = (sNet.blockHead + 1) % LINK_BLOCK_INBOX;
    if (sNet.blockCount < LINK_BLOCK_INBOX)
        sNet.blockCount++;
    else
        sNet.blockTail = (sNet.blockTail + 1) % LINK_BLOCK_INBOX; // oldest falls off
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
    // Zero bytes is the server saying "this character has no stored save". That is an answer,
    // not a failure: it is what a brand new character gets, and knowing it means the wait in the
    // main menu can end on a fact rather than on a timer.
    if (total == 0)
    {
        SDL_LockMutex(sNet.lock);
        sNet.saveDecided = TRUE;
        SDL_UnlockMutex(sNet.lock);
        SDL_Log("net: the server has no stored save for this character");
        return;
    }

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
        // Durable record that the server holds a save for this character.
        //
        // Separate from hasServerSave on purpose. That one is a one-shot signal the main menu
        // *consumes* via Net_TakeServerSave to decide whether to swap the loaded save, so it is
        // FALSE for the rest of the session afterwards. Gating the upload on it inverted the
        // behaviour: a successful handshake re-enabled uploads, and a failed one disabled them
        // -- disabling exactly the case where the client is holding local-only progress and the
        // upload is the last thing that would have saved it.
        sNet.serverOwnsSave = TRUE;
        sNet.saveDecided = TRUE;
        SDL_UnlockMutex(sNet.lock);
        SDL_Log("net: loaded the server's save (%u bytes)", (unsigned)total);
    }
}

static void HandleBankState(const u8 *payload, u32 len)
{
    u64 bank;

    if (len < 12) // u64 bank + u32 carried
        return;

    bank = (u64)payload[0] | ((u64)payload[1] << 8) | ((u64)payload[2] << 16)
         | ((u64)payload[3] << 24) | ((u64)payload[4] << 32) | ((u64)payload[5] << 40)
         | ((u64)payload[6] << 48) | ((u64)payload[7] << 56);

    SDL_LockMutex(sNet.lock);
    sNet.bankState.bank = (bank > 0xFFFFFFFFu) ? 0xFFFFFFFFu : (u32)bank;
    sNet.bankState.carried = (u32)(payload[8] | (payload[9] << 8) | (payload[10] << 16) | (payload[11] << 24));
    sNet.hasBankState = TRUE;
    SDL_UnlockMutex(sNet.lock);
}

static void HandleMapDrops(const u8 *payload, u32 len)
{
    u16 count;
    u16 i;
    u32 at;

    if (len < 2)
        return;
    count = (u16)(payload[0] | (payload[1] << 8));

    SDL_LockMutex(sNet.lock);
    sNet.dropCount = 0;
    at = 2;
    for (i = 0; i < count; i++)
    {
        u64 id;
        if (at + 16 > len || sNet.dropCount >= NET_MAX_DROPS)
            break;
        id = (u64)payload[at] | ((u64)payload[at + 1] << 8) | ((u64)payload[at + 2] << 16)
           | ((u64)payload[at + 3] << 24) | ((u64)payload[at + 4] << 32) | ((u64)payload[at + 5] << 40)
           | ((u64)payload[at + 6] << 48) | ((u64)payload[at + 7] << 56);
        sNet.drops[sNet.dropCount].id = (u32)id;
        sNet.drops[sNet.dropCount].item = (u16)(payload[at + 8] | (payload[at + 9] << 8));
        sNet.drops[sNet.dropCount].quantity = (u16)(payload[at + 10] | (payload[at + 11] << 8));
        sNet.drops[sNet.dropCount].x = (s16)(payload[at + 12] | (payload[at + 13] << 8));
        sNet.drops[sNet.dropCount].y = (s16)(payload[at + 14] | (payload[at + 15] << 8));
        sNet.dropCount++;
        at += 16;
    }
    SDL_UnlockMutex(sNet.lock);
}

static void HandlePickedUp(const u8 *payload, u32 len)
{
    if (len < 4)
        return;

    SDL_LockMutex(sNet.lock);
    sNet.pickedItem = (u16)(payload[0] | (payload[1] << 8));
    sNet.pickedQuantity = (u16)(payload[2] | (payload[3] << 8));
    sNet.hasPicked = TRUE;
    SDL_UnlockMutex(sNet.lock);
}

static void HandleCorrection(const u8 *payload, u32 len)
{
    if (len < 8)
        return;

    SDL_LockMutex(sNet.lock);
    sNet.correction.mapGroup = payload[0];
    sNet.correction.mapNum = payload[1];
    sNet.correction.x = ReadS16(payload + 2);
    sNet.correction.y = ReadS16(payload + 4);
    sNet.correction.facing = payload[6];
    sNet.correction.elevation = payload[7];
    sNet.hasCorrection = TRUE;
    SDL_UnlockMutex(sNet.lock);
}

static void HandleBattleStarting(const u8 *payload, u32 len)
{
    if (len < 1 + 4 + NET_NAME_LEN)
        return;

    SDL_LockMutex(sNet.lock);
    sNet.battleStart.linkId = payload[0];
    sNet.battleStart.opponent = ReadU32(payload + 1);
    CopyField(sNet.battleStart.opponentName, payload + 5, NET_NAME_LEN);
    sNet.hasBattleStart = TRUE;
    SDL_UnlockMutex(sNet.lock);
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
    case MSG_RATES:
        HandleRates(body + 1, len - 1);
        break;
    case MSG_LINK_BLOCK:
        HandleLinkBlock(body + 1, len - 1);
        break;
    case MSG_BANK_STATE:
        HandleBankState(body + 1, len - 1);
        break;
    case MSG_MAP_DROPS:
        HandleMapDrops(body + 1, len - 1);
        break;
    case MSG_PICKED_UP:
        HandlePickedUp(body + 1, len - 1);
        break;
    case MSG_CORRECTION:
        HandleCorrection(body + 1, len - 1);
        break;
    case MSG_BATTLE_STARTING:
        HandleBattleStarting(body + 1, len - 1);
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
    u8 frame[SAVE_CHUNK_HEADER + SAVE_CHUNK_BYTES];
    u32 offset;

    for (offset = 0; offset < sizeof(FLASH_BASE); offset += SAVE_CHUNK_BYTES)
    {
        u32 remaining = sizeof(FLASH_BASE) - offset;
        u16 length = remaining < SAVE_CHUNK_BYTES ? (u16)remaining : SAVE_CHUNK_BYTES;
        u32 bodyLen = SAVE_CHUNK_HEADER + length;
        u8 header[4];
        int sent;

        frame[0] = MSG_SAVE_CHUNK;
        PutU32(frame + 1, offset);
        PutU32(frame + 5, sizeof(FLASH_BASE));
        PutU16(frame + 9, length);
        memcpy(frame + SAVE_CHUNK_HEADER, FLASH_BASE + offset, length);

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

        // Say who we are before anything else.
        //
        // A sidecar that was told which game it belongs to will drop this connection if the
        // token does not match, which is what stops a game attaching to a sidecar that is
        // not its own and being signed in as somebody else's character. Sent every time the
        // socket is established, since a reconnect is a new connection to the sidecar.
        {
            const char *token = Platform_GetInstanceToken();
            u8 hello[1 + 32];
            size_t len = strlen(token);

            if (len > 32)
                len = 32;
            hello[0] = MSG_HELLO;
            memcpy(hello + 1, token, len);
            Enqueue(hello, (u32)(1 + len));
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
            // Uploads are retired for any character the server already holds a save for.
            //
            // Gated on sNet.hasServerSave, which is set only once a complete image has arrived
            // and been applied. An earlier version of this used its own flag set as soon as a
            // save *frame* appeared, before the size check -- so a save this build rejected
            // would have retired the upload anyway, leaving the server with no image for that
            // character and every typed report with nothing to splice into. Nothing would have
            // persisted at all.
            //
            // The flag is still taken either way, so a change does not sit pending forever and
            // reappear as an upload if this is ever turned back on.
            {
                bool8 changed = Net_TakeSaveChanged();
                bool8 serverHasSave;

                SDL_LockMutex(sNet.lock);
                serverHasSave = sNet.serverOwnsSave;
                SDL_UnlockMutex(sNet.lock);

                if (changed && !serverHasSave && !SendSaveImage(sock))
                    break;
            }

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
    // The socket thread writes sNet.playerName under the lock (see the auth handler). Reading the
    // raw field here without it let the game thread copy a name the socket thread was midway
    // through overwriting -- a torn read. Copy it out under the same lock into a buffer this thread
    // owns. Only the game thread calls this, so the static is not itself contended between callers;
    // callers use the result immediately (SeatPlayer, MmoText_FromAscii) rather than caching it.
    static char name[NET_NAME_LEN];
    if (!sInitialised)
        return "";
    SDL_LockMutex(sNet.lock);
    memcpy(name, sNet.playerName, sizeof(name));
    SDL_UnlockMutex(sNet.lock);
    return name;
}

const char *Net_GetLoginUrl(void)
{
    // Same torn-read hazard as Net_GetPlayerName: loginUrl is written by the socket thread under
    // the lock, so read it out under the lock into a game-thread-owned buffer.
    static char url[NET_URL_LEN];
    if (!sInitialised)
        return "";
    SDL_LockMutex(sNet.lock);
    memcpy(url, sNet.loginUrl, sizeof(url));
    SDL_UnlockMutex(sNet.lock);
    return url;
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

// One frame's key state, for the server to drive this character's validation instance with.
//
// Batched rather than sent every frame: sixty two-byte messages a second would be almost all
// framing. Frames accumulate and go out together a few times a second, which is what the sidecar
// and server both expect (a run of frames, not one). Only sent while linked; there is nowhere for
// them to go otherwise, and the server simply drops them when no instance is running for this
// character, so this costs nothing until replay validation is switched on.
#define KEYS_BATCH_FRAMES 6
void Net_SendKeys(u16 keys)
{
    static u16 sBatch[KEYS_BATCH_FRAMES];
    static u8 sCount;

    if (!sInitialised || !Net_IsLinked())
    {
        sCount = 0; // do not carry a half-batch across a disconnect
        return;
    }

    // Guarded so sBatch can never be indexed past its end even if the invariant below were ever
    // broken; in practice sCount is 0..KEYS_BATCH_FRAMES-1 here and the flush resets it at the cap.
    if (sCount < KEYS_BATCH_FRAMES)
        sBatch[sCount++] = keys;
    if (sCount >= KEYS_BATCH_FRAMES)
    {
        u8 body[1 + KEYS_BATCH_FRAMES * 2];
        u8 i;

        body[0] = MSG_KEYS;
        // Loop to the constant, not sCount: the batch is always exactly full here, and a constant
        // bound keeps every body[] write provably inside the fixed-size buffer.
        for (i = 0; i < KEYS_BATCH_FRAMES; i++)
        {
            body[1 + i * 2] = (u8)(sBatch[i] & 0xFF);
            body[1 + i * 2 + 1] = (u8)((sBatch[i] >> 8) & 0xFF);
        }
        // Dropped on backpressure like any other report: a lost run of inputs is a gap the
        // divergence check tolerates, not a reason to stall the game.
        Enqueue(body, (u16)(1 + KEYS_BATCH_FRAMES * 2));
        sCount = 0;
    }
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

void Net_ForceBattle(u32 playerId)
{
    u8 body[5];

    if (!sInitialised)
        return;
    body[0] = MSG_FORCE_BATTLE;
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

void Net_SendLinkBlock(const void *src, u16 size)
{
    u8 body[3 + NET_LINK_BLOCK_MAX];

    if (!sInitialised || src == NULL || size == 0 || size > NET_LINK_BLOCK_MAX)
        return;

    body[0] = MSG_LINK_BLOCK_SEND;
    body[1] = (u8)(size & 0xFF);
    body[2] = (u8)(size >> 8);
    memcpy(body + 3, src, size);
    Enqueue(body, 3 + size);
}

void Net_SendMoney(u32 amount)
{
    // Only when it actually changes. Money is read far more often than it is spent, and the
    // server does a database write and a save rebuild per report, so sending it every frame
    // would turn a wallet into a workload.
    static u32 sLast;
    static bool8 sHaveSent;
    u8 body[5];

    if (!sInitialised)
        return;
    if (sHaveSent && amount == sLast)
        return;

    sLast = amount;
    sHaveSent = TRUE;

    body[0] = MSG_MONEY;
    body[1] = (u8)(amount & 0xFF);
    body[2] = (u8)((amount >> 8) & 0xFF);
    body[3] = (u8)((amount >> 16) & 0xFF);
    body[4] = (u8)((amount >> 24) & 0xFF);
    Enqueue(body, sizeof(body));
}

void Net_SendItem(u8 pocket, u16 item, u16 quantity)
{
    u8 body[6];

    if (!sInitialised)
        return;

    body[0] = MSG_ITEM;
    body[1] = pocket;
    body[2] = (u8)(item & 0xFF);
    body[3] = (u8)(item >> 8);
    body[4] = (u8)(quantity & 0xFF);
    body[5] = (u8)(quantity >> 8);
    Enqueue(body, sizeof(body));
}

bool8 Net_SendParty(u8 count, const void *mons, u32 size)
{
    u8 body[2 + 600];

    if (!sInitialised)
        return FALSE;
    // The server refuses anything but the exact size, so sending a different one would be a
    // silent no-op. Better to notice here than to wonder later why nothing was stored.
    if (size != 600)
        return FALSE;

    body[0] = MSG_PARTY;
    body[1] = count;
    memcpy(body + 2, mons, size);
    return Enqueue(body, 2 + size);
}

bool8 Net_SendRegion(u32 offset, const void *bytes, u32 size)
{
    u8 body[5 + 0x400];

    if (!sInitialised)
        return FALSE;
    if (size > 0x400)
        return FALSE;

    body[0] = MSG_REGION;
    body[1] = (u8)(offset & 0xFF);
    body[2] = (u8)((offset >> 8) & 0xFF);
    body[3] = (u8)((offset >> 16) & 0xFF);
    body[4] = (u8)((offset >> 24) & 0xFF);
    memcpy(body + 5, bytes, size);
    return Enqueue(body, 5 + size);
}

bool8 Net_SendBlockChunk(u8 block, u32 offset, u32 total, const void *bytes, u32 size)
{
    u8 body[10 + 0x400];

    if (!sInitialised)
        return FALSE;
    if (size > 0x400)
        return FALSE;

    body[0] = MSG_BLOCK;
    body[1] = block;
    body[2] = (u8)(offset & 0xFF);
    body[3] = (u8)((offset >> 8) & 0xFF);
    body[4] = (u8)((offset >> 16) & 0xFF);
    body[5] = (u8)((offset >> 24) & 0xFF);
    body[6] = (u8)(total & 0xFF);
    body[7] = (u8)((total >> 8) & 0xFF);
    body[8] = (u8)((total >> 16) & 0xFF);
    body[9] = (u8)((total >> 24) & 0xFF);
    memcpy(body + 10, bytes, size);
    return Enqueue(body, 10 + size);
}

void Net_SendBattleEnded(void)
{
    u8 body[1];

    if (!sInitialised)
        return;

    body[0] = MSG_BATTLE_ENDED;
    Enqueue(body, sizeof(body));
}

void Net_HardReset(void)
{
    u8 body[1];

    if (!sInitialised)
        return;

    body[0] = MSG_HARD_RESET;
    Enqueue(body, sizeof(body));
}

void Net_BankDeposit(void)
{
    u8 body[1];

    if (!sInitialised)
        return;

    body[0] = MSG_BANK_DEPOSIT;
    Enqueue(body, sizeof(body));
}

void Net_BankWithdraw(void)
{
    u8 body[1];

    if (!sInitialised)
        return;

    body[0] = MSG_BANK_WITHDRAW;
    Enqueue(body, sizeof(body));
}

bool8 Net_PopBankState(struct NetBankState *out)
{
    if (!sInitialised || out == NULL)
        return FALSE;

    SDL_LockMutex(sNet.lock);
    if (!sNet.hasBankState)
    {
        SDL_UnlockMutex(sNet.lock);
        return FALSE;
    }
    *out = sNet.bankState;
    sNet.hasBankState = FALSE;
    SDL_UnlockMutex(sNet.lock);
    return TRUE;
}

void Net_DropItem(u16 item, u16 quantity)
{
    u8 body[5];

    if (!sInitialised)
        return;

    body[0] = MSG_DROP_ITEM;
    body[1] = (u8)(item & 0xFF);
    body[2] = (u8)((item >> 8) & 0xFF);
    body[3] = (u8)(quantity & 0xFF);
    body[4] = (u8)((quantity >> 8) & 0xFF);
    Enqueue(body, sizeof(body));
}

void Net_PickUpItem(u32 id)
{
    u8 body[9];

    if (!sInitialised)
        return;

    // The wire id is 64-bit; this client tracks the low 32 (enough to tell drops on one map apart),
    // so the high word goes out as zero. Ids start at zero and climb, so they stay in 32 bits.
    body[0] = MSG_PICKUP_ITEM;
    body[1] = (u8)(id & 0xFF);
    body[2] = (u8)((id >> 8) & 0xFF);
    body[3] = (u8)((id >> 16) & 0xFF);
    body[4] = (u8)((id >> 24) & 0xFF);
    body[5] = 0;
    body[6] = 0;
    body[7] = 0;
    body[8] = 0;
    Enqueue(body, sizeof(body));
}

u8 Net_GetMapDrops(struct NetDrop *out, u8 max)
{
    u8 n;

    if (!sInitialised || out == NULL)
        return 0;

    SDL_LockMutex(sNet.lock);
    n = (sNet.dropCount < max) ? sNet.dropCount : max;
    if (n != 0)
        memcpy(out, sNet.drops, (size_t)n * sizeof(struct NetDrop));
    SDL_UnlockMutex(sNet.lock);
    return n;
}

bool8 Net_PopPickedUp(u16 *item, u16 *quantity)
{
    if (!sInitialised || item == NULL || quantity == NULL)
        return FALSE;

    SDL_LockMutex(sNet.lock);
    if (!sNet.hasPicked)
    {
        SDL_UnlockMutex(sNet.lock);
        return FALSE;
    }
    *item = sNet.pickedItem;
    *quantity = sNet.pickedQuantity;
    sNet.hasPicked = FALSE;
    SDL_UnlockMutex(sNet.lock);
    return TRUE;
}

bool8 Net_WasOnline(void)
{
    bool8 was;

    if (!sInitialised)
        return FALSE;

    SDL_LockMutex(sNet.lock);
    was = sNet.wasOnline;
    SDL_UnlockMutex(sNet.lock);
    return was;
}

bool8 Net_PopLinkBlock(struct NetLinkBlock *out)
{
    if (!sInitialised || out == NULL)
        return FALSE;

    SDL_LockMutex(sNet.lock);
    if (sNet.blockCount == 0)
    {
        SDL_UnlockMutex(sNet.lock);
        return FALSE;
    }
    *out = sNet.blockInbox[sNet.blockTail];
    sNet.blockTail = (sNet.blockTail + 1) % LINK_BLOCK_INBOX;
    sNet.blockCount--;
    SDL_UnlockMutex(sNet.lock);
    return TRUE;
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

bool8 Net_PopBattleStart(struct NetBattleStart *out)
{
    if (!sInitialised || out == NULL)
        return FALSE;

    SDL_LockMutex(sNet.lock);
    if (!sNet.hasBattleStart)
    {
        SDL_UnlockMutex(sNet.lock);
        return FALSE;
    }
    *out = sNet.battleStart;
    sNet.hasBattleStart = FALSE;
    SDL_UnlockMutex(sNet.lock);
    return TRUE;
}

bool8 Net_PopCorrection(struct NetCorrection *out)
{
    if (!sInitialised || out == NULL)
        return FALSE;

    SDL_LockMutex(sNet.lock);
    if (!sNet.hasCorrection)
    {
        SDL_UnlockMutex(sNet.lock);
        return FALSE;
    }
    *out = sNet.correction;
    sNet.hasCorrection = FALSE;
    SDL_UnlockMutex(sNet.lock);
    return TRUE;
}

bool8 Net_ServerSaveDecided(void)
{
    bool8 decided;

    SDL_LockMutex(sNet.lock);
    decided = sNet.saveDecided;
    SDL_UnlockMutex(sNet.lock);
    return decided;
}

bool8 Net_HasServerSave(void)
{
    bool8 has;

    if (!sInitialised)
        return FALSE;

    SDL_LockMutex(sNet.lock);
    has = sNet.hasServerSave;
    SDL_UnlockMutex(sNet.lock);
    return has;
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
