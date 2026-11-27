#!/usr/bin/env bash
# Plain-bash tests for scripts/install.sh and scripts/update.sh using a stub `docker` on PATH.
# Run: bash scripts/tests/test-scripts.sh
set -u
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
W=$(mktemp -d); trap 'rm -rf "$W"' EXIT
FAILS=0 PASS=0
ok() { PASS=$((PASS+1)); echo "ok   - $1"; }
bad() { FAILS=$((FAILS+1)); echo "FAIL - $1"; }
check() { # check "name" cmd...
  local n=$1; shift; if "$@" >/dev/null 2>&1; then ok "$n"; else bad "$n"; fi
}

# --- stub docker -------------------------------------------------------------------------
mkdir -p "$W/bin"
cat >"$W/bin/docker" <<'STUB'
#!/usr/bin/env bash
echo "$*" >>"$STUB_LOG"
args=" $* "
case $args in
  *" ps -q "*) case $args in *bsmcp-postgres*) [ "${STUB_PG:-1}" = 1 ] && echo cid1;; *) [ "${STUB_RUNNING:-1}" = 1 ] && echo cid1;; esac; exit 0;;
  *" config --services "*) printf 'bsmcp-postgres\nbsmcp-server\nbsmcp-embedder\n'; exit 0;;
  *" exec "*pg_dump*) echo PGDUMPDATA; exit 0;;
  *" cp "*) last=${*: -1}; echo data >"${last%/.}/bookstack-mcp.db"; exit 0;;
  *" logs "*) echo "bsmcp-server version 9.9.9 listening"; exit 0;;
  " inspect -f {{.Config.Image}} "*) echo ghcr.io/drbelt27/bsmcp-server:dev; exit 0;;
  " inspect -f {{.Image}} "*) echo sha256:imgid; exit 0;;
  " inspect "*) echo healthy; exit 0;;
  " image inspect "*) echo ghcr.io/drbelt27/bsmcp-server@sha256:old; exit 0;;
  " buildx "*) exit 1;;
esac
exit 0
STUB
chmod +x "$W/bin/docker"
export PATH="$W/bin:$PATH" STUB_LOG="$W/docker.log"

newdir() { local d="$W/$1"; mkdir -p "$d"; cp "$ROOT/.env.example" "$d/"; cp -r "$ROOT/docker" "$d/"; echo "$d"; }
val() { sed -n "s/^$2=//p" "$1/.env" | tail -n 1; }
TOKSEC='sp ace $x #y'

