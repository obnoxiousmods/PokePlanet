@ Interacting with another PokePlanet player.
@
@ Remote players are spawned dynamically and have no entry in the map's object event
@ template list, so the engine cannot resolve a script for them the usual way. This script
@ is returned directly by MmoPlayers_GetInteractionScript instead.
@
@ gStringVar1 holds the other player's display name, set by that function.

PokePlanet_EventScript_PlayerInteract::
	lock
	faceplayer
	msgbox PokePlanet_Text_AskBattle, MSGBOX_YESNO
	goto_if_eq VAR_RESULT, NO, PokePlanet_EventScript_DeclineBattle
	special PokePlanet_SendBattleRequest
	msgbox PokePlanet_Text_RequestSent, MSGBOX_DEFAULT
	release
	end

PokePlanet_EventScript_DeclineBattle::
	release
	end

@ Someone challenged us. Started from MmoPlayers_Update the moment the field is idle, so
@ the prompt never lands on top of another script.
@
@ gStringVar1 holds the challenger's name.
PokePlanet_EventScript_BattleInvite::
	lock
	msgbox PokePlanet_Text_Challenged, MSGBOX_YESNO
	goto_if_eq VAR_RESULT, NO, PokePlanet_EventScript_DeclineChallenge
	special PokePlanet_AcceptBattle
	msgbox PokePlanet_Text_Accepted, MSGBOX_DEFAULT
	release
	end

PokePlanet_EventScript_DeclineChallenge::
	special PokePlanet_DeclineBattle
	release
	end

@ How the challenge we sent turned out. gStringVar1 holds the other player's name, or the
@ server's reason in the failure case.
PokePlanet_EventScript_ChallengeAccepted::
	lock
	msgbox PokePlanet_Text_TheyAccepted, MSGBOX_DEFAULT
	release
	end

PokePlanet_EventScript_ChallengeDeclined::
	lock
	msgbox PokePlanet_Text_TheyDeclined, MSGBOX_DEFAULT
	release
	end

PokePlanet_EventScript_ChallengeFailed::
	lock
	msgbox PokePlanet_Text_ChallengeFailed, MSGBOX_DEFAULT
	release
	end

@ The player walked away between the interaction and the script starting.
PokePlanet_EventScript_PlayerGone::
	lock
	msgbox PokePlanet_Text_PlayerGone, MSGBOX_DEFAULT
	release
	end

PokePlanet_Text_AskBattle:
	.string "{STR_VAR_1} is another TRAINER\n"
	.string "here on POKéPLANET!\p"
	.string "Do you want to battle them?$"

PokePlanet_Text_RequestSent:
	.string "Battle request sent to {STR_VAR_1}!\p"
	.string "They can answer it from their\n"
	.string "POKéNAV.$"

PokePlanet_Text_PlayerGone:
	.string "They wandered off…$"

PokePlanet_Text_Challenged:
	.string "{STR_VAR_1} wants to battle!\p"
	.string "Do you accept?$"

PokePlanet_Text_Accepted:
	.string "You accepted {STR_VAR_1}'s\n"
	.string "challenge!$"

PokePlanet_Text_TheyAccepted:
	.string "{STR_VAR_1} accepted your\n"
	.string "challenge!$"

PokePlanet_Text_TheyDeclined:
	.string "{STR_VAR_1} turned down your\n"
	.string "challenge…$"

PokePlanet_Text_ChallengeFailed:
	.string "The challenge could not be sent.\p"
	.string "{STR_VAR_1}$"
