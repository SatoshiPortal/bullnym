#!/usr/bin/env bash
# Drives the public signet deployment from your own machine: two local bark
# wallets (an owner and a customer), the owner API and the cashier link.
#
# Wallets
#   create owner|customer             new wallet
#   balance owner|customer            Ark balance
#   onchain-balance owner|customer    on-chain balance
#   faucet <wallet> [btc]             signet coins on-chain (default 0.01)
#   board <wallet>                    move all on-chain coins into Ark
#   mine [n]                          mine n blocks now (default 1)
#   bark <wallet> <args...>           any bark command
#
# Owner
#   nym <nym>                         register the Lightning Address <nym>@<host>
#   register "<name>" [CRC]           register the store of the nym
#   store                             show the store
#   store-set <key>=<value>...        name, currency, onchain_min_sat
#   cashier-pin <digits>              set or change the PIN that shows the
#                                     cashier screen its own sales
#   owner-sales [n]                   the store's latest sales
#   owner-csv [from] [to]             CSV export (dates YYYY-MM-DD)
#
# Cashier (the store's link; the web app at https://<host>/<nym> is the
# real one)
#   rate                              the store's bitcoin price
#   sell <amount> [memo]              new sale; amount is fiat ("6250",
#                                     "12.50") or sats ("20000sat")
#   sale [id]                         show a sale (default: the last one)
#   wait [id] [state]                 wait until the sale leaves state
#                                     (default: open)
#   cancel [id]                       cancel an open sale
#   bitcoin [id]                      add Bitcoin to an open sale, as the
#                                     cashier screen does after its warning
#   qr [id] [lightning|bitcoin]       print one of the sale's QRs (needs
#                                     qrencode)
#
# Customer
#   pay <wallet> [id] [invoice|onchain]
#                                     pay a sale (default: the last one, by
#                                     its invoice: over Ark when the wallet
#                                     has address hints on and the invoice
#                                     has the hint, else over Lightning).
#                                     onchain pays from the wallet's on-chain
#                                     coins, once Bitcoin was added; mine 1 to
#                                     confirm.
#   pay-uri <wallet> <uri|invoice>    pay a copied QR or invoice
#   lnaddress <wallet> <nym> <sats>   pay a Lightning Address
#
# SIGNET_HOST names the deployment. State lives in $SIGNET_HOME (default
# ~/.bullnym-signet). The faucet, mining and the store's on-chain wallet use
# the deployment's public test helpers at https://<host>/dev/.
set -euo pipefail

HOST="${SIGNET_HOST:?set SIGNET_HOST to the deployment host name}"
ARK="https://$HOST:8443"
ESPLORA="https://$HOST:10000"
SERVICE="https://$HOST"
HOME_DIR="${SIGNET_HOME:-$HOME/.bullnym-signet}"
BARK="${BARK:-bark}"

mkdir -p "$HOME_DIR"

die() { echo "error: $*" >&2; exit 1; }
wallet_dir() {
	case "$1" in owner|customer) echo "$HOME_DIR/$1" ;; *) die "wallet is owner or customer" ;; esac
}
bark() { local d; d=$(wallet_dir "$1"); shift; "$BARK" --datadir "$d" -q "$@" 2>/dev/null; }
dev() { # endpoint json
	curl -sS -f -X POST "$SERVICE/dev/$1" -H 'Content-Type: application/json' -d "$2" \
		|| die "the test helper /dev/$1 failed"
}
mine() { dev mine "{\"blocks\": ${1:-1}}" | jq -r .height; }