# --- install.sh ---------------------------------------------------------------------------
D=$(newdir inst)
IA=(--yes --dir "$D" --db postgres --semantic on --public-domain mcp.example.com --bookstack-url https://bs.example.com --skip-net-check)
BSMCP_EMBED_TOKEN_ID=tid BSMCP_EMBED_TOKEN_SECRET=$TOKSEC bash "$ROOT/scripts/install.sh" "${IA[@]}" >"$W/out1" 2>&1
check "install: exits ok and writes .env" test -f "$D/.env"
check "install: encryption key >= 32 chars" test "$(val "$D" BSMCP_ENCRYPTION_KEY | wc -c)" -ge 33
check "install: webhook secret >= 16 chars" test "$(val "$D" BSMCP_WEBHOOK_SECRET | wc -c)" -ge 17
check "install: postgres password generated" test "$(val "$D" BSMCP_DB_PASSWORD | wc -c)" -ge 17
check "install: placeholder encryption key replaced" bash -c "! grep -q 'your-secret-key' '$D/.env'"
S1=$(val "$D" BSMCP_ENCRYPTION_KEY)$(val "$D" BSMCP_WEBHOOK_SECRET)$(val "$D" BSMCP_DB_PASSWORD)
bash "$ROOT/scripts/install.sh" "${IA[@]}" >"$W/out2" 2>&1
S2=$(val "$D" BSMCP_ENCRYPTION_KEY)$(val "$D" BSMCP_WEBHOOK_SECRET)$(val "$D" BSMCP_DB_PASSWORD)
check "install: re-run keeps existing secrets" test "$S1" = "$S2"
check "install: re-run detects in-place mode" grep -q "in-place mode" "$W/out2"
# value with spaces, $ and # must survive both `source` and a compose-style parse
GOT=$(set -a; . "$D/.env"; printf %s "$BSMCP_EMBED_TOKEN_SECRET")
check "install: .env with spaces sources in bash" test "$GOT" = "$TOKSEC"
RAW=$(val "$D" BSMCP_EMBED_TOKEN_SECRET); RAW=${RAW#\'}; RAW=${RAW%\'}
check "install: compose-style parse (single quotes literal)" test "$RAW" = "$TOKSEC"
echo x >"$W/m"; chmod 600 "$W/m"
if [ "$(stat -c %a "$W/m" 2>/dev/null)" = 600 ]; then
  check "install: .env mode 600" test "$(stat -c %a "$D/.env")" = 600
else echo "skip - .env mode 600 (chmod not effective on this filesystem)"; fi
check "install: no secret value in stdout/stderr" bash -c "! cat '$W/out1' '$W/out2' | grep -qF -e '$S1' -e \"\$(sed -n 's/^BSMCP_ENCRYPTION_KEY=//p' '$D/.env')\" -e 'sp ace'"
check "install: prints Apache and Nginx snippets" bash -c "grep -q 'proxy_buffering off' '$W/out1' && grep -q 'ProxyPass' '$W/out1'"
D2=$(newdir dry)
bash "$ROOT/scripts/install.sh" --yes --dry-run --dir "$D2" --db sqlite --semantic off --public-domain h.example --bookstack-url https://b.example --skip-net-check >"$W/out3" 2>&1
check "install: --dry-run writes no .env" test ! -f "$D2/.env"
check "install: validates compose config" grep -q "config -q" "$STUB_LOG"

# --- update.sh ----------------------------------------------------------------------------
U=$(newdir upd); cp "$D/.env" "$U/.env"; : >"$STUB_LOG"
STUB_PG=0 bash "$ROOT/scripts/update.sh" --yes --dir "$U" >"$W/u1" 2>&1; rc=$?
check "update: exits ok" test $rc = 0
bl=$(grep -n ' exec ' "$STUB_LOG" | head -n 1 | cut -d: -f1); pl=$(grep -n ' pull ' "$STUB_LOG" | head -n 1 | cut -d: -f1)
check "update: backup attempted before pull" test -n "$bl" -a -n "$pl" -a "${bl:-99}" -lt "${pl:-0}"
check "update: backup non-empty on host" bash -c "find '$U/.bsmcp-update/backups' -name 'bookstack-mcp.db' -size +0 | grep -q ."
check "update: previous digests recorded" bash -c "grep -q 'sha256:old' '$U'/.bsmcp-update/*.json"
check "update: only bsmcp-server touched by default" bash -c "grep ' up -d' '$STUB_LOG' | grep -q 'no-deps bsmcp-server' && ! grep ' up -d' '$STUB_LOG' | grep -q embedder"
check "update: source has no 'down' command (let alone -v)" bash -c "! grep -v '^[[:space:]]*#' '$ROOT/scripts/update.sh' | grep -Eq '\bdown\b|--volumes'"
check "update: no volume removal in log" bash -c "! grep -Eq ' down |volume rm|--volumes' '$STUB_LOG'"
STUB_PG=0 bash "$ROOT/scripts/update.sh" --yes --dir "$U" --rollback >"$W/u2" 2>&1; rc=$?
check "rollback: exits ok" test $rc = 0
check "rollback: prints migration warning" grep -q "DATABASE MIGRATION" "$W/u2"
check "rollback: prints restore commands, does not run them" bash -c "grep -q 'To restore' '$W/u2' && ! grep -q ' stop ' '$STUB_LOG'"
check "rollback: pins recorded digest" grep -q 'sha256:old' "$U/.bsmcp-update/override.yml"
STUB_RUNNING=0 bash "$ROOT/scripts/update.sh" --yes --dir "$U" >"$W/u3" 2>&1; rc=$?
check "update: refuses when nothing is running" test $rc != 0
U2=$(newdir updpg); cp "$D/.env" "$U2/.env"; : >"$STUB_LOG"
bash "$ROOT/scripts/update.sh" --yes --dir "$U2" >"$W/u4" 2>&1
check "update(postgres): pg_dump backup is non-empty" bash -c "find '$U2/.bsmcp-update/backups' -name bsmcp.dump -size +0 | grep -q ."

echo "passed=$PASS failed=$FAILS"
[ "$FAILS" = 0 ]
