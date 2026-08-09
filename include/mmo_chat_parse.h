#ifndef GUARD_MMO_CHAT_PARSE_H
#define GUARD_MMO_CHAT_PARSE_H

// Working out where a typed chat line is meant to go.
//
// Split out from mmo_chat.c and written against plain C types on purpose: this is the one
// part of chat that is pure logic with a great many cases, and keeping it free of game
// headers lets tools/debug/test-chat-parse.c compile it on the host and check every one of
// them in a second. Sending a private message to the wrong audience is not a bug that
// should be found by a player.

// Scopes, matching the NET_CHAT_* wire values. mmo_chat.c asserts they still agree.
#define MMO_CHAT_SCOPE_GLOBAL 0
#define MMO_CHAT_SCOPE_LOCAL 1
#define MMO_CHAT_SCOPE_PRIVATE 2

// A line whose scope cannot be resolved -- a whisper with nobody to whisper to. Deliberately
// not a value the wire accepts: the caller must drop it rather than send it somewhere.
#define MMO_CHAT_SCOPE_UNRESOLVED 0xFF

// Work out where `typed` is meant to go.
//
// `replyTo` is whoever last whispered, used by /r; pass "" if nobody has. `target` receives
// the recipient for a private message and is emptied otherwise. `*body` is set to the
// message with any command stripped, pointing into `typed`.
unsigned char MmoChat_ParseCompose(const char *typed, const char *replyTo, char *target,
                                   unsigned int targetSize, const char **body);

#endif // GUARD_MMO_CHAT_PARSE_H
