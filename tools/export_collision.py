#!/usr/bin/env python3
"""Export every map's collision so the server can check where a player may stand.

The server has to answer "is that tile walkable" without trusting the client, and the game
already knows: each entry in a layout's map.bin is a u16 with the collision packed into bits
10-11, so nothing needs to be inferred from tilesets. This reads the same files the game
builds from and writes one compact table.

Run from the repo root:

    tools/export_collision.py server/collision.bin

Output format, all little-endian:

    magic  "PPCL"            4 bytes
    version u16              1
    count   u16              number of maps
    then per map:
        group  u8
        num    u8
        width  u16
        height u16
        bits   ceil(width*height / 8) bytes, one per tile, set = blocked

A tile is blocked when its collision value is non-zero. The game treats non-zero as
impassable for ordinary walking; the finer values distinguish surf and ledges, which matter
for *how* you may cross rather than whether the tile is solid, and are left for when the
server understands those moves.
"""

import json
import os
import struct
import sys

MAGIC = b"PPCL"
VERSION = 1
COLLISION_MASK = 0x0C00
COLLISION_SHIFT = 10


def load_layouts(root):
    with open(os.path.join(root, "data/layouts/layouts.json"), encoding="utf-8") as f:
        data = json.load(f)
    return {entry["id"]: entry for entry in data["layouts"] if entry.get("id")}


def load_map_groups(root):
    """Return (group_index, map_index, map_name) for every map.

    Group and map numbers are positional: the order in map_groups.json is exactly the order
    the generated constants use, which is what the client reports.
    """
    with open(os.path.join(root, "data/maps/map_groups.json"), encoding="utf-8") as f:
        data = json.load(f)

    out = []
    for group_index, group_name in enumerate(data["group_order"]):
        for map_index, map_name in enumerate(data[group_name]):
            out.append((group_index, map_index, map_name))
    return out


def collision_bits(blockdata, width, height):
    """One bit per tile, set when the tile is solid."""
    expected = width * height
    tiles = len(blockdata) // 2
    if tiles < expected:
        raise ValueError(f"map.bin holds {tiles} tiles, layout claims {expected}")

    bits = bytearray((expected + 7) // 8)
    for i in range(expected):
        block = blockdata[i * 2] | (blockdata[i * 2 + 1] << 8)
        if (block & COLLISION_MASK) >> COLLISION_SHIFT:
            bits[i // 8] |= 1 << (i % 8)
    return bits


def main():
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    out_path = sys.argv[1] if len(sys.argv) > 1 else os.path.join(root, "server/collision.bin")

    layouts = load_layouts(root)
    records = []
    skipped = []

    for group, num, map_name in load_map_groups(root):
        map_json = os.path.join(root, "data/maps", map_name, "map.json")
        if not os.path.exists(map_json):
            skipped.append((map_name, "no map.json"))
            continue
        with open(map_json, encoding="utf-8") as f:
            layout_id = json.load(f).get("layout")

        layout = layouts.get(layout_id)
        if layout is None:
            skipped.append((map_name, f"unknown layout {layout_id}"))
            continue

        blockdata_path = os.path.join(root, layout["blockdata_filepath"])
        if not os.path.exists(blockdata_path):
            skipped.append((map_name, "no blockdata"))
            continue

        with open(blockdata_path, "rb") as f:
            blockdata = f.read()

        width, height = layout["width"], layout["height"]
        try:
            bits = collision_bits(blockdata, width, height)
        except ValueError as e:
            skipped.append((map_name, str(e)))
            continue

        records.append((group, num, width, height, bits))

    with open(out_path, "wb") as f:
        f.write(MAGIC)
        f.write(struct.pack("<HH", VERSION, len(records)))
        for group, num, width, height, bits in records:
            f.write(struct.pack("<BBHH", group, num, width, height))
            f.write(bits)

    print(f"wrote {len(records)} maps to {out_path}")
    # Say what was left out rather than letting a map quietly have no collision, which would
    # read as "walkable everywhere" on the server.
    if skipped:
        print(f"skipped {len(skipped)}:")
        for name, why in skipped[:10]:
            print(f"  {name}: {why}")


if __name__ == "__main__":
    main()
