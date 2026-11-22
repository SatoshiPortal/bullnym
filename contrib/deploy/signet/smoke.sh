#!/usr/bin/env bash
# End-to-end check of a signet deployment. A bark wallet registers a nym
# and its store, then goes quiet while sales are paid through the store's
# public checkout: one by the external Lightning node, one by a second bark
# wallet through the invoice's Ark hint, and one on-chain. It also pays the
# Lightning Address.
#
# Run after setup.sh and fund.sh. It creates its wallets under ~/smoke and
# reuses them on the next run.
set -euo pipefail

DATA=/var/lib/scuttle
PREFIX=/opt/scuttle
SERVICE=http://127.0.0.1:3080
NYM=${NYM:-smoke}
WORK="$HOME/smoke"
mkdir -p "$WORK"
eval "$(sudo cat /etc/scuttle/secrets.env)"

cli() { sudo -u scuttle bitcoin-cli -datadir="$DATA/bitcoind" -signet "$@"; }
mine() { sudo -u scuttle "$PREFIX/bin/signet-mine" "$1" >/dev/null; }
lncli() { sudo docker exec "scuttle-$1" lightning-cli --network=signet --lightning-dir=/data/.lightning "${@:2}"; }
bark() { local w=$1; shift; "$PREFIX/bin/bark" --datadir "$WORK/$w" -q "$@" 2>/dev/null; }
api() { # method path [token] [json]
	curl -sf -X "$1" "$SERVICE$2" ${3:+-H "Authorization: Bearer $3"} \
		${4:+-H 'Content-Type: application/json' -d "$4"}
}
new_wallet() {
	[ -d "$WORK/$1" ] && return
	"$PREFIX/bin/bark" create --datadir "$WORK/$1" --signet --ark http://127.0.0.1:3535 \
		--bitcoind http://127.0.0.1:38332 --bitcoind-user scuttle --bitcoind-pass "$BITCOIND_RPC_PASS" \
		>/dev/null 2>&1
}
wait_state() { # id state
	for i in $(seq 1 90); do
		state=$(api GET "/api/v1/sales/$1" | jq -r .state)
		[ "$state" = "$2" ] && return 0
		sleep 2
	done
	echo "sale $1 is $state, expected $2" >&2
	return 1
}