# Calls the service. Prints the body; on an error status prints it and fails.
api() { # method path [token] [json]
	local out status
	out=$(curl -sS -w '\n%{http_code}' -X "$1" "$SERVICE$2" ${3:+-H "Authorization: Bearer $3"} \
		${4:+-H 'Content-Type: application/json' -d "$4"})
	status=${out##*$'\n'}
	out=${out%$'\n'*}
	if [ "$status" -ge 400 ]; then
		echo "bullnym $1 $2: $status $out" >&2
		return 1
	fi
	printf '%s\n' "$out"
}

# The owner logs in by signing a challenge with the nym's address. The token
# lasts 24 hours and is cached.
owner_token() {
	local cache="$HOME_DIR/owner-token" address challenge sig token
	if [ -f "$cache" ] && [ "$(( $(date +%s) - $(stat -c %Y "$cache") ))" -lt 80000 ]; then
		cat "$cache"; return
	fi
	address=$(cat "$HOME_DIR/nym-address" 2>/dev/null) || die "register the nym first"
	challenge=$(api POST /api/v1/owner/challenge "" "$(jq -n --arg a "$address" '{ark_address: $a}')" | jq -r .challenge)
	sig=$(bark owner message sign "$challenge" "$address" | jq -r .signature)
	token=$(api POST /api/v1/owner/login "" "$(jq -n --arg a "$address" --arg c "$challenge" --arg s "$sig" \
		'{ark_address: $a, challenge: $c, signature: $s}')" | jq -r .token)
	(umask 077; echo "$token" > "$cache")
	echo "$token"
}

# The API prefix of the store's cashier link.
checkout() {
	echo "/api/v1/checkout/$(cat "$HOME_DIR/nym" 2>/dev/null || die "register the nym first")"
}

sale_id() {
	if [ -n "${1:-}" ]; then echo "$1"; else cat "$HOME_DIR/last-sale" 2>/dev/null || die "no sale yet"; fi
}

get_sale() { api GET "/api/v1/sales/$1"; }

show_sale() {
	jq '{id, state, progress, warnings, notice, amount_sat, fiat_amount, currency, memo,
		received_sat, seen_sat, paid_rail, paid_at, late_payment, expires_at,
		onchain_address, lightning_request, onchain_request}'
}

cmd="${1:-}"; shift || true
case "$cmd" in

# Wallets

create)
	d=$(wallet_dir "${1:-}")
	[ -e "$d" ] && die "$d exists"
	"$BARK" create --datadir "$d" --signet --ark "$ARK" --esplora "$ESPLORA" >/dev/null 2>&1 \
		|| die "bark create failed, see $d/debug.log"
	echo "created $1: $(bark "$1" address)"
	;;
balance)
	bark "${1:?wallet}" balance | jq -c '{spendable_sat, pending_board_sat, pending_lightning_send_sat, pending_arkoor_send_sat}'
	;;
onchain-balance)
	bark "${1:?wallet}" onchain balance | jq -c '{confirmed_sat, trusted_pending_sat, untrusted_pending_sat}'
	;;
faucet)
	who="${1:?wallet}"; amount="${2:-0.01}"
	addr=$(bark "$who" onchain address | jq -r .address)
	dev faucet "$(jq -n --arg a "$addr" --argjson b "$amount" '{address: $a, btc: $b}')" \
		| jq -r --arg w "$who" '"sent \(.btc) BTC to \($w), txid \(.txid), confirmed at height \(.height)"'
	;;
board)
	who="${1:?wallet}"
	# Esplora takes a few seconds to index a new block.
	for i in $(seq 1 30); do
		[ "$(bark "$who" onchain balance | jq .confirmed_sat)" -gt 0 ] && break
		sleep 2
	done
	bark "$who" board --all >/dev/null || die "board failed: no confirmed on-chain coins?"
	mine 1 >/dev/null
	for i in $(seq 1 30); do
		[ "$(bark "$who" balance | jq .pending_board_sat)" = 0 ] && break
		sleep 2
	done
	bark "$who" balance | jq -c '{spendable_sat, pending_board_sat}'
	;;
mine)
	echo "height $(mine "${1:-1}")"
	;;
bark)
	who="${1:?wallet}"; shift
	"$BARK" --datadir "$(wallet_dir "$who")" "$@"
	;;

# Owner

