#!/usr/bin/env python3
"""Derive the set of monotonic story flags for server-side validation.

A monotonic flag is one the game never clears: no script `clearflag`s it and no C code
`FlagClear`s it. Because the honest game never clears such a flag, refusing a report that clears
one cannot strand an honest player -- it can only catch a rollback forgery. That safety property
is the whole reason this is derived rather than hand-listed: a hand-list is wrong in ways that
freeze real players mid-story.

Excluded from the monotonic set, deliberately and for safety:
  - temporary flags (TEMP_FLAGS, cleared on every map load),
  - daily flags (DAILY_FLAGS, cleared each day),
  - special/runtime flags (>= SPECIAL_FLAGS_START, not persisted),
  - anything cleared anywhere in scripts or C.

Output: server/server/src/quest_flags_gen.rs, a generated MONOTONIC_FLAGS: &[u16] plus the badge
flag ids. Regenerate with this script when the game's flags or scripts change.
"""
import os
import re
import subprocess
import sys

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
FLAGS_H = os.path.join(ROOT, "include", "constants", "flags.h")


def flag_names():
    names = []
    for line in open(FLAGS_H, encoding="utf-8"):
        m = re.match(r"\s*#define\s+(FLAG_[A-Z0-9_]+)\b", line)
        if m:
            names.append(m.group(1))
    return names


def flag_values(names):
    """Resolve each FLAG_* to its integer value using the C preprocessor -- the game's own view,
    rather than re-implementing the macro arithmetic here."""
    src = ['#include "constants/flags.h"', "const int V[] = {"]
    src += [f"  {n}," for n in names]
    src += ["};", "// markers so we can print at cpp time is impossible; compile instead"]
    # flags.h references a couple of rematch symbols this fork spells differently in the enum;
    # the full game build resolves them through its own define chain. We only need story-flag
    # values, not these VS-Seeker registration ones, so alias the missing spelling to the enum
    # entry that does exist rather than fight the include order.
    c = "#include \"constants/rematches.h\"\n"
    c += "#ifndef REMATCH_WALLY\n#define REMATCH_WALLY REMATCH_WALLY_VR\n#endif\n"
    c += "#include \"constants/flags.h\"\n#include <stdio.h>\n"
    c += "int main(void){\n"
    for n in names:
        c += f'  printf("%d %s\\n", (int)({n}), "{n}");\n'
    c += "  return 0;\n}\n"
    tmp = "/tmp/flagdump.c"
    open(tmp, "w").write(c)
    exe = "/tmp/flagdump"
    inc = os.path.join(ROOT, "include")
    inc2 = os.path.join(ROOT, "gflib")
    subprocess.run(
        ["gcc", "-I", inc, "-I", inc2, "-o", exe, tmp],
        check=True,
    )
    out = subprocess.check_output([exe]).decode()
    values = {}
    for line in out.splitlines():
        v, n = line.split()
        values[n] = int(v)
    return values


def cleared_flags():
    """Every flag cleared anywhere -- scripts (clearflag) or C (FlagClear). Union, so the
    monotonic set errs toward being smaller (miss a cheat) rather than refusing an honest
    clear."""
    cleared = set()
    for base, _dirs, files in os.walk(os.path.join(ROOT, "data", "scripts")):
        for f in files:
            txt = open(os.path.join(base, f), encoding="utf-8", errors="ignore").read()
            cleared |= set(re.findall(r"clearflag\s+(FLAG_[A-Z0-9_]+)", txt))
    for base, _dirs, files in os.walk(os.path.join(ROOT, "src")):
        for f in files:
            if not f.endswith(".c"):
                continue
            txt = open(os.path.join(base, f), encoding="utf-8", errors="ignore").read()
            cleared |= set(re.findall(r"FlagClear\(\s*(FLAG_[A-Z0-9_]+)\s*\)", txt))
    return cleared


def main():
    names = flag_names()
    values = flag_values(names)
    cleared = cleared_flags()

    temp_end = values.get("FLAG_TEMP_1F", 0x1F)
    daily_start = min(
        (v for n, v in values.items() if "DAILY" in n), default=0x91F
    )
    special_start = 0x4000

    badges = [values[f"FLAG_BADGE0{i}_GET"] for i in range(1, 9)]

    # Ranges the game clears *dynamically* -- by computed id, so a name scan of clearflag/
    # FlagClear misses them. Found by auditing FlagClear(BASE + var) call sites:
    #   ClearTrainerFlag           src/battle_setup.c   TRAINER_FLAGS_START + trainerId
    #   decorations                src/decoration.c     FLAG_DECORATION_1 + i
    #   union room hide            src/union_room...c   FLAG_HIDE_UNION_ROOM_PLAYER_1 + idx
    # These are legitimately cleared in play (rematches, redecorating, leaving a union room),
    # so a monotonic rule over them would freeze honest players. Excluded wholesale.
    dyn_ranges = [
        (0x500, 0x85F),  # TRAINER_FLAGS_START .. TRAINER_FLAGS_END
        (values["FLAG_DECORATION_1"], values["FLAG_DECORATION_1"] + 63),
        (values["FLAG_HIDE_UNION_ROOM_PLAYER_1"], values["FLAG_HIDE_UNION_ROOM_PLAYER_1"] + 15),
    ]

    def in_dynamic(v):
        return any(lo <= v <= hi for lo, hi in dyn_ranges)

    monotonic = []
    for n, v in values.items():
        if v <= temp_end:
            continue  # temporary
        if v >= special_start:
            continue  # runtime-only, not persisted
        if v >= daily_start:
            continue  # daily
        if n in cleared:
            continue  # named-cleared somewhere -> not monotonic
        if in_dynamic(v):
            continue  # cleared by computed id -> not monotonic
        if n.startswith("FLAG_UNUSED") or n.startswith("FLAG_TEMP"):
            continue
        monotonic.append(v)
    monotonic = sorted(set(monotonic))

    out = os.path.join(ROOT, "server", "server", "src", "quest_flags_gen.rs")
    with open(out, "w", encoding="utf-8") as f:
        f.write("// @generated by tools/questflags/extract.py -- do not edit by hand.\n")
        f.write("//\n")
        f.write("// Flags the game never clears (no script clearflag, no C FlagClear), in the\n")
        f.write("// persisted, non-temporary, non-daily range. Clearing one in a report is a\n")
        f.write("// rollback the honest game never performs. See the extractor for why this is\n")
        f.write("// safe against stranding players.\n")
        f.write(f"pub const MONOTONIC_FLAGS: &[u16] = &[\n")
        for i in range(0, len(monotonic), 12):
            row = ", ".join(str(v) for v in monotonic[i : i + 12])
            f.write(f"    {row},\n")
        f.write("];\n\n")
        f.write("/// The eight badge flags, monotonic and also cross-checked against the badge count.\n")
        f.write(f"pub const BADGE_FLAGS: [u16; 8] = {badges!r};\n".replace("[", "[").replace("'", ""))
    print(f"{len(monotonic)} monotonic flags, {len(cleared)} cleared flags excluded -> {out}")


if __name__ == "__main__":
    main()
