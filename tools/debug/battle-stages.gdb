# Where does a player battle actually get to?
#
# Each marker is a stage the battle must pass through in order, so a run reads as a high-water
# mark rather than a yes/no. Breakpoints delete themselves after firing once: a battle stage
# reached is reached, and leaving them live buries the interesting lines under repeats.
#
# Numbers below MUST match creation order. An off-by-one here once left the challenge firing
# every frame, restarting the battle continuously, which read as a code regression and was not.

set confirm off
set pagination off
set print thread-events off
set unwind-on-signal on

# 1
break MmoBattle_Start
commands
  silent
  printf "### 1 battle starting, slot %d\n", (int)start->linkId
  continue
end

# 2 -- the engine has begun asking controllers for data. Reached today; it stalls here.
break BattleIntroGetMonsData
commands
  silent
  delete 2
  printf "### 2 intro: asking controllers for party data\n"
  continue
end

# 3 -- proves a controller message completed its round trip over the link.
break BattleIntroDrawTrainersOrMonsSprites
commands
  silent
  delete 3
  printf "### 3 intro: drawing, so the exec flags cleared\n"
  continue
end

# 4
break TryDoEventsBeforeFirstTurn
commands
  silent
  delete 4
  printf "### 4 FIRST TURN REACHED\n"
  continue
end

# 5 -- the battle is playable from here.
break HandleTurnActionSelectionState
commands
  silent
  delete 5
  printf "### 5 CHOOSING AN ACTION -- battle is running\n"
  continue
end

run
quit
