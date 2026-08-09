// Every case of "where is this chat line going", checked on the host in about a second.
//
// Compiles the game's parser directly rather than a copy of it, so this cannot drift away
// from what actually ships. Run it with tools/debug/test-chat-parse.sh.

#include "mmo_chat_parse.h"
#include <stdio.h>
#include <string.h>

static int sFailures;

static const char *ScopeName(unsigned char scope)
{
    switch (scope)
    {
    case MMO_CHAT_SCOPE_GLOBAL:
        return "global";
    case MMO_CHAT_SCOPE_LOCAL:
        return "local";
    case MMO_CHAT_SCOPE_PRIVATE:
        return "private";
    case MMO_CHAT_SCOPE_UNRESOLVED:
        return "unresolved";
    default:
        return "?";
    }
}

// `replyTo` is who last whispered; "" when nobody has.
static void Check(const char *typed, const char *replyTo, unsigned char wantScope,
                  const char *wantTarget, const char *wantBody)
{
    char target[24];
    const char *body = NULL;
    unsigned char scope = MmoChat_ParseCompose(typed, replyTo, target, sizeof(target), &body);
    int ok = scope == wantScope && strcmp(target, wantTarget) == 0
          && body != NULL && strcmp(body, wantBody) == 0;

    if (!ok)
    {
        sFailures++;
        printf("FAIL  \"%s\"\n", typed);
        printf("      want %s to=[%s] body=[%s]\n", ScopeName(wantScope), wantTarget, wantBody);
        printf("      got  %s to=[%s] body=[%s]\n", ScopeName(scope), target,
               body ? body : "(null)");
    }
    else
    {
        printf("ok    %-26s -> %-10s to=[%s] body=[%s]\n", typed, ScopeName(scope), target, body);
    }
}

int main(void)
{
    // Plain text is global, which is what most messages are.
    Check("hello there", "", MMO_CHAT_SCOPE_GLOBAL, "", "hello there");
    Check("", "", MMO_CHAT_SCOPE_GLOBAL, "", "");

    // Global can also be asked for explicitly.
    Check("/g hi all", "", MMO_CHAT_SCOPE_GLOBAL, "", "hi all");
    Check("/global hi all", "", MMO_CHAT_SCOPE_GLOBAL, "", "hi all");

    // Local, under any of its names.
    Check("/s hi here", "", MMO_CHAT_SCOPE_LOCAL, "", "hi here");
    Check("/say hi here", "", MMO_CHAT_SCOPE_LOCAL, "", "hi here");
    Check("/l hi here", "", MMO_CHAT_SCOPE_LOCAL, "", "hi here");
    Check("/local hi here", "", MMO_CHAT_SCOPE_LOCAL, "", "hi here");

    // Private takes the next word as the recipient.
    Check("/w bob secret", "", MMO_CHAT_SCOPE_PRIVATE, "bob", "secret");
    Check("/whisper bob secret", "", MMO_CHAT_SCOPE_PRIVATE, "bob", "secret");
    Check("/msg bob secret", "", MMO_CHAT_SCOPE_PRIVATE, "bob", "secret");
    Check("/tell bob secret", "", MMO_CHAT_SCOPE_PRIVATE, "bob", "secret");

    // Commands are recognised whatever the case; names keep theirs, since they are matched
    // against real account names on the server.
    Check("/W Bob Hi", "", MMO_CHAT_SCOPE_PRIVATE, "Bob", "Hi");

    // Extra spaces anywhere are not a syntax error.
    Check("/w   bob   hi  there", "", MMO_CHAT_SCOPE_PRIVATE, "bob", "hi  there");

    // The dangerous cases: a private message with nobody to send it to must never fall back
    // to global. Dropped is the only safe answer.
    Check("/w ", "", MMO_CHAT_SCOPE_UNRESOLVED, "", "");
    Check("/w", "", MMO_CHAT_SCOPE_UNRESOLVED, "", "");
    Check("/r nobody has whispered", "", MMO_CHAT_SCOPE_UNRESOLVED, "", "nobody has whispered");
    Check("/reply nobody", "", MMO_CHAT_SCOPE_UNRESOLVED, "", "nobody");

    // Reply goes to whoever whispered last.
    Check("/r sure thing", "amy", MMO_CHAT_SCOPE_PRIVATE, "amy", "sure thing");
    Check("/reply sure thing", "amy", MMO_CHAT_SCOPE_PRIVATE, "amy", "sure thing");

    // A whisper with a name and no message: the scope resolves, and the caller drops it for
    // being empty rather than sending a blank line.
    Check("/w bob", "", MMO_CHAT_SCOPE_PRIVATE, "bob", "");

    // A command has to be a whole word, so these are ordinary messages.
    Check("/gg wat", "", MMO_CHAT_SCOPE_GLOBAL, "", "/gg wat");
    Check("/sneak attack", "", MMO_CHAT_SCOPE_GLOBAL, "", "/sneak attack");
    Check("/rest in peace", "", MMO_CHAT_SCOPE_GLOBAL, "", "/rest in peace");
    Check("/wow", "", MMO_CHAT_SCOPE_GLOBAL, "", "/wow");

    // An unknown command is sent as typed rather than swallowed.
    Check("/dance", "", MMO_CHAT_SCOPE_GLOBAL, "", "/dance");

    // A name too long for the buffer must not have its tail read as the message, which
    // would whisper to a truncated name and send text nobody expected.
    Check("/w abcdefghijklmnopqrstuvwxyz hi", "", MMO_CHAT_SCOPE_PRIVATE,
          "abcdefghijklmnopqrstuvw", "hi");

    // A slash on its own is a message, not a command.
    Check("/", "", MMO_CHAT_SCOPE_GLOBAL, "", "/");

    if (sFailures != 0)
    {
        printf("\n%d failure(s)\n", sFailures);
        return 1;
    }
    printf("\nall chat scope cases pass\n");
    return 0;
}
