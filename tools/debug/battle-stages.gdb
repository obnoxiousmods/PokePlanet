# Where does a player battle actually get to?
#
# Each marker is a stage the battle must pass through in order, so a run reads as a high-water
# mark rather than a yes/no. Breakpoints delete themselves after firing: a stage reached is
# reached, and leaving them live buries the interesting lines under repeats.
#
# The numbers in `delete N` MUST match creation order. An off-by-one here once left the
# challenge firing every frame, restarting the battle continuously, which read as a code
# regression and was not.

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

# 2
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
  printf "### 4 first turn reached\n"
  continue
end

# 5
break HandleTurnActionSelectionState
commands
  silent
  delete 5
  printf "### 5 choosing an action\n"
  continue
end

# 6 -- both players' chosen actions arrived, or the order could not be decided.
break SetActionsAndBattlersTurnOrder
commands
  silent
  delete 6
  printf "### 6 turn order set\n"
  continue
end

# 7 -- moves are executing.
break RunTurnActionsFunctions
commands
  silent
  delete 7
  printf "### 7 running turn actions\n"
  continue
end

# 8 -- a whole turn finished and the next is starting. This is the real proof.
break HandleEndTurn_ContinueBattle
commands
  silent
  delete 8
  printf "### 8 TURN COMPLETED, next turn beginning\n"
  continue
end

run
quit
