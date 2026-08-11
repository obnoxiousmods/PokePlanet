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
// In a battle the overworld base 0x139 collides with the standard battle window at 0x16e, so chat
// uses its own base there. 0x1F8..0x290 is verified free in the standard battle window set (the
// only set the MMO's battles use), with room for the incoming window's 78 tiles.
#define CHAT_WINDOW_BATTLE_BASE 0x1F8
#define CHAT_WINDOW_WIDTH      26

// Long enough to read a line, short enough that the screen is mostly clear.
#define CHAT_VISIBLE_FRAMES 240

// Incoming lines and the composer are two different things and no longer share a spot, which was
// the whole of the complaint: what you typed appeared exactly where the messages you were reading
// did. Now arriving messages sit at the top and the composer sits at the bottom, out of the way of
// them. Only one is ever on screen at once -- lines are queued while you type -- so both windows
// use the same VRAM tiles and are torn down and rebuilt when the mode changes; the incoming height
// of three is the larger, so the shared base block is sized for it and the free BG0 range holds it.
enum { CHAT_WINDOW_NONE, CHAT_WINDOW_INCOMING, CHAT_WINDOW_COMPOSER };

static const struct WindowTemplate sIncomingWindowTemplate =
{
    .bg = 0,
    .tilemapLeft = 2,
    .tilemapTop = 1,
    .width = CHAT_WINDOW_WIDTH,
    .height = 3,
    .paletteNum = 15,
    .baseBlock = CHAT_WINDOW_BASE_BLOCK,
};

static const struct WindowTemplate sComposerWindowTemplate =
{
    .bg = 0,
    .tilemapLeft = 2,
    .tilemapTop = 15,
    .width = CHAT_WINDOW_WIDTH,
    .height = 2,
    .paletteNum = 15,
    .baseBlock = CHAT_WINDOW_BASE_BLOCK,
};

static u8 sChatWindowId = WINDOW_NONE;
static u8 sChatWindowKind = CHAT_WINDOW_NONE;
static u16 sFramesLeft;
static bool8 sComposing;
static bool8 sWelcomed;

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

// Shown once on entering the world. The channel is #pokeplanet, but the game font has no '#', so
// the name is spelled plainly; the point of the line is the key, which nothing else advertises.
static const u8 sWelcomeText[] = _("Welcome! Shift+Enter to chat");

// The parser lives in mmo_chat_parse.c so it can be tested on the host; see that header.
// Its scope values are written to match the wire's, and this is where that is checked
// rather than trusted -- a silent mismatch would send private messages to everyone.
STATIC_ASSERT(MMO_CHAT_SCOPE_GLOBAL == NET_CHAT_GLOBAL, ChatScopeGlobalAgrees);
STATIC_ASSERT(MMO_CHAT_SCOPE_LOCAL == NET_CHAT_LOCAL, ChatScopeLocalAgrees);
STATIC_ASSERT(MMO_CHAT_SCOPE_PRIVATE == NET_CHAT_PRIVATE, ChatScopePrivateAgrees);

// Who last whispered, so /r can answer without retyping the name.
static char sLastWhisper[NET_SENDER_LEN];

// Tear down the current window's on-screen tiles. In a battle the window has no std frame (see
// EnsureChatWindow), so clearing one -- which reads the shared border gfx the battle owns -- would
// corrupt the battle's own frames; clear only the window's own tilemap there instead.
static void ClearChatTiles(void)
{
    if (gMain.inBattle)
    {
        ClearWindowTilemap(sChatWindowId);
        CopyWindowToVram(sChatWindowId, COPYWIN_MAP);
    }
    else
    {
        ClearStdWindowAndFrame(sChatWindowId, TRUE);
    }
}

static void HideChatWindow(void)
{
    if (sChatWindowId == WINDOW_NONE)
        return;

    ClearChatTiles();
    RemoveWindow(sChatWindowId);
    sChatWindowId = WINDOW_NONE;
    sChatWindowKind = CHAT_WINDOW_NONE;
    sFramesLeft = 0;
}

// Make sure the window on screen is the one this `kind` needs -- top-of-screen for incoming lines,
// bottom for the composer -- creating it, or tearing down and rebuilding it if the other kind is
// currently up. Returns FALSE only when no window slot is free, in which case the caller simply
// does not draw. Loads the shared message-box border the first time a window is made.
static bool8 EnsureChatWindow(u8 kind)
{
    if (sChatWindowKind == kind && sChatWindowId != WINDOW_NONE)
        return TRUE;

    if (sChatWindowId != WINDOW_NONE)
    {
        ClearChatTiles();
        RemoveWindow(sChatWindowId);
        sChatWindowId = WINDOW_NONE;
        sChatWindowKind = CHAT_WINDOW_NONE;
    }

    // In a battle, move the window off the overworld base (which collides with battle windows) and
    // render frameless -- the std frame's border gfx would overwrite the battle's own frames.
    {
        struct WindowTemplate template = (kind == CHAT_WINDOW_COMPOSER)
                                             ? sComposerWindowTemplate
                                             : sIncomingWindowTemplate;
        if (gMain.inBattle)
            template.baseBlock = CHAT_WINDOW_BATTLE_BASE;
        sChatWindowId = AddWindow(&template);
    }
    if (sChatWindowId == WINDOW_NONE)
        return FALSE; // Every window slot is taken; the line is simply not shown.

    sChatWindowKind = kind;
    if (!gMain.inBattle)
        LoadMessageBoxAndBorderGfx();
    return TRUE;
}

