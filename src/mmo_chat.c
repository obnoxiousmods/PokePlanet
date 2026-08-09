// Chat, as seen from the overworld.
//
// The transport for this already existed: the server routes global, per-map and private
// messages and bridges global to IRC, and net_client buffers arriving lines. What was
// missing was any way to see them.
//
// Messages appear in a small window at the top of the screen and fade after a few seconds,
// the way the map name popup does, rather than in a box that has to be dismissed. Chat has
// to be readable *while walking*; a message that stops the player is a message they will
// come to resent.
//
// VRAM. The overworld commits all four background layers, so this shares BG0 with the
// dialogue box and needs a tile range of its own. BG0's windows sit at 0x8 (start menu),
// 0x107 (map name popup), 0x125 (yes/no) and 0x194 (the dialogue box), which leaves 0x139
// through 0x193 free -- 91 tiles. This window is 26x3, or 78.

#include "global.h"
#include "main.h"
#include "menu.h"
#include "mmo_chat.h"
#include "mmo_chat_parse.h"
#include "mmo_text.h"
#include "net_client.h"
#include "field_player_avatar.h"
#include "script.h"
#include "string_util.h"
#include "text.h"
#include "platform.h"
#include "window.h"
#include "constants/characters.h"

#define CHAT_WINDOW_BASE_BLOCK 0x139
#define CHAT_WINDOW_WIDTH      26
#define CHAT_WINDOW_HEIGHT     3

// Long enough to read a line, short enough that the screen is mostly clear.
#define CHAT_VISIBLE_FRAMES 240

static const struct WindowTemplate sChatWindowTemplate =
{
    .bg = 0,
    .tilemapLeft = 2,
    .tilemapTop = 1,
    .width = CHAT_WINDOW_WIDTH,
    .height = CHAT_WINDOW_HEIGHT,
    .paletteNum = 15,
    .baseBlock = CHAT_WINDOW_BASE_BLOCK,
};

static u8 sChatWindowId = WINDOW_NONE;
static u16 sFramesLeft;
static bool8 sComposing;

// Comfortably longer than anything that fits the window, but bounded: the wire caps a
// message at NET_TEXT_LEN and the server will not carry more.
#define TEXT_INPUT_LIMIT 96

// The prompt names the scope, so it is never a guess where a message is about to go.
static const u8 sSayPrompt[] = _("Say: ");
static const u8 sNearbyPrompt[] = _("Nearby: ");
static const u8 sToPrompt[] = _("To ");
static const u8 sToPromptEnd[] = _(": ");

// Tags on arriving lines, for the same reason: a whisper must not read like something the
// whole server saw. Global is untagged, being both the default and the common case.
static const u8 sNearbyTag[] = _(" nearby");
static const u8 sWhispersTag[] = _(" whispers");

// The parser lives in mmo_chat_parse.c so it can be tested on the host; see that header.
// Its scope values are written to match the wire's, and this is where that is checked
// rather than trusted -- a silent mismatch would send private messages to everyone.
STATIC_ASSERT(MMO_CHAT_SCOPE_GLOBAL == NET_CHAT_GLOBAL, ChatScopeGlobalAgrees);
STATIC_ASSERT(MMO_CHAT_SCOPE_LOCAL == NET_CHAT_LOCAL, ChatScopeLocalAgrees);
STATIC_ASSERT(MMO_CHAT_SCOPE_PRIVATE == NET_CHAT_PRIVATE, ChatScopePrivateAgrees);

// Who last whispered, so /r can answer without retyping the name.
static char sLastWhisper[NET_SENDER_LEN];

static void HideChatWindow(void)
{
    if (sChatWindowId == WINDOW_NONE)
        return;

    ClearStdWindowAndFrame(sChatWindowId, TRUE);
    RemoveWindow(sChatWindowId);
    sChatWindowId = WINDOW_NONE;
    sFramesLeft = 0;
}

// Drop the window without touching VRAM. For a map change, where every window is torn down
// underneath us and clearing one we no longer own would corrupt whatever took its place.
void MmoChat_Reset(void)
{
    sChatWindowId = WINDOW_NONE;
    sFramesLeft = 0;
}

static void ShowLine(const struct NetChatLine *line)
{
    u8 text[NET_SENDER_LEN + NET_TEXT_LEN + 16];
    u8 encoded[NET_TEXT_LEN + 1];
    u8 *end;

    if (sChatWindowId == WINDOW_NONE)
    {
        sChatWindowId = AddWindow(&sChatWindowTemplate);
        if (sChatWindowId == WINDOW_NONE)
            return; // Every window slot is taken; the line is simply not shown.
        LoadMessageBoxAndBorderGfx();
    }

    // "Name: what they said", both converted from the server's ASCII, with the scope named
    // unless it is global.
    MmoText_FromAscii(encoded, line->from, NET_SENDER_LEN + 1);
    end = StringCopy(text, encoded);
    if (line->kind == NET_CHAT_LOCAL)
        end = StringCopy(end, sNearbyTag);
    else if (line->kind == NET_CHAT_PRIVATE)
        end = StringCopy(end, sWhispersTag);
    *end++ = CHAR_COLON;
    *end++ = CHAR_SPACE;
    MmoText_FromAscii(encoded, line->text, sizeof(encoded));
    StringCopy(end, encoded);

    DrawStdWindowFrame(sChatWindowId, FALSE);
    FillWindowPixelBuffer(sChatWindowId, PIXEL_FILL(1));
    AddTextPrinterParameterized(sChatWindowId, FONT_NARROW, text, 0, 1, TEXT_SKIP_DRAW, NULL);
    CopyWindowToVram(sChatWindowId, COPYWIN_FULL);
    sFramesLeft = CHAT_VISIBLE_FRAMES;

    if (line->kind == NET_CHAT_PRIVATE)
    {
        u32 i;

        for (i = 0; i + 1 < sizeof(sLastWhisper) && line->from[i] != '\0'; i++)
            sLastWhisper[i] = line->from[i];
        sLastWhisper[i] = '\0';
    }
}

