#!/usr/bin/env bash
# Guided update of bookstack-mcp services: backup first, record previous digests, pull, restart
# only the selected services, wait until healthy. Never runs 'down' and never removes volumes.
# Usage: scripts/update.sh [--dir D] [--service NAME]... [--all] [--channel dev|stable|<tag>]
#                          [--rollback] [--db sqlite|postgres] [--yes] [--timeout SEC]
#   default service: bsmcp-server. --all = every service except bsmcp-postgres.
#   --channel: dev = compose default; stable = :latest (last v* release); anything else = that tag.
#   For production prefer pinned tags or digests over :dev.
set -eu

YES=0 ALL=0 ROLLBACK=0 DIR=${BSMCP_INSTALL_DIR:-} DB= CHANNEL= TIMEOUT=180 SVCS=
while [ $# -gt 0 ]; do
  case $1 in
    --yes|-y) YES=1;; --all) ALL=1;; --rollback) ROLLBACK=1;;
    --dir) DIR=$2; shift;; --db) DB=$2; shift;; --channel) CHANNEL=$2; shift;;
    --service) SVCS="$SVCS $2"; shift;; --timeout) TIMEOUT=$2; shift;;
    -h|--help) sed -n 2,8p "$0"; exit 0;;
    *) echo "unknown option: $1" >&2; exit 2;;
  esac
  shift
done

say() { printf '%s\n' "$*"; }
die() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }
warn() { printf 'WARN: %s\n' "$*" >&2; }
confirm() { [ "$YES" = 1 ] && return 0; [ -t 0 ] || die "no terminal: pass --yes"; local a; read -r -p "$1 [y/N]: " a; [ "$a" = y ] || [ "$a" = Y ]; }
[ -n "$DIR" ] || DIR=$(cd "$(dirname "$0")/.." && pwd)
cd "$DIR" || die "cannot enter $DIR"
[ -f .env ] || die "no .env in $DIR: run scripts/install.sh first"
command -v docker >/dev/null && docker info >/dev/null 2>&1 || die "docker not available or daemon not reachable"

STATE=.bsmcp-update; PINS=$STATE/pins; OVR=$STATE/override.yml
mkdir -p "$STATE"; chmod 700 "$STATE"; : >>"$PINS"
PGF=docker/docker-compose.yml; SQF=docker/docker-compose.sqlite.yml
dc() { # compose with the right file, plus the pin override when one exists
  if [ -s "$PINS" ]; then docker compose --env-file .env -f "$CF" -f "$OVR" "$@"; else docker compose --env-file .env -f "$CF" "$@"; fi
}
write_pins() { # regenerate override.yml from the pins file ("svc image" per line)
  if [ -s "$PINS" ]; then
    { echo "services:"; while read -r s i; do printf '  %s:\n    image: %s\n' "$s" "$i"; done <"$PINS"; } >"$OVR"
  else rm -f "$OVR"; fi
}
pin() { grep -v "^$1 " "$PINS" >"$PINS.n" || true; [ -z "${2:-}" ] || echo "$1 $2" >>"$PINS.n"; mv "$PINS.n" "$PINS"; }

# --- detect the running stack ---------------------------------------------------------------
CF=$PGF
case $DB in
  sqlite) CF=$SQF;; postgres) CF=$PGF;;
  '') if docker compose --env-file .env -f "$PGF" ps -q bsmcp-postgres 2>/dev/null | grep -q .; then CF=$PGF; else CF=$SQF; fi;;
  *) die "--db must be sqlite or postgres";;
esac
write_pins
dc ps -q bsmcp-server | grep -q . || die "bsmcp-server is not running with $CF: nothing to update (start it first, or pass --db)"
if [ "$CF" = "$SQF" ]; then KIND=sqlite; else KIND=postgres; fi
say "Stack: $CF ($KIND)"