// Drop the window without touching VRAM. For a map change, where every window is torn down
// underneath us and clearing one we no longer own would corrupt whatever took its place.
void MmoChat_Reset(void)
{
    sChatWindowId = WINDOW_NONE;
    sChatWindowKind = CHAT_WINDOW_NONE;
    sFramesLeft = 0;
}

static void ShowLine(const struct NetChatLine *line)
{
    u8 text[NET_SENDER_LEN + NET_TEXT_LEN + 16];
    u8 encoded[NET_TEXT_LEN + 1];
    u8 *end;

    if (!EnsureChatWindow(CHAT_WINDOW_INCOMING))
        return;

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

    // No std frame in a battle: its border gfx would land on tiles the battle's own frames use.
    // The pixel-fill below still gives the text a solid backing, so it stays readable frameless.
    if (!gMain.inBattle)
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

    if (!EnsureChatWindow(CHAT_WINDOW_COMPOSER))
        return;

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

    // No std frame in a battle: its border gfx would land on tiles the battle's own frames use.
    // The pixel-fill below still gives the text a solid backing, so it stays readable frameless.
    if (!gMain.inBattle)
        DrawStdWindowFrame(sChatWindowId, FALSE);
    FillWindowPixelBuffer(sChatWindowId, PIXEL_FILL(1));
    AddTextPrinterParameterized(sChatWindowId, FONT_NARROW, text, 0, 1, TEXT_SKIP_DRAW, NULL);
    CopyWindowToVram(sChatWindowId, COPYWIN_FULL);
    sFramesLeft = 0; // Stays up until the player is done.
}

// A one-off greeting in the incoming-message spot, fading like any other line. It exists purely
// so a new player learns chat is here and which key reaches it.
static void ShowWelcome(void)
{
    if (!EnsureChatWindow(CHAT_WINDOW_INCOMING))
        return;

    // No std frame in a battle: its border gfx would land on tiles the battle's own frames use.
    // The pixel-fill below still gives the text a solid backing, so it stays readable frameless.
    if (!gMain.inBattle)
        DrawStdWindowFrame(sChatWindowId, FALSE);
    FillWindowPixelBuffer(sChatWindowId, PIXEL_FILL(1));
    AddTextPrinterParameterized(sChatWindowId, FONT_NARROW, sWelcomeText, 0, 1, TEXT_SKIP_DRAW,
                                NULL);
    CopyWindowToVram(sChatWindowId, COPYWIN_FULL);
    sFramesLeft = CHAT_VISIBLE_FRAMES;
}

// Ticked once per frame, in the overworld and in a battle.
void MmoChat_Update(void)
{
    // Crossing into or out of a battle tears down every window this owns, along with the rest of
    // the field's. Forget the id on that boundary rather than reuse a freed one -- which is the
    // window equivalent of a dangling pointer, and would draw chat into whatever took its slot.
    static bool8 sWasInBattle = FALSE;
    struct NetChatLine line;

    if (gMain.inBattle != sWasInBattle)
    {
        MmoChat_Reset();
        sWasInBattle = gMain.inBattle;
    }

    if (!Net_IsLinked() || Net_GetAuthState() != NET_AUTH_ONLINE)
    {
        if (sComposing)
        {
            // Went offline mid-sentence; there is nowhere to send it.
            Platform_EndTextInput();
            sComposing = FALSE;
            if (!gMain.inBattle)
                UnlockPlayerFieldControls();
            HideChatWindow();
        }
        // Greet again after a reconnect, so the reminder is there for a fresh session.
        sWelcomed = FALSE;
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
        // Not in a battle: there the battle holds the field lock, and releasing it here (without
        // having taken it -- see the open below) would hand control back mid-battle.
        if (!gMain.inBattle)
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
    //
    // In a battle the field controls are always locked -- that is not a script, it is just how a
    // battle holds the field -- so that condition is skipped there, or chat could never open in a
    // battle at all. The battle has its own base and renders frameless, so it is safe to draw.
    if (!gMain.inBattle && (ScriptContext_IsEnabled() || ArePlayerFieldControlsLocked()))
    {
        HideChatWindow();
        return;
    }

    // The first clear moment online, tell the player chat exists and how to reach it. Nothing
    // else on screen says so, and a chat nobody knows is there may as well not be.
    if (!sWelcomed)
    {
        sWelcomed = TRUE;
        ShowWelcome();
        return;
    }

    // Shift+Enter opens the composer. The field is locked while typing so the player does not
    // walk off mid-sentence, and the platform stops reporting buttons at all. Shift+Enter rather
    // than a face button so it is the same reach on a keyboard as any chat, and off the START key.
    if (Platform_ConsumeChatOpen())
    {
        sComposing = TRUE;
        // In a battle the field is already the battle's to hold, so do not take it here (and so
        // do not release it on close). Platform_BeginTextInput stops button input either way, so
        // the battle receives nothing while the player types -- the move waits until they finish.
        if (!gMain.inBattle)
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