// Draw what is being typed, with a caret, so it is obvious the game is listening.
static void ShowComposer(const char *typed)
{
    u8 text[TEXT_INPUT_LIMIT + NET_SENDER_LEN + 16];
    u8 encoded[TEXT_INPUT_LIMIT + 1];
    char target[NET_SENDER_LEN];
    const char *body;
    u8 scope;
    u8 *end;

    if (sChatWindowId == WINDOW_NONE)
    {
        sChatWindowId = AddWindow(&sChatWindowTemplate);
        if (sChatWindowId == WINDOW_NONE)
            return;
        LoadMessageBoxAndBorderGfx();
    }

    // Reparsed on every keystroke so the prompt follows what is being typed: the moment a
    // name completes, "Say:" becomes "To Bob:" and there is no doubt about who will read it.
    scope = MmoChat_ParseCompose(typed, sLastWhisper, target, sizeof(target), &body);
    if (scope == NET_CHAT_LOCAL)
    {
        end = StringCopy(text, sNearbyPrompt);
    }
    else if (scope == NET_CHAT_PRIVATE)
    {
        end = StringCopy(text, sToPrompt);
        MmoText_FromAscii(encoded, target, sizeof(target));
        end = StringCopy(end, encoded);
        end = StringCopy(end, sToPromptEnd);
    }
    else
    {
        end = StringCopy(text, sSayPrompt);
    }

    // The command itself is not echoed back; what is shown is the message as it will be
    // sent, under a prompt that says where it is going.
    MmoText_FromAscii(encoded, body, sizeof(encoded));
    end = StringCopy(end, encoded);
    *end++ = CHAR_UNDERSCORE;
    *end = EOS;

    DrawStdWindowFrame(sChatWindowId, FALSE);
    FillWindowPixelBuffer(sChatWindowId, PIXEL_FILL(1));
    AddTextPrinterParameterized(sChatWindowId, FONT_NARROW, text, 0, 1, TEXT_SKIP_DRAW, NULL);
    CopyWindowToVram(sChatWindowId, COPYWIN_FULL);
    sFramesLeft = 0; // Stays up until the player is done.
}

// Ticked once per overworld frame.
void MmoChat_Update(void)
{
    struct NetChatLine line;

    if (!Net_IsLinked() || Net_GetAuthState() != NET_AUTH_ONLINE)
    {
        if (sComposing)
        {
            // Went offline mid-sentence; there is nowhere to send it.
            Platform_EndTextInput();
            sComposing = FALSE;
            UnlockPlayerFieldControls();
            HideChatWindow();
        }
        return;
    }

    if (sComposing)
    {
        char typed[TEXT_INPUT_LIMIT];
        u8 result = Platform_PollTextInput(typed, sizeof(typed));

        if (result == 0)
        {
            ShowComposer(typed);
            return;
        }

        Platform_EndTextInput();
        sComposing = FALSE;
        UnlockPlayerFieldControls();

        if (result == 1)
        {
            char target[NET_SENDER_LEN];
            const char *body;
            u8 scope = MmoChat_ParseCompose(typed, sLastWhisper, target,
                                            sizeof(target), &body);

            // A whisper with nobody to whisper to is dropped rather than broadcast.
            if (scope != MMO_CHAT_SCOPE_UNRESOLVED && body[0] != '\0')
                Net_SendChat(scope, target, body);
        }

        HideChatWindow();
        return;
    }

    // A script owns the screen while it runs, and the dialogue box shares this layer.
    // Lines that arrive meanwhile stay queued in net_client until the field is clear.
    if (ScriptContext_IsEnabled() || ArePlayerFieldControlsLocked())
    {
        HideChatWindow();
        return;
    }

    // R opens the composer. The field is locked while typing so the player does not walk
    // off mid-sentence, and the platform stops reporting buttons at all.
    if (JOY_NEW(R_BUTTON))
    {
        sComposing = TRUE;
        LockPlayerFieldControls();
        Platform_BeginTextInput();
        ShowComposer("");
        return;
    }

    if (Net_PopChatLine(&line))
    {
        ShowLine(&line);
        return;
    }

    if (sFramesLeft != 0 && --sFramesLeft == 0)
        HideChatWindow();
}
