<!--
Two things this project has learned the hard way, so they are asked for up front rather than
in review.
-->

## What this changes

<!-- The behaviour, not the diff. -->

## Why

<!-- What was wrong, or what could not be done before. -->

## How it was verified

<!--
Not "it builds". What did you run, and what did it print?

Three changes in this repo have compiled, read correctly, and been wrong; two of them would
have lost players their Pokemon. Each was caught by a test that could fail.

If the check is new, say how you confirmed it can fail -- a fixture-gated test that quietly
skips looks exactly like one that passes.
-->

## Checklist

- [ ] `cargo test --all` in `server/` passes
- [ ] If the link or battle path changed: `tools/debug/two-client-battle.sh` still reaches
      "choosing an action" or further
- [ ] If chat parsing changed: `tools/debug/test-chat-parse.sh` passes
- [ ] If anything moved between done / in progress / planned: **both** `ROADMAP.md` and
      `README.md` updated, identically, in this commit