if [ "$ALL" = 1 ]; then SVCS=$(dc config --services | grep -v '^bsmcp-postgres$' | tr '\n' ' '); fi
[ -n "${SVCS# }" ] || SVCS=bsmcp-server
TS=$(date -u +%Y%m%dT%H%M%SZ)

cur_ref() { docker inspect -f '{{.Config.Image}}' "$(dc ps -q "$1" | head -n 1)"; }
cur_dig() { docker image inspect -f '{{if .RepoDigests}}{{index .RepoDigests 0}}{{end}}' "$(docker inspect -f '{{.Image}}' "$(dc ps -q "$1" | head -n 1)")"; }

# --- backup (before anything is touched) ----------------------------------------------------
backup() {
  local d="$STATE/backups/$TS"; mkdir -p "$d"
  if [ "$KIND" = postgres ]; then
    dc exec -T bsmcp-postgres pg_dump -U bsmcp -Fc bsmcp >"$d/bsmcp.dump" || die "pg_dump failed: nothing was changed"
    BK=$d/bsmcp.dump
  else
    dc exec -T bsmcp-server sh -c '
      db=/data/bookstack-mcp.db; o=/data/backups/pre-update-'"$TS"'; mkdir -p "$o" || exit 1
      if command -v sqlite3 >/dev/null 2>&1; then sqlite3 "$db" ".backup $o/bookstack-mcp.db"
      else for s in "" -wal -shm; do [ -f "$db$s" ] && cp "$db$s" "$o/"; done; true; fi' \
      || die "sqlite backup failed: nothing was changed"
    dc cp "bsmcp-server:/data/backups/pre-update-$TS/." "$d/" || die "could not copy the backup out of the container"
    BK=$d/bookstack-mcp.db
  fi
  [ -s "$BK" ] || die "backup $BK is missing or empty: nothing was changed"
  say "Backup OK: $BK ($(wc -c <"$BK") bytes)"
}
restore_help() {
  say "To restore the data (NOT run automatically; stops the services, then you start them again):"
  say "  docker compose --env-file .env -f $CF stop bsmcp-server bsmcp-embedder bsmcp-worker"
  if [ "$KIND" = postgres ]; then
    say "  docker compose --env-file .env -f $CF exec -T bsmcp-postgres pg_restore --clean --if-exists -U bsmcp -d bsmcp < $BK"
  else
    say "  docker compose --env-file .env -f $CF run --rm --no-deps --entrypoint sh bsmcp-server -c 'rm -f /data/bookstack-mcp.db-wal /data/bookstack-mcp.db-shm'"
    say "  for f in $(dirname "$BK")/bookstack-mcp.db*; do docker compose --env-file .env -f $CF cp \"\$f\" bsmcp-server:/data/; done"
  fi
  say "  docker compose --env-file .env -f $CF up -d --no-deps bsmcp-server bsmcp-embedder bsmcp-worker"
}

wait_ready() { # wait_ready SVC
  local cid st t=0; cid=$(dc ps -q "$1" | head -n 1)
  while [ "$t" -lt "$TIMEOUT" ]; do
    st=$(docker inspect -f '{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}' "$cid" 2>/dev/null || echo gone)
    case $st in healthy|running) return 0;; esac
    sleep 3; t=$((t+3))
  done
  warn "$1 not ready after ${TIMEOUT}s (state: $st). Last logs:"; dc logs --tail 40 "$1" >&2 || true
  return 1
}
post_check() {
  say "Version/log line: $(dc logs --tail 200 bsmcp-server 2>/dev/null | grep -iE 'version|listening|starting' | tail -n 1 || true)"
  port=$(sed -n 's/^BSMCP_PORT=//p' .env | tail -n 1); port=${port:-8080}
  if curl -fsS --max-time 5 "http://127.0.0.1:$port/health" >/dev/null 2>&1; then say "/health: ok"; else warn "/health did not answer on 127.0.0.1:$port (port may be remapped or bound elsewhere)"; fi
  if dc exec -T bsmcp-server bsmcp-server doctor; then say "doctor: ok"; else warn "doctor unavailable in this image or reported problems"; fi
}

