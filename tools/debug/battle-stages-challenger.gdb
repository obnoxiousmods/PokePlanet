# The challenger's half of the two-client battle test: the same stage markers, plus the
# challenge itself.
#
# The challenge is issued from the debugger rather than by walking up and pressing A, because
# scripted input cannot reliably put two clients face to face, and a battle that never starts
# tells you nothing about a battle that freezes.

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

# 3
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

# 5
break HandleTurnActionSelectionState
commands
  silent
  delete 5
  printf "### 5 CHOOSING AN ACTION -- battle is running\n"
  continue
end

# 6 -- deletes itself first, so the challenge is issued exactly once. Without that it fires
# every frame and restarts the battle forever.
break MmoPlayers_Update
commands
  silent
  delete 6
  printf "### 0 challenging the other client\n"
  call (void) Net_RequestBattle(5)
  continue
end

run
quit