nym)
	nym="${1:?nym}"
	[ -f "$HOME_DIR/nym" ] && die "already registered: $(cat "$HOME_DIR/nym")"
	address=$(bark owner address)
	# The owner's wallet gives its Silent Payment address; this test wallet
	# has none, so the deployment's helper makes one.
	sp=$(dev silent-payment-address '{}' | jq -r .address)
	ts=$(date +%s)
	msg=$(printf 'bullnym registration v1\nnym:%s\nark_address:%s\nsilent_payment_address:%s\ntimestamp:%s' \
		"$nym" "$address" "$sp" "$ts")
	sig=$(bark owner message sign "$msg" "$address" | jq -r .signature)
	api POST /api/v1/nym "" "$(jq -n --arg n "$nym" --arg a "$address" --arg p "$sp" --argjson t "$ts" --arg s "$sig" \
		'{registration: {nym: $n, ark_address: $a, silent_payment_address: $p, timestamp: $t}, signature: $s}')" \
		| jq -c .nym
	echo "$address" > "$HOME_DIR/nym-address"
	echo "$nym" > "$HOME_DIR/nym"
	;;
register)
	name="${1:?store name}"; currency="${2:-CRC}"
	address=$(cat "$HOME_DIR/nym-address" 2>/dev/null) || die "register the nym first"
	nym=$(cat "$HOME_DIR/nym")
	# The store's on-chain account is a dedicated one. Here it is a wallet on
	# the deployment's bitcoind, named after the store.
	sw=$(dev store-wallet "$(jq -n --arg n "$name" '{name: $n}')")
	wallet=$(jq -r .wallet <<<"$sw")
	descriptor=$(jq -r .onchain_descriptor <<<"$sw")
	# posd's mailbox id, which the service stores and does not use.
	mailbox=$(bark owner debug-info | jq -r .mailbox_id)
	ts=$(date +%s)
	msg=$(printf 'scuttle-pos registration v1\nname:%s\nark_address:%s\nmailbox_id:%s\nonchain_descriptor:%s\ncurrency:%s\ntimestamp:%s\nnym:%s' \
		"$name" "$address" "$mailbox" "$descriptor" "$currency" "$ts" "$nym")
	sig=$(bark owner message sign "$msg" "$address" | jq -r .signature)
	api POST /api/v1/stores "" "$(jq -n --arg n "$name" --arg a "$address" --arg m "$mailbox" --arg d "$descriptor" \
		--arg c "$currency" --argjson t "$ts" --arg y "$nym" --arg s "$sig" \
		'{registration: {name: $n, ark_address: $a, mailbox_id: $m, onchain_descriptor: $d, currency: $c, timestamp: $t, nym: $y}, signature: $s}')" \
		| jq -c .store
	echo "$wallet" > "$HOME_DIR/store-onchain-wallet"
	echo "on-chain sales go to the test wallet '$wallet' on the deployment"
	echo "checkout: $SERVICE/$nym"
	;;
store)
	api GET /api/v1/owner/store "$(owner_token)" | jq .
	;;
store-set)
	[ $# -gt 0 ] || die "give key=value pairs"
	body='{}'
	for kv in "$@"; do
		k=${kv%%=*}; v=${kv#*=}
		case "$k" in
			onchain_min_sat) body=$(jq --arg k "$k" --argjson v "$v" '.[$k] = $v' <<<"$body") ;;
			name|currency) body=$(jq --arg k "$k" --arg v "$v" '.[$k] = $v' <<<"$body") ;;
			*) die "unknown key $k" ;;
		esac
	done
	api PATCH /api/v1/owner/store "$(owner_token)" "$body" | jq .
	;;
cashier-pin)
	pin="${1:?4 to 12 digits}"
	api PUT /api/v1/owner/store/cashier-pin "$(owner_token)" "$(jq -n --arg p "$pin" '{pin: $p}')" \
		| jq -c '{name, cashier_pin_set}'
	;;
owner-sales)
	api GET "/api/v1/owner/sales?limit=${1:-10}" "$(owner_token)" \
		| jq -r '.sales[] | "\(.id)\t\(.state)\t\(.amount_sat) sat\t\(.fiat_amount // "-") \(.currency // "")\t\(.paid_rail // "-")\t\(.warnings | join(","))\t\(.memo // "")"'
	;;
