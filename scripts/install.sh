#!/usr/bin/env bash
# Guided installer for bookstack-mcp. Prepares .env and picks the compose file;
# it never starts containers and never touches firewall, proxy or system config.
# Usage: scripts/install.sh [--yes] [--dir D] [--db sqlite|postgres] [--semantic on|off]
#   [--public-domain H] [--bookstack-url U] [--bookstack-public-url U]
#   [--backend bookstack|libstack] [--dry-run] [--skip-net-check]
# Env fallbacks: BSMCP_INSTALL_DIR, BSMCP_DB_BACKEND, BSMCP_SEMANTIC_SEARCH, BSMCP_PUBLIC_DOMAIN,
#   BSMCP_BOOKSTACK_URL, BSMCP_BOOKSTACK_PUBLIC_URL, BSMCP_BACKEND, BSMCP_EMBED_TOKEN_ID/_SECRET.
set -eu

YES=0 DRY=0 NETCHK=1
DIR=${BSMCP_INSTALL_DIR:-} DB=${BSMCP_DB_BACKEND:-} SEM=${BSMCP_SEMANTIC_SEARCH:-}
DOMAIN=${BSMCP_PUBLIC_DOMAIN:-} BURL=${BSMCP_BOOKSTACK_URL:-} BPUB=${BSMCP_BOOKSTACK_PUBLIC_URL:-}
BACKEND=${BSMCP_BACKEND:-}
while [ $# -gt 0 ]; do
  case $1 in
    --yes|-y) YES=1;; --dry-run) DRY=1;; --skip-net-check) NETCHK=0;;
    --dir) DIR=$2; shift;; --db) DB=$2; shift;; --semantic) SEM=$2; shift;;
    --public-domain) DOMAIN=$2; shift;; --bookstack-url) BURL=$2; shift;;
    --bookstack-public-url) BPUB=$2; shift;; --backend) BACKEND=$2; shift;;
    -h|--help) sed -n 2,8p "$0"; exit 0;;
    *) echo "unknown option: $1" >&2; exit 2;;
  esac
  shift
done

say() { printf '%s\n' "$*"; }
die() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }
warn() { printf 'WARN: %s\n' "$*" >&2; }
[ -n "$DIR" ] || DIR=$(cd "$(dirname "$0")/.." && pwd)
cd "$DIR" || die "cannot enter $DIR"
[ -f .env.example ] && [ -d docker ] || die "$DIR is not a bookstack-mcp checkout (.env.example/docker missing)"

# --- prompts ---------------------------------------------------------------
INTERACTIVE=0; [ "$YES" = 0 ] && [ -t 0 ] && INTERACTIVE=1
[ "$YES" = 1 ] || [ "$INTERACTIVE" = 1 ] || die "no terminal: pass --yes (and flags) for non-interactive use"
ask() { # ask VAR "question" default  (--yes: keep default)
  local a; [ "$INTERACTIVE" = 1 ] || { printf -v "$1" %s "$3"; return 0; }
  read -r -p "$2 [$3]: " a; printf -v "$1" %s "${a:-$3}"
}
confirm() { # confirm "question" default(y|n)
  local a; [ "$INTERACTIVE" = 1 ] || { [ "$2" = y ]; return; }
  read -r -p "$1 [$2]: " a; a=${a:-$2}; [ "$a" = y ] || [ "$a" = Y ]
}

# --- prerequisites -----------------------------------------------------------
command -v docker >/dev/null || die "docker not found"
docker compose version >/dev/null 2>&1 || die "docker compose v2 not found (need the 'docker compose' plugin)"
command -v curl >/dev/null || die "curl not found"
[ -r /dev/urandom ] || die "/dev/urandom not readable"
docker info >/dev/null 2>&1 || die "docker daemon not reachable (is it running? are you in the docker group?)"

