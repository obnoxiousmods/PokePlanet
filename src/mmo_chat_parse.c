// Where a typed chat line is meant to go. See mmo_chat_parse.h for why this is on its own.

#include "mmo_chat_parse.h"
#include <stddef.h>

static const char *SkipSpaces(const char *s)
{
    while (*s == ' ')
        s++;
    return s;
}

// Match a leading command word, returning what follows it, or NULL if it is not this one.
//
// The word has to end at a space or the end of the line, so "/global" is not read as "/g"
// followed by a message of "lobal".
static const char *MatchCommand(const char *s, const char *word)
{
    while (*word != '\0')
    {
        char c = *s;

        if (c >= 'A' && c <= 'Z')
            c += 'a' - 'A';
        if (c != *word)
            return NULL;
        s++;
        word++;
    }

    if (*s != '\0' && *s != ' ')
        return NULL;
    return SkipSpaces(s);
}

// Copy a word into `target`, stopping at a space, and return what follows it.
static const char *TakeWord(const char *s, char *target, unsigned int targetSize)
{
    unsigned int i = 0;

    while (*s != '\0' && *s != ' ' && i + 1 < targetSize)
        target[i++] = *s++;
    target[i] = '\0';

    // A name longer than the buffer would otherwise be truncated and the remainder read as
    // the message, quietly whispering to the wrong person. Skip the rest of the word so at
    // worst the name is wrong in a way the prompt shows before anything is sent.
    while (*s != '\0' && *s != ' ')
        s++;
    return SkipSpaces(s);
}

unsigned char MmoChat_ParseCompose(const char *typed, const char *replyTo, char *target,
                                   unsigned int targetSize, const char **body)
{
    const char *rest;

    target[0] = '\0';
    *body = typed;

    if (*typed != '/')
        return MMO_CHAT_SCOPE_GLOBAL;

    if ((rest = MatchCommand(typed, "/g")) != NULL
     || (rest = MatchCommand(typed, "/global")) != NULL)
    {
        *body = rest;
        return MMO_CHAT_SCOPE_GLOBAL;
    }

    if ((rest = MatchCommand(typed, "/s")) != NULL
     || (rest = MatchCommand(typed, "/say")) != NULL
     || (rest = MatchCommand(typed, "/l")) != NULL
     || (rest = MatchCommand(typed, "/local")) != NULL)
    {
        *body = rest;
        return MMO_CHAT_SCOPE_LOCAL;
    }

    if ((rest = MatchCommand(typed, "/w")) != NULL
     || (rest = MatchCommand(typed, "/whisper")) != NULL
     || (rest = MatchCommand(typed, "/msg")) != NULL
     || (rest = MatchCommand(typed, "/tell")) != NULL)
    {
        *body = TakeWord(rest, target, targetSize);
        // No name means there is nobody to whisper to. Falling back to global here would be
        // the friendlier-looking choice and a genuinely harmful one: someone who types "/w "
        // and fumbles the name would have a message they believed was private announced to
        // the entire server, with no way to take it back.
        return target[0] != '\0' ? MMO_CHAT_SCOPE_PRIVATE : MMO_CHAT_SCOPE_UNRESOLVED;
    }

    if ((rest = MatchCommand(typed, "/r")) != NULL
     || (rest = MatchCommand(typed, "/reply")) != NULL)
    {
        unsigned int i = 0;

        while (replyTo[i] != '\0' && i + 1 < targetSize)
        {
            target[i] = replyTo[i];
            i++;
        }
        target[i] = '\0';
        *body = rest;
        // Nobody has whispered yet, so the same reasoning applies as above.
        return target[0] != '\0' ? MMO_CHAT_SCOPE_PRIVATE : MMO_CHAT_SCOPE_UNRESOLVED;
    }

    // Not a command this build knows. Send it as typed rather than swallowing it: a line
    // opening with a slash is far more likely to be a message than a mistake.
    return MMO_CHAT_SCOPE_GLOBAL;
}
