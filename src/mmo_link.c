// The link cable, as a network connection.
//
// A GBA link battle exchanges fixed-size blocks: the handshake, then each side's party,
// then a block per turn carrying the chosen move. On hardware that goes down a wire and the
// serial interrupt fills gBlockRecvBuffer. On this port the whole of that machinery is dead
// -- HandleLinkConnection is stubbed out under PORTABLE and LinkMain2, which is the only
// thing that ever sets a block-received flag, is unreachable -- so the block layer had to be
// implemented rather than redirected.
//
// What it does is narrow on purpose. The battle engine is not reimplemented and no block is
// interpreted here: a block goes out to the server, the opponent's comes back, and it is
// filed under their slot exactly as the hardware would have filed it. Everything above this
// -- the controllers, the turn order, the damage -- runs unchanged.
//
// Two details are not obvious and the battle will hang without either.
//
// The sender's own block echoes back to itself. On a real cable a transmission is seen by
// every machine including the one that sent it, and the handshake relies on it: the wait in
// CB2_HandleStartBattle is `(GetBlockReceivedStatus() & 3) == 3`, which never becomes true
// from the opponent's block alone. So sending also delivers locally.
//
// And the engine asks IsLinkTaskFinished before sending the next block. That reads
// gLinkCallback, which nothing on this path ever sets, so it happens to answer TRUE -- but
// only by accident of the dead machinery. It is left alone deliberately: a send here is a
// queued write to a loopback socket and is finished by the time anything asks.

#include "global.h"
#include "link.h"
#include "mmo_link.h"
#include "net_client.h"

// Blocks are exchanged only while a battle is running, and only between the two players the
// server has seated. Outside that, arriving blocks are dropped rather than filed: a block
// written into gBlockRecvBuffer between battles would be read as the first block of the
// next one.
static bool8 sInBattle;

void MmoLink_BeginBattle(void)
{
    sInBattle = TRUE;
    ResetBlockReceivedFlags();
}

void MmoLink_EndBattle(void)
{
    // Only worth saying once, and only if there was a battle: this is also reached on paths
    // that tidy up when none was running.
    if (sInBattle)
        Net_SendBattleEnded();

    sInBattle = FALSE;
    ResetBlockReceivedFlags();
}

bool8 MmoLink_InBattle(void)
{
    return sInBattle;
}

// File one block under a player's slot, the way the serial interrupt would have.
static void Deliver(u8 slot, const u8 *bytes, u16 size)
{
    if (slot >= MAX_RFU_PLAYERS || size > BLOCK_BUFFER_SIZE)
        return;

    memcpy(gBlockRecvBuffer[slot], bytes, size);
    Link_SetBlockReceived(slot);
}

// Send a block to the opponent, and to ourselves.
//
// Returns TRUE like the hardware path does, meaning the send was accepted rather than
// completed. Nothing above this distinguishes the two.
bool8 MmoLink_SendBlock(const void *src, u16 size)
{
    if (!sInBattle || src == NULL || size == 0 || size > BLOCK_BUFFER_SIZE)
        return FALSE;

    Net_SendLinkBlock(src, size);
    // The echo. Without it the handshake waits forever for a block it sent itself.
    Deliver(GetMultiplayerId(), src, size);
    return TRUE;
}

// Drain whatever the opponent sent. Called once per frame from the battle's main loop, so a
// block is available on the frame after it arrives rather than whenever the game next
// happens to ask.
void MmoLink_Update(void)
{
    struct NetLinkBlock block;

    if (!sInBattle)
        return;

    while (Net_PopLinkBlock(&block))
    {
        // Our own slot is filled by the echo at send time. A block claiming to come from it
        // is the server confused or a client lying, and filing it would overwrite what we
        // are still waiting to have read.
        if (block.fromSlot == GetMultiplayerId())
            continue;

        Deliver(block.fromSlot, block.bytes, block.len);
    }
}