# --- .env helpers (values single-quoted only when needed: valid for bash source AND compose) ---
get() { # get KEY -> value of the last KEY= line, quotes stripped
  sed -n "s/^$1=//p" "$ENVF" | tail -n 1 | sed -e "s/^'\(.*\)'$/\1/" -e 's/^"\(.*\)"$/\1/'
}
put() { # put KEY VALUE
  case $2 in *"'"*|*$'\n'*) die "value for $1 contains a quote or newline: not supported";; esac
  local v=$2 tmp="$ENVF.tmp.$$"
  case $v in *[!A-Za-z0-9_@%+=:,./-]*|'') v="'$v'";; esac
  (umask 077; V="$v" K="$1" awk '
    BEGIN{k=ENVIRON["K"]; l=k"="ENVIRON["V"]}
    $0 ~ "^"k"=" {if(!d){print l; d=1}; next}
    $0 ~ "^# ?"k"=" && !d {print l; d=1; next}
    {print} END{if(!d) print l}' "$ENVF" >"$tmp") && mv "$tmp" "$ENVF"
}
rand() { head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n'; }  # 48 hex chars
is_placeholder() { case $1 in your-*|changeme|*://your-*) return 0;; *) return 1;; esac; }
secret() { # secret KEY: generate only when missing (or placeholder on a fresh .env); never replace
  local cur; cur=$(get "$1")
  if [ -z "$cur" ] || { [ "$FRESH" = 1 ] && is_placeholder "$cur"; }; then
    put "$1" "$(rand)"; say "  $1: generated"
  else say "  $1: kept (existing)"; fi
}
setval() { # setval KEY NEW  -- set if empty/placeholder, else ask before changing (never for secrets)
  [ -n "$2" ] || return 0
  local cur; cur=$(get "$1")
  is_placeholder "$cur" && cur=
  [ "$cur" = "$2" ] && return 0
  if [ -n "$cur" ] && [ "$FRESH" = 0 ]; then
    confirm "  $1 is '$cur'; change to '$2'?" n || { say "  $1: kept"; return 0; }
  fi
  put "$1" "$2"; say "  $1: set"
}

# --- .env (real, or a temp copy for --dry-run) -------------------------------------------------
FRESH=0
if [ -f .env ]; then say "Existing install detected (.env present): in-place mode, existing values are kept unless you confirm."
else FRESH=1; fi
if [ "$DRY" = 1 ]; then
  ENVF=$(mktemp); trap 'rm -f "$ENVF"' EXIT
  if [ "$FRESH" = 1 ]; then cp .env.example "$ENVF"; else cp .env "$ENVF"; fi
  say "DRY RUN: nothing is written; checks run against a temporary copy."
else
  ENVF=.env
  if [ "$FRESH" = 1 ]; then (umask 077; cp .env.example .env); say "Created .env from .env.example"; fi
fi

# --- choices -----------------------------------------------------------------------------------
[ -n "$DB" ] || ask DB "Database (sqlite|postgres)" postgres
case $DB in sqlite|postgres) ;; *) die "--db must be sqlite or postgres";; esac
[ -n "$SEM" ] || ask SEM "Semantic search (on|off)" on
case $SEM in on|true) SEM=true;; off|false) SEM=false;; *) die "--semantic must be on or off";; esac
[ -n "$BACKEND" ] || BACKEND=bookstack
case $BACKEND in bookstack|libstack) ;; *) die "--backend must be bookstack or libstack";; esac
if [ "$BACKEND" = libstack ]; then UKEY=BSMCP_LIBSTACK_URL PKEY=BSMCP_LIBSTACK_PUBLIC_URL; else UKEY=BSMCP_BOOKSTACK_URL PKEY=BSMCP_BOOKSTACK_PUBLIC_URL; fi
if [ -z "$BURL" ]; then
  cur=$(get "$UKEY"); is_placeholder "$cur" && cur=
  if [ -n "$cur" ]; then BURL=$cur; else ask BURL "URL the server dials for the content API" ""; fi
fi
[ -n "$BURL" ] || die "--bookstack-url is required"
if [ -z "$DOMAIN" ]; then cur=$(get BSMCP_PUBLIC_DOMAIN); ask DOMAIN "Public domain of this MCP server (OAuth redirects)" "$cur"; fi
[ -n "$DOMAIN" ] || die "--public-domain is required"
[ -n "$BPUB" ] || BPUB=$(get "$PKEY")

say "Writing configuration:"
[ "$BACKEND" = bookstack ] && [ -z "$(get BSMCP_BACKEND)" ] || setval BSMCP_BACKEND "$BACKEND"
setval "$UKEY" "$BURL"
setval "$PKEY" "$BPUB"
setval BSMCP_PUBLIC_DOMAIN "$DOMAIN"
setval BSMCP_SEMANTIC_SEARCH "$SEM"
secret BSMCP_ENCRYPTION_KEY
secret BSMCP_WEBHOOK_SECRET
if [ "$DB" = postgres ]; then secret BSMCP_DB_PASSWORD; fi
if [ "$BACKEND" = libstack ]; then warn "the shipped compose files do not forward BSMCP_BACKEND/LIBSTACK_* to the containers: add them to the compose 'environment:' yourself."; fi
enc=$(get BSMCP_ENCRYPTION_KEY); [ "${#enc}" -ge 32 ] || warn "BSMCP_ENCRYPTION_KEY is shorter than 32 chars: the server will refuse to start"

if [ "$SEM" = true ] && [ -z "$(get BSMCP_EMBED_TOKEN_ID)" ]; then
  id=${BSMCP_EMBED_TOKEN_ID:-} sec=${BSMCP_EMBED_TOKEN_SECRET:-}
  if [ -z "$id" ] && [ "$INTERACTIVE" = 1 ]; then
    read -r -p "Embedder BookStack token ID (empty to skip): " id
    if [ -n "$id" ]; then read -r -s -p "Embedder token secret (hidden): " sec; echo; fi
  fi
  if [ -n "$id" ] && [ -n "$sec" ]; then put BSMCP_EMBED_TOKEN_ID "$id"; put BSMCP_EMBED_TOKEN_SECRET "$sec"; say "  embedder token: set"
  else warn "semantic search is on but BSMCP_EMBED_TOKEN_ID/SECRET are not set: the embedder cannot crawl until you add them to .env"; fi
fi
if [ "$DRY" = 1 ]; then :; else chmod 600 .env; fi

