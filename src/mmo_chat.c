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
#include "menu.h"
#include "mmo_chat.h"
#include "mmo_text.h"
#include "net_client.h"
#include "script.h"
#include "string_util.h"
#include "text.h"
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
    u8 text[NET_SENDER_LEN + NET_TEXT_LEN + 4];
    u8 encoded[NET_TEXT_LEN + 1];
    u8 *end;

    if (sChatWindowId == WINDOW_NONE)
    {
        sChatWindowId = AddWindow(&sChatWindowTemplate);
        if (sChatWindowId == WINDOW_NONE)
            return; // Every window slot is taken; the line is simply not shown.
        LoadMessageBoxAndBorderGfx();
    }

    // "Name: what they said", both converted from the server's ASCII.
    MmoText_FromAscii(encoded, line->from, NET_SENDER_LEN + 1);
    end = StringCopy(text, encoded);
    *end++ = CHAR_COLON;
    *end++ = CHAR_SPACE;
    MmoText_FromAscii(encoded, line->text, sizeof(encoded));
    StringCopy(end, encoded);

    DrawStdWindowFrame(sChatWindowId, FALSE);
    FillWindowPixelBuffer(sChatWindowId, PIXEL_FILL(1));
    AddTextPrinterParameterized(sChatWindowId, FONT_NARROW, text, 0, 1, TEXT_SKIP_DRAW, NULL);
    CopyWindowToVram(sChatWindowId, COPYWIN_FULL);
    sFramesLeft = CHAT_VISIBLE_FRAMES;
}

// Ticked once per overworld frame.
void MmoChat_Update(void)
{
    struct NetChatLine line;

    if (!Net_IsLinked() || Net_GetAuthState() != NET_AUTH_ONLINE)
        return;

    // A script owns the screen while it runs, and the dialogue box shares this layer.
    // Lines that arrive meanwhile stay queued in net_client until the field is clear.
    if (ScriptContext_IsEnabled() || ArePlayerFieldControlsLocked())
    {
        HideChatWindow();
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