owner-csv)
	q=""; [ -n "${1:-}" ] && q="from=$1"; [ -n "${2:-}" ] && q="$q&to=$2"
	api GET "/api/v1/owner/sales.csv?$q" "$(owner_token)"
	;;

# Cashier

rate)
	api GET "$(checkout)/rate" | jq -c .
	;;
sell)
	amount="${1:?amount}"; memo="${2:-}"
	if [[ "$amount" == *sat ]]; then
		body=$(jq -n --argjson a "${amount%sat}" --arg m "$memo" '{amount_sat: $a} + (if $m == "" then {} else {memo: $m} end)')
	else
		body=$(jq -n --arg a "$amount" --arg m "$memo" '{fiat_amount: $a} + (if $m == "" then {} else {memo: $m} end)')
	fi
	sale=$(api POST "$(checkout)/sales" "" "$body")
	jq -r .id <<<"$sale" > "$HOME_DIR/last-sale"
	show_sale <<<"$sale"
	;;
sale)
	get_sale "$(sale_id "${1:-}")" | show_sale
	;;
wait)
	id=$(sale_id "${1:-}"); from="${2:-open}"
	# The API long-polls for 25 s at a time.
	for i in $(seq 1 24); do
		sale=$(api GET "/api/v1/sales/$id?wait=$from")
		[ "$(jq -r .state <<<"$sale")" != "$from" ] && break
	done
	show_sale <<<"$sale"
	;;
cancel)
	api POST "/api/v1/sales/$(sale_id "${1:-}")/cancel" | show_sale
	;;
bitcoin)
	api POST "/api/v1/sales/$(sale_id "${1:-}")/onchain" | show_sale
	;;
qr)
	command -v qrencode >/dev/null || die "install qrencode"
	field=lightning_request; [ "${2:-lightning}" = bitcoin ] && field=onchain_request
	qr=$(get_sale "$(sale_id "${1:-}")" | jq -r ".$field // empty")
	[ -n "$qr" ] || die "the sale has no $field; add Bitcoin first"
	qrencode -t ansiutf8 <<<"$qr"
	;;

# Customer

pay)
	who="${1:?wallet}"; id=$(sale_id "${2:-}"); method="${3:-invoice}"
	sale=$(get_sale "$id")
	case "$method" in
	invoice)
		# One invoice for every payer. With address hints on, bark pays its
		# Ark hint by an Ark transfer; otherwise it pays over Lightning.
		inv=$(jq -r '.lightning_invoice // empty' <<<"$sale"); [ -n "$inv" ] || die "the sale has no invoice"
		bark "$who" send "$inv" >/dev/null || die "payment failed, see $(wallet_dir "$who")/debug.log"
		;;
	onchain)
		addr=$(jq -r '.onchain_address // empty' <<<"$sale")
		[ -n "$addr" ] || die "no on-chain address: add Bitcoin to the sale first (bitcoin [id])"
		bark "$who" onchain send "$addr" "$(jq -r .amount_sat <<<"$sale") sats" | jq -c . \
			|| die "on-chain payment failed: does the wallet have on-chain coins? (faucet)"
		echo "sent; the sale is in progress until 'mine 1'"
		;;
	*) die "method is invoice or onchain" ;;
	esac
	echo "paid $id by $method"
	;;
pay-uri)
	who="${1:?wallet}"; uri="${2:?uri or invoice}"
	if [[ "$uri" == lightning:* ]]; then
		bark "$who" send "${uri#lightning:}" >/dev/null || die "payment failed"
	else
		bark "$who" send "$uri" >/dev/null || die "payment failed"
	fi
	echo "paid"
	;;
lnaddress)
	who="${1:?wallet}"; name="${2:?nym}"; sats="${3:?sats}"
	bark "$who" send "$name@$HOST" "$sats sats" >/dev/null || die "payment failed, see $(wallet_dir "$who")/debug.log"
	echo "paid $sats sats to $name@$HOST"
	;;

*)
	sed -n '2,/^set -euo/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'
	;;
esac
