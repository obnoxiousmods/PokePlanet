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
	multichoice 0, 0, MULTI_POKEPLANET_PLAYER, FALSE
	switch VAR_RESULT
	case POKEPLANET_PLAYER_BATTLE, PokePlanet_EventScript_MenuBattle
	case POKEPLANET_PLAYER_TRADE, PokePlanet_EventScript_MenuTrade
	case POKEPLANET_PLAYER_FOLLOW, PokePlanet_EventScript_MenuFollow
	@ Exit (case 3) and a B press fall through to here.
	release
	end

PokePlanet_EventScript_MenuBattle::
	special PokePlanet_SendBattleRequest
	msgbox PokePlanet_Text_RequestSent, MSGBOX_DEFAULT
	release
	end

@ Trade: give the other player carried pokedollars. A preset-amount menu keeps it to a couple of
@ button presses (no free-form number entry). The move is server-authoritative: the special sends
@ the amount, the server takes it from your save and adds it to theirs, and both wallets update when
@ the server pushes them back. The whole item/Pokemon trade builds on this same channel later.
PokePlanet_EventScript_MenuTrade::
	multichoice 0, 0, MULTI_POKEPLANET_GIVEMONEY, FALSE
	switch VAR_RESULT
	case POKEPLANET_GIVE_100, PokePlanet_EventScript_Give100
	case POKEPLANET_GIVE_1000, PokePlanet_EventScript_Give1000
	case POKEPLANET_GIVE_10000, PokePlanet_EventScript_Give10000
	@ Cancel (and a B press) fall through.
	release
	end

PokePlanet_EventScript_Give100::
	setvar VAR_0x8004, 100
	goto PokePlanet_EventScript_GiveMoney

PokePlanet_EventScript_Give1000::
	setvar VAR_0x8004, 1000
	goto PokePlanet_EventScript_GiveMoney

PokePlanet_EventScript_Give10000::
	setvar VAR_0x8004, 10000
	goto PokePlanet_EventScript_GiveMoney

PokePlanet_EventScript_GiveMoney::
	special PokePlanet_GiveMoney
	msgbox PokePlanet_Text_MoneyGiven, MSGBOX_DEFAULT
	release
	end

@ Follow: trail the other player automatically until you move by hand (or they leave).
PokePlanet_EventScript_MenuFollow::
	special PokePlanet_SetFollow
	msgbox PokePlanet_Text_Following, MSGBOX_DEFAULT
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

PokePlanet_Text_ComingSoon:
	.string "That's coming soon to\n"
	.string "POKéPLANET!$"

PokePlanet_Text_Following:
	.string "Now following {STR_VAR_1}!\p"
	.string "Move on your own to stop.$"

PokePlanet_Text_MoneyGiven:
	.string "Sent the money to {STR_VAR_1}!$"

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