# --- rollback -------------------------------------------------------------------------------
if [ "$ROLLBACK" = 1 ]; then
  rec=$(ls -1 "$STATE"/*.json 2>/dev/null | tail -n 1 || true); [ -n "$rec" ] || die "no record in $STATE: cannot roll back"
  say "Rolling back to the images recorded in $rec"
  ORIG=$(sed -n 's/.*"backup": "\(.*\)".*/\1/p' "$rec")
  warn "If the new version ran a DATABASE MIGRATION, an image rollback may NOT be enough: the old binary can fail on the migrated schema. Then restore the backup too."
  confirm "Roll back now?" || die "aborted"
  rows=$(sed -n 's/^    "\([^"]*\)": {"image": "\([^"]*\)", "digest": "\([^"]*\)"}.*/\1 \3/p' "$rec")
  [ -n "$rows" ] || die "record has no services"
  TS=${TS}-rollback; backup; BK=$ORIG
  while read -r s dg <&3; do
    [ -n "$dg" ] || { warn "$s: no digest recorded (locally built image?): skipped"; continue; }
    pin "$s" "$dg"; write_pins; dc pull "$s" || warn "$s: pull failed, using the local image if present"
    dc up -d --no-deps "$s"; wait_ready "$s" || warn "$s did not become ready after rollback"
  done 3<<EOR
$rows
EOR
  post_check
  restore_help
  exit 0
fi

# --- update ---------------------------------------------------------------------------------
case $CHANNEL in ''|dev) TAG=dev;; stable) TAG=latest;; *) TAG=$CHANNEL;; esac
say "Services: ${SVCS# }   channel: ${CHANNEL:-compose default} (tag: $TAG)"
say "Tip: for production pin a version tag or digest (--channel <tag>) instead of ':dev'."
for s in $SVCS; do
  ref=$(cur_ref "$s"); dig=$(cur_dig "$s")
  new=${ref%%@*}; new="${new%:*}:$TAG"
  avail=$(docker buildx imagetools inspect "$new" 2>/dev/null | sed -n 's/^Digest:[[:space:]]*//p' | head -n 1 || true)
  say "  $s: current $ref ${dig:+($dig)}"
  if [ -n "$avail" ]; then say "  $s: available $new ($avail)"; else say "  $s: available $new (digest known after pull)"; fi
  if [ -n "$avail" ]; then case $dig in *"$avail") say "  $s: already up to date";; esac; fi
done
confirm "Back up and update now?" || die "aborted"

backup
rec=$STATE/$TS.json
{
  echo "{"; echo "  \"timestamp\": \"$TS\","; echo "  \"compose_file\": \"$CF\","; echo "  \"backup\": \"$BK\","; echo "  \"services\": {"
  n=0; for s in $SVCS; do n=$((n+1)); sep=,; [ "$n" -lt "$(echo $SVCS | wc -w)" ] || sep=
    printf '    "%s": {"image": "%s", "digest": "%s"}%s\n' "$s" "$(cur_ref "$s")" "$(cur_dig "$s")" "$sep"; done
  echo "  }"; echo "}"
} >"$rec"
say "Recorded previous images: $rec"

for s in $SVCS; do
  ref=$(cur_ref "$s")
  if [ -z "$CHANNEL" ] || [ "$CHANNEL" = dev ]; then pin "$s" ""; else new=${ref%%@*}; pin "$s" "${new%:*}:$TAG"; fi
done
write_pins
# shellcheck disable=SC2086
dc pull $SVCS
# shellcheck disable=SC2086
dc up -d --no-deps $SVCS
bad=0; for s in $SVCS; do wait_ready "$s" || bad=1; done
if [ "$bad" = 1 ]; then
  warn "update failed. Roll back with: scripts/update.sh --rollback   (backup: $BK)"; exit 1
fi
post_check
say "Done. Rollback if needed: scripts/update.sh --rollback"
