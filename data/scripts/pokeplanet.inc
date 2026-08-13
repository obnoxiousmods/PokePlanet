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

@ Trade: give the other player carried pokedollars or an item. Both are server-authoritative -- the
@ special sends the amount/item, the server moves it between the two saves, and both sides' wallets
@ and bags update when the server pushes them back. Pokemon trading builds on this same channel later.
PokePlanet_EventScript_MenuTrade::
	multichoice 0, 0, MULTI_POKEPLANET_TRADE, FALSE
	switch VAR_RESULT
	case POKEPLANET_TRADE_MONEY, PokePlanet_EventScript_GiveMoneyMenu
	case POKEPLANET_TRADE_ITEM, PokePlanet_EventScript_GiveItemStart
	@ Exit (and a B press) fall through.
	release
	end

@ Give money: a preset-amount menu keeps it to a couple of button presses (no free-form entry).
PokePlanet_EventScript_GiveMoneyMenu::
	multichoice 0, 0, MULTI_POKEPLANET_GIVEMONEY, FALSE
	switch VAR_RESULT
	case POKEPLANET_GIVE_100, PokePlanet_EventScript_Give100
	case POKEPLANET_GIVE_1000, PokePlanet_EventScript_Give1000
	case POKEPLANET_GIVE_10000, PokePlanet_EventScript_Give10000
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

@ Give item: open the bag to pick one item (key/important items are refused), choose how many, then
@ hand it over. The server clamps the count to what you actually hold and cannot duplicate it.
PokePlanet_EventScript_GiveItemStart::
	special PokePlanetGiveOpenBagMenu
	waitstate
	special PokePlanet_GiveItemPicked
	goto_if_eq VAR_RESULT, FALSE, PokePlanet_EventScript_GiveItemDone
	multichoice 0, 0, MULTI_POKEPLANET_GIVEITEMQTY, FALSE
	switch VAR_RESULT
	case POKEPLANET_GIVEITEM_1, PokePlanet_EventScript_GiveItemQty1
	case POKEPLANET_GIVEITEM_10, PokePlanet_EventScript_GiveItemQty10
	case POKEPLANET_GIVEITEM_99, PokePlanet_EventScript_GiveItemQty99
	release
	end

PokePlanet_EventScript_GiveItemQty1::
	setvar VAR_0x8004, 1
	goto PokePlanet_EventScript_DoGiveItem

PokePlanet_EventScript_GiveItemQty10::
	setvar VAR_0x8004, 10
	goto PokePlanet_EventScript_DoGiveItem

PokePlanet_EventScript_GiveItemQty99::
	setvar VAR_0x8004, 99
	goto PokePlanet_EventScript_DoGiveItem

PokePlanet_EventScript_DoGiveItem::
	special PokePlanet_GiveItem
	goto_if_eq VAR_RESULT, FALSE, PokePlanet_EventScript_GiveItemDone
	msgbox PokePlanet_Text_ItemGiven, MSGBOX_DEFAULT
	release
	end

PokePlanet_EventScript_GiveItemDone::
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

PokePlanet_Text_ItemGiven:
	.string "Sent the item to {STR_VAR_1}!$"

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
