#!/usr/bin/env bash
# The UwULock sync code against a real UwULock Server, and the move against a real UwUSync
# Server: both on free ports of this machine, the accounts the tests use, then the tests
# (`crates/uwurdp-sync/src/lock/real.rs`, ignored by a plain `cargo test`). Not part of CI.
#
#   UWULOCK_REPO=../UwULock-Server scripts/lock-live.sh [test name, or the start of one]
#
# UWULOCK_REPO: a checkout of UwULock-Server with `cargo build -p uwulock-server` done (its
#   test CA and registration scripts come from there, `node` runs the latter).
# UWULOCK_BINARY: another server binary than $UWULOCK_REPO/target/debug/uwulock-server.
# UWUSYNC_BINARY: a UwUSync Server binary (`cargo build` in UwUSync-Server); without one, the
#   move's test fails for want of a setup code.
set -euo pipefail

cd "$(dirname "$0")/.."
repo=${UWULOCK_REPO:?UWULOCK_REPO=<UwULock-Server checkout>}
lock_binary=${UWULOCK_BINARY:-$repo/target/debug/uwulock-server}
sync_binary=${UWUSYNC_BINARY:-}
[ -x "$lock_binary" ] || { echo "no $lock_binary: cargo build -p uwulock-server there first"; exit 1; }

port() { python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])'; }
work=$(mktemp -d)
pids=()
trap 'kill "${pids[@]}" 2>/dev/null || true; rm -rf "$work"' EXIT
trap 'echo "--- UwULock Server:"; tail -n 30 "$work/lock.log"; [ -f "$work/sync.log" ] && { echo "--- UwUSync Server:"; tail -n 15 "$work/sync.log"; }' ERR

# UwULock, with a certificate from a test CA, as its own e2e tests run it.
lock_port=$(port)
"$repo/scripts/e2e/test-ca.sh" "$work/tls" >/dev/null
origin=https://localhost:$lock_port
password='correct horse battery staple'
(
  export UWULOCK_DATA=$work/lock UWULOCK_LISTEN=127.0.0.1:$lock_port UWULOCK_PUBLIC=$origin
  export UWULOCK_TLS=files UWULOCK_TLS_CERT=$work/tls/cert.pem UWULOCK_TLS_KEY=$work/tls/key.pem
  export UWULOCK_UPDATE_CHECK=off UWULOCK_LOGIN_ATTEMPTS=1000
  mkdir -p "$UWULOCK_DATA"
  # Room for what the tests push, and a quota one of them runs into.
  "$lock_binary" settings set suite.maxRecords 200 >/dev/null
  "$lock_binary" invite --admin admin@example.com >/dev/null
  for account in login twostep race records rekey refresh realtime move; do
    echo "$account $("$lock_binary" invite "$account@example.com" | tail -1)"
  done >"$work/invites"
  exec "$lock_binary" serve >"$work/lock.log" 2>&1
) &
pids+=($!)

# UwUSync, with its own certificate, pinned through the setup code.
if [ -n "$sync_binary" ]; then
  sync_port=$(port)
  mkdir -p "$work/sync"
  export UWUSYNC_DATA=$work/sync UWUSYNC_LISTEN=127.0.0.1:$sync_port
  export UWUSYNC_PUBLIC=https://127.0.0.1:$sync_port UWUSYNC_UPDATE_CHECK=off
  UWUSYNC_TEST_SETUP=$("$sync_binary" invite | awk '/Setup code:/ { print $3 }')
  export UWUSYNC_TEST_SETUP
  "$sync_binary" serve >"$work/sync.log" 2>&1 &
  pids+=($!)
fi

export NODE_EXTRA_CA_CERTS=$work/tls/ca.pem
for _ in $(seq 1 100); do
  curl -sf --cacert "$NODE_EXTRA_CA_CERTS" "$origin/alive" >/dev/null && [ -s "$work/invites" ] && break
  sleep 0.2
done
while read -r account link; do
  node "$repo/scripts/e2e/register.mjs" "$origin" "$link" "$account@example.com" "$password" >/dev/null
done <"$work/invites"

export UWULOCK_TEST_SERVER=$origin UWULOCK_TEST_CA=$work/tls/ca.pem UWULOCK_TEST_PASSWORD=$password
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-3}
cargo test -p uwurdp-sync --lib --locked -- --ignored "lock::real::${1:-}"