echo "== owner registers the nym and the store"
new_wallet owner
# The nym is fixed to this address for good, so keep using the first one.
[ -f "$WORK/owner-address" ] || bark owner address > "$WORK/owner-address"
address=$(cat "$WORK/owner-address")
sp=$(curl -sf -X POST http://127.0.0.1:3090/dev/silent-payment-address -d '{}' | jq -r .address)
ts=$(date +%s)
msg=$(printf 'bullnym registration v1\nnym:%s\nark_address:%s\nsilent_payment_address:%s\ntimestamp:%s' \
	"$NYM" "$address" "$sp" "$ts")
sig=$(bark owner message sign "$msg" "$address" | jq -r .signature)
api POST /api/v1/nym "" "$(jq -n --arg n "$NYM" --arg a "$address" --arg p "$sp" --argjson t "$ts" --arg s "$sig" \
	'{registration: {nym: $n, ark_address: $a, silent_payment_address: $p, timestamp: $t}, signature: $s}')" \
	>/dev/null || echo "(nym already registered)"
# The store's dedicated on-chain account, here a watch-only descriptor from
# bitcoind.
cli -named createwallet wallet_name=smoke-store >/dev/null 2>&1 || true
descriptor=$(cli -rpcwallet=smoke-store listdescriptors \
	| jq -r '[.descriptors[] | select(.desc | startswith("tr(")) | select(.internal == false)][0].desc')
ts=$(date +%s)
msg=$(printf 'scuttle-pos registration v1\nname:%s\nark_address:%s\nonchain_descriptor:%s\ncurrency:%s\ntimestamp:%s\nnym:%s' \
	"Smoke Cafe" "$address" "$descriptor" CRC "$ts" "$NYM")
sig=$(bark owner message sign "$msg" "$address" | jq -r .signature)
reg=$(jq -n --arg a "$address" --arg d "$descriptor" --argjson t "$ts" --arg y "$NYM" --arg s "$sig" \
	'{registration: {name: "Smoke Cafe", ark_address: $a, onchain_descriptor: $d, currency: "CRC", timestamp: $t, nym: $y}, signature: $s}')
api POST /api/v1/stores "" "$reg" >/dev/null || echo "(store already registered)"
challenge=$(api POST /api/v1/owner/challenge "" "{\"ark_address\": \"$address\"}" | jq -r .challenge)
login_sig=$(bark owner message sign "$challenge" "$address" | jq -r .signature)
owner=$(api POST /api/v1/owner/login "" "$(jq -n --arg a "$address" --arg c "$challenge" --arg s "$login_sig" \
	'{ark_address: $a, challenge: $c, signature: $s}')" | jq -r .token)

echo "== sale 1, paid over Lightning by the external node"
sale=$(api POST "/api/v1/checkout/$NYM/sales" "" '{"amount_sat": 21000, "memo": "smoke lightning", "cashier_name": "Smoke till"}')
id=$(echo "$sale" | jq -r .id)
lncli cln-external xpay "$(echo "$sale" | jq -r .lightning_invoice)" >/dev/null
wait_state "$id" paid
api GET "/api/v1/sales/$id" | jq -c '{state, paid_rail, received_sat}'

echo "== sale 2, paid over Ark through the invoice's hint"
new_wallet payer
if [ "$(bark payer balance | jq '.spendable_sat')" -lt 100000 ]; then
	cli -rpcwallet=miner sendtoaddress "$(bark payer onchain address | jq -r .address)" 0.01 >/dev/null
	mine 1
	bark payer onchain sync >/dev/null 2>&1 || true
	bark payer board --all >/dev/null
	mine 2
	for i in $(seq 1 60); do
		[ "$(bark payer balance | jq '.spendable_sat')" -ge 100000 ] && break
		sleep 2
	done
fi
sale=$(api POST "/api/v1/checkout/$NYM/sales" "" '{"amount_sat": 30000, "memo": "smoke ark"}')
id=$(echo "$sale" | jq -r .id)
# One invoice for every payer. A wallet with address hints on (the default
# with S3) pays its Ark hint by an Ark transfer.
bark payer send "$(echo "$sale" | jq -r .lightning_invoice)" >/dev/null
wait_state "$id" paid
api GET "/api/v1/sales/$id" | jq -c '{state, paid_rail, received_sat}'

echo "== sale 3, paid on-chain: in progress, not guaranteed, then paid"
sale=$(api POST "/api/v1/checkout/$NYM/sales" "" '{"amount_sat": 60000, "memo": "smoke onchain"}')
id=$(echo "$sale" | jq -r .id)
cli -rpcwallet=miner sendtoaddress "$(echo "$sale" | jq -r .onchain_address)" 0.0006 >/dev/null
wait_state "$id" in-progress
api GET "/api/v1/sales/$id" | jq -c '{state, progress, warnings, received_sat, seen_sat}'
mine 1
wait_state "$id" paid
api GET "/api/v1/sales/$id" | jq -c '{state, paid_rail, received_sat}'

echo "== the Lightning Address"
pr=$(api GET "/api/v1/lnurlp/$NYM/callback?amount=5000000" | jq -r .pr)
lncli cln-external xpay "$pr" >/dev/null
api GET "/api/v1/lnurlp/$NYM/callback?amount=5000000&payment_method=ARK" | jq -c .
api GET "/api/v1/lnurlp/$NYM/callback?amount=5000000&payment_method=SP" | jq -c .

echo "== the owner's history and wallet"
api GET "/api/v1/owner/sales?limit=3" "$owner" | jq -c '.sales[] | {state, paid_rail, final, warnings}'
bark owner sync >/dev/null 2>&1 || true
bark owner balance | jq -c '{spendable_sat}'