# --- resources (BSMCP_EMBED_CPUS is the only limit the compose files support) -------------------
cpus=$(nproc 2>/dev/null || echo 1)
mem_kb=$(sed -n 's/^MemTotal:[[:space:]]*\([0-9]*\).*/\1/p' /proc/meminfo 2>/dev/null || true)
say "Host: ${cpus} CPU(s), $(( ${mem_kb:-0} / 1024 )) MiB RAM"
[ "${mem_kb:-9999999}" -ge 3000000 ] || warn "under 3 GiB RAM: the local embedding model (ONNX) may be OOM-killed; consider BSMCP_EMBED_PROVIDER=ollama/openai"
if [ -z "$(get BSMCP_EMBED_CPUS)" ] && [ "$cpus" -gt 1 ]; then
  half=$(( cpus / 2 ))
  if confirm "Limit the embedder to $half CPU(s) (BSMCP_EMBED_CPUS) so it does not starve the host?" y; then put BSMCP_EMBED_CPUS "$half"; fi
fi

# --- compose file, validation, ports -----------------------------------------------------------
CF=docker/docker-compose.yml; if [ "$DB" = sqlite ]; then CF=docker/docker-compose.sqlite.yml; fi
docker compose --env-file "$ENVF" -f "$CF" config -q || die "compose validation failed for $CF"
say "Compose file valid: $CF"

port=$(get BSMCP_PORT); port=${port:-8080}
if ! docker compose --env-file "$ENVF" -f "$CF" ps -q bsmcp-server 2>/dev/null | grep -q .; then
  if { ss -ltn 2>/dev/null || true; } | grep -qE "[:.]$port[[:space:]]" || { command -v lsof >/dev/null && lsof -iTCP:"$port" -sTCP:LISTEN >/dev/null 2>&1; }; then
    warn "port $port is already in use: set BSMCP_PORT in .env to another value"
  fi
fi

# --- network checks (throwaway container; read-only) --------------------------------------------
if [ "$NETCHK" = 1 ]; then
  say "Network checks (throwaway container):"
  probe() { docker run --rm curlimages/curl:8.10.1 -sS -o /dev/null --max-time 10 "$@" 2>/dev/null; }
  if probe -I https://huggingface.co; then say "  container egress: ok"
  else warn "containers have no outbound internet (cannot reach huggingface.co); the embedder needs it to download its model.
         Typical cause: host firewall forwarding (nftables/iptables/Webmin/Virtualmin) drops traffic from Docker bridges.
         Fix on the host (not done by this script): allow forwarding and masquerade for docker0 and br-* interfaces,
         then restart docker. Verify: docker run --rm curlimages/curl -sSI https://huggingface.co"; fi
  case $BURL in http://*|https://*)
    host=${BURL#*://}; host=${host%%/*}; host=${host%%:*}
    if probe -I "$BURL"; then say "  container -> $BURL: ok"
    elif curl -sS -o /dev/null --max-time 10 -I "$BURL" 2>/dev/null; then
      warn "reachable from the host but NOT from a container: no NAT hairpin for $host.
         Keep HTTPS and TLS verification; map the name to an address the host can reach, in a compose override file
         (create it yourself, pass it with an extra '-f'), for bsmcp-server, bsmcp-embedder and bsmcp-worker:
           services:
             bsmcp-server:
               extra_hosts:
                 - \"$host:<private-or-LAN-IP-of-the-BookStack-host>\"
         The hostname stays the same, so certificates still validate. 'host-gateway' works if BookStack runs on this host."
    else warn "$BURL is not reachable from the host either: check the URL"; fi;;
  esac
fi

# --- next steps ---------------------------------------------------------------------------------
cat <<EOT

Next commands (run from $DIR):
  docker compose --env-file .env -f $CF pull
  docker compose --env-file .env -f $CF up -d
  curl -fsS http://127.0.0.1:$port/health

MCP endpoint behind your proxy: https://$DOMAIN/mcp/sse
To publish the port on localhost only (reverse proxy on the same host), use a compose override (compose >= 2.24):
  services:
    bsmcp-server:
      ports: !override
        - "127.0.0.1:$port:$port"

--- Apache (mod_proxy, mod_proxy_http; SSE: no buffering, long timeouts) ---
  ProxyPreserveHost On
  ProxyPass        /  http://127.0.0.1:$port/ timeout=3600 flushpackets=on
  ProxyPassReverse /  http://127.0.0.1:$port/
  RequestHeader set X-Forwarded-Proto "https"

--- Nginx (inside your server block for $DOMAIN) ---
  location / {
    proxy_pass http://127.0.0.1:$port;
    proxy_http_version 1.1;
    proxy_set_header Host \$host;
    proxy_set_header X-Forwarded-Proto \$scheme;
    proxy_set_header X-Forwarded-For \$proxy_add_x_forwarded_for;
    proxy_set_header Connection "";
    proxy_buffering off;
    proxy_cache off;
    proxy_read_timeout 3600s;
    proxy_send_timeout 3600s;
  }
EOT
if [ "$DRY" = 1 ]; then say "DRY RUN finished: no file was changed."; fi
