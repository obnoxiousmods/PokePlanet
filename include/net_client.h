#ifndef GUARD_NET_CLIENT_H
#define GUARD_NET_CLIENT_H

// Link to the PokePlanet network sidecar (pokeplanet-net.exe).
//
// The sidecar owns QUIC, TLS and the Discord login. This side owns only a loopback
// socket and a fixed-layout message format, so the game binary stays free of network
// dependencies beyond winsock.
//
// Everything here is safe to call from game code on the AgbMain thread; the socket runs
// on its own thread and hands over data behind a mutex.
//
// Keep the constants below in sync with server/proto/src/ipc.rs.

#include "global.h"

#define NET_NAME_LEN     16
#define NET_SENDER_LEN   24
#define NET_TEXT_LEN     200
#define NET_URL_LEN      192

// The game can only draw a few remote players: OBJECT_EVENTS_COUNT bounds live object
// events and MAX_SPRITES bounds OAM entries, both shared with the map's own NPCs.
#define NET_MAX_REMOTE_PLAYERS 8

enum NetAuthState
{
    NET_AUTH_OFFLINE,
    NET_AUTH_CONNECTING,
    NET_AUTH_NEEDS_LOGIN,
    NET_AUTH_AWAITING_BROWSER,
    NET_AUTH_ONLINE,
    // This character signed in elsewhere. The world has moved on without us, so the game
    // closes rather than sitting there looking connected.
    NET_AUTH_SUPERSEDED,
};

enum NetChatKind
{
    NET_CHAT_GLOBAL,
    NET_CHAT_LOCAL,
    NET_CHAT_PRIVATE,
};

struct NetRemotePlayer
{
    u32 playerId;
    u8 mapGroup;
    u8 mapNum;
    s16 x;
    s16 y;
    u8 facing;
    u8 graphicsId;
    u8 elevation;
    bool8 moving;
    char name[NET_NAME_LEN];
};

struct NetChatLine
{
    u8 kind;
    char from[NET_SENDER_LEN];
    char text[NET_TEXT_LEN];
};

// The authoritative save summary, as held by the server. The client displays this
// rather than reading progress from any local file.
struct NetProfile
{
    char name[NET_NAME_LEN];
    u8 graphicsId;
    u8 badges;
    u16 pokedexCaught;
    u16 pokedexSeen;
    u32 playTimeSeconds;
    u32 money;
};

// Start the socket thread. Safe to call more than once; later calls do nothing.
void Net_Init(void);
void Net_Shutdown(void);

// True once the sidecar has accepted our connection.
bool8 Net_IsLinked(void);
u8 Net_GetAuthState(void);

// Display name once signed in, otherwise an empty string. Never NULL.
const char *Net_GetPlayerName(void);
// The Discord URL to visit, when the state is NEEDS_LOGIN or AWAITING_BROWSER.
const char *Net_GetLoginUrl(void);

// Copy the server's save summary into `out`. Returns FALSE until one has arrived.
bool8 Net_GetProfile(struct NetProfile *out);

void Net_BeginLogin(void);
void Net_CancelLogin(void);
void Net_Logout(void);

// Report where this player is standing. Cheap: rate limiting happens internally, so
// callers may invoke it every frame.
void Net_SendSelf(u8 mapGroup, u8 mapNum, s16 x, s16 y, u8 facing, bool8 moving,
                  u8 graphicsId, u8 elevation);

// Copy the most recent snapshot into `out`, returning how many entries were written.
// `out` must have room for NET_MAX_REMOTE_PLAYERS.
u8 Net_GetRemotePlayers(struct NetRemotePlayer *out);

// Challenge another player to a battle. The server refuses the request if they are
// offline, on another map, or already holding an invitation.
void Net_RequestBattle(u32 playerId);
// Answer a challenge someone sent us.
void Net_RespondToBattle(u32 playerId, bool8 accepted);

void Net_SendChat(u8 kind, const char *target, const char *text);
// Pop the oldest unread chat line. Returns FALSE when nothing is queued.
bool8 Net_PopChatLine(struct NetChatLine *out);

#endif // GUARD_NET_CLIENT_H
