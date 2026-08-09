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
	msgbox PokePlanet_Text_RequestSent, MSGBOX_DEFAULT
	release
	end

PokePlanet_EventScript_DeclineBattle::
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
