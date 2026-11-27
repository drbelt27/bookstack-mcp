#!/usr/bin/env bash
# Optional post-install smoke test for a LIVE bookstack-mcp server. Read-only unless --write.
# Usage: scripts/smoke-test.sh --url URL --token-env NAME [options]
#   --url URL                 server base URL, e.g. https://mcp.example.com
#   --token-env NAME          name of an env var holding the BookStack API token as id:secret
#   --token2-env NAME         env var with a SECOND user's token (isolation check)
#   --private-page-id N       a page user 2 must NOT be able to read (isolation check)
#   --webhook-secret-env NAME env var with BSMCP_WEBHOOK_SECRET (webhook check)
#   --write                   enable every step that modifies data (temp page, image uploads)
#   --book-id N               book where the temp page is created (required with --write)
#   --yes                     skip the interactive 'yes' confirmation for --write
#   --image-url URL           image fetched by upload_image
#                             (default https://www.w3.org/assets/logos/w3c-2025-transitional/w3c-72x48.png)
#   --index-wait SECONDS      max wait for the semantic index to pick up the temp page (default 420)
#   -h, --help                this text
# Tokens are read ONLY from the named env vars, never from argv, and never printed.
# Needs: bash, curl (>= 7.55), jq. Created resources are named '[bsmcp-smoketest] <timestamp>' and are
# deleted on exit (they stay in the BookStack recycle bin until you empty it).
# Output: one line per step: PASS|FAIL|SKIP|WARN  <id>  <detail>. Exit 1 if any FAIL.
# Not covered: attachments upload, ACL reconciliation after permission changes.
# Persistence after restart (step 10) is manual: restart the container, re-run without --write.
set -u

URL= TOKEN_ENV= TOKEN2_ENV= WH_ENV= BOOK= PRIV= WRITE=0 YES=0
IMG=https://www.w3.org/assets/logos/w3c-2025-transitional/w3c-72x48.png
WAIT=420
while [ $# -gt 0 ]; do
  case $1 in
    --url) URL=${2:?--url needs a value}; shift;;
    --token-env) TOKEN_ENV=${2:?--token-env needs a value}; shift;;
    --token2-env) TOKEN2_ENV=${2:?--token2-env needs a value}; shift;;
    --webhook-secret-env) WH_ENV=${2:?--webhook-secret-env needs a value}; shift;;
    --book-id) BOOK=${2:?--book-id needs a value}; shift;;
    --private-page-id) PRIV=${2:?--private-page-id needs a value}; shift;;
    --image-url) IMG=${2:?--image-url needs a value}; shift;;
    --index-wait) WAIT=${2:?--index-wait needs a value}; shift;;
    --write) WRITE=1;; --yes|-y) YES=1;;
    -h|--help) sed -n '2,/^set -u/{/^set -u/!p}' "$0" | sed 's/^# \{0,1\}//'; exit 0;;
    *) echo "unknown option: $1" >&2; exit 2;;
  esac
  shift
done

say() { printf '%s\n' "$*"; }
die() { printf 'ERROR: %s\n' "$*" >&2; exit 2; }
FAILS=0
res() { [ "$1" = FAIL ] && FAILS=$((FAILS+1)); printf '%s  %s  %s\n' "$1" "$2" "$3"; }
isnum() { case $1 in ''|*[!0-9]*) return 1;; esac; }
envval() { # envval NAME -> value of the env var called NAME
  case $1 in *[!A-Za-z0-9_]*|'') die "invalid env var name: $1";; esac
  printf %s "${!1:-}"
}

# --- prerequisites and arguments ---------------------------------------------------------
for c in curl jq base64; do command -v "$c" >/dev/null || die "$c not found (need bash, curl, jq)"; done
[ -n "$URL" ] || die "--url is required (see --help)"
URL=${URL%/}
[ -n "$TOKEN_ENV" ] || die "--token-env is required (see --help)"
TOK1=$(envval "$TOKEN_ENV"); [ -n "$TOK1" ] || die "env var $TOKEN_ENV is empty or unset"
case $TOK1 in *:*) ;; *) die "token in $TOKEN_ENV must look like id:secret";; esac
TOK2=; [ -z "$TOKEN2_ENV" ] || { TOK2=$(envval "$TOKEN2_ENV"); [ -n "$TOK2" ] || die "env var $TOKEN2_ENV is empty or unset"; }
WH_SECRET=; [ -z "$WH_ENV" ] || WH_SECRET=$(envval "$WH_ENV")
isnum "$WAIT" || die "--index-wait must be a number"
[ -z "$PRIV" ] || isnum "$PRIV" || die "--private-page-id must be a number"
if [ "$WRITE" = 1 ]; then
  isnum "$BOOK" || die "--write needs --book-id N"
  if [ "$YES" = 0 ]; then
    printf 'This will create and delete temporary "[bsmcp-smoketest]" pages in book %s on %s.\nType yes to continue: ' "$BOOK" "$URL" >&2
    ans=; read -r ans || ans=
    [ "$ans" = yes ] || die "not confirmed: nothing was changed (use --yes to skip the prompt)"
  fi
fi

T=$(umask 077; mktemp -d); PAGES=
# Tokens go to curl via header files (not argv, so not visible in `ps`).
TF1=$T/h1 TF2=$T/h2 TFW=$T/hw
(umask 077; printf 'Authorization: Bearer %s\n' "$TOK1" >"$TF1"
 printf 'Authorization: Bearer %s\n' "$TOK2" >"$TF2"
 printf 'X-Webhook-Secret: %s\n' "$WH_SECRET" >"$TFW")

# --- helpers ------------------------------------------------------------------------------
code() { curl -sS -m 30 -o /dev/null -w '%{http_code}' "$@" 2>/dev/null; }  # code [curl args] URL
rpc() { # rpc tokfile json -> $BODY (response headers in $T/resp.h)
  BODY=$(curl -sS -m 90 -D "$T/resp.h" -H "@$1" -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' ${SID:+-H "Mcp-Session-Id: $SID"} \
    --data "$2" "$URL/mcp/sse" 2>/dev/null)
}
tool() { # tool tokfile name args-json -> $TXT (result text); rc 0 only when no JSON-RPC/tool error
  rpc "$1" "$(jq -nc --arg n "$2" --argjson a "$3" '{jsonrpc:"2.0",id:1,method:"tools/call",params:{name:$n,arguments:$a}}')"
  TXT=$(printf %s "$BODY" | jq -r '.result.content[0].text // empty' 2>/dev/null)
  printf %s "$BODY" | jq -e '.result != null and (.error // null) == null and (.result.isError // false) == false' >/dev/null 2>&1
}
cleanup() {
  local id
  for id in $PAGES; do
    if tool "$TF1" delete_page "{\"id\":$id}"; then res PASS cleanup "deleted temp page $id"
    else res FAIL cleanup "could NOT delete temp page $id: delete it manually"; fi
  done
  [ -z "$PAGES" ] || say "NOTE: deleted pages remain in the BookStack recycle bin; empty it to purge them."
  PAGES=
}
trap 'cleanup; rm -rf "$T"' EXIT
trap 'exit 130' INT TERM

# --- 1-2 plain endpoints ------------------------------------------------------------------
c=$(code "$URL/health"); [ "$c" = 200 ] && res PASS 1 "health 200" || res FAIL 1 "health returned $c"
c=$(code "$URL/.well-known/oauth-authorization-server"); [ "$c" = 200 ] && res PASS 2 "oauth metadata 200" || res FAIL 2 "oauth metadata returned $c"

# --- 3 initialize + tools/list ------------------------------------------------------------
SID=; TOOLS=
rpc "$TF1" '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"bsmcp-smoke","version":"1"}}}'
if printf %s "$BODY" | jq -e '.result != null' >/dev/null 2>&1; then
  SID=$(sed -n 's/^[Mm][Cc][Pp]-[Ss]ession-[Ii]d:[[:space:]]*//p' "$T/resp.h" | tr -d '\r' | head -n 1)
  rpc "$TF1" '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
  TOOLS=$(printf %s "$BODY" | jq -r '.result.tools[]?.name' 2>/dev/null)
  if printf '%s\n' "$TOOLS" | grep -qx search_content; then res PASS 3 "initialize ok, $(printf '%s\n' "$TOOLS" | grep -c .) tools listed"
  else res FAIL 3 "tools/list does not contain search_content"; fi
else res FAIL 3 "initialize failed (bad token or wrong URL?)"; fi

# --- 4 read-only search -------------------------------------------------------------------
if tool "$TF1" search_content '{"query":"test"}'; then res PASS 4 "search_content ok"; else res FAIL 4 "search_content failed: ${TXT:0:120}"; fi

# --- 5 webhook auth -----------------------------------------------------------------------
WHURL=$URL/webhooks/bookstack; WHBODY='{"event":"smoketest_noop"}'
c=$(code -X POST -H 'Content-Type: application/json' --data "$WHBODY" "$WHURL")
case $c in 401) res PASS 5 "webhook without secret -> 401";; 404) res SKIP 5 "webhook 404 (semantic search off)";; *) res FAIL 5 "webhook without secret returned $c, expected 401";; esac
if [ -z "$WH_SECRET" ]; then res SKIP 5 "webhook with secret: no --webhook-secret-env"; else
  c=$(code -X POST -H "@$TFW" -H 'Content-Type: application/json' --data "$WHBODY" "$WHURL")
  case $c in 202) res PASS 5 "webhook with secret -> 202";; 404) res SKIP 5 "webhook 404 (semantic search off)";; *) res FAIL 5 "webhook with secret returned $c, expected 202";; esac
fi

# --- 6-9 write steps ----------------------------------------------------------------------
if [ "$WRITE" = 0 ]; then
  for i in 6 7 8 9; do res SKIP $i "needs --write"; done
else
  STAMP=$(date -u +%Y%m%dT%H%M%SZ); MARK=bsmcpsmoke$(date +%s)$RANDOM; PID=
  NAME="[bsmcp-smoketest] $STAMP"
  if tool "$TF1" create_page "$(jq -nc --arg n "$NAME" --argjson b "$BOOK" --arg m "# Smoke test

Marker: $MARK" '{name:$n,book_id:$b,markdown:$m}')"; then
    PID=$(printf %s "$TXT" | jq -r '.id // empty' 2>/dev/null)
  fi
  if isnum "$PID"; then PAGES=$PID; res PASS 6 "created page $PID '$NAME'"; else res FAIL 6 "create_page failed: ${TXT:0:120}"; fi

  if ! isnum "$PID"; then for i in 7 8 9; do res SKIP $i "no temp page"; done; else
    case $IMG in *.jpg|*.jpeg) MIME=image/jpeg;; *.gif) MIME=image/gif;; *.webp) MIME=image/webp;; *) MIME=image/png;; esac
    # 7: image from URL, embedded
    if tool "$TF1" upload_image "$(jq -nc --arg n "$NAME img" --argjson p "$PID" --arg u "$IMG" --arg m "$MIME" '{name:$n,uploaded_to:$p,url:$u,embed:true,mime_type:$m}')"; then
      if tool "$TF1" get_page "{\"id\":$PID}" && printf %s "$TXT" | jq -r '(.html // "") + (.markdown // "")' 2>/dev/null | grep -Eqi '<img|!\['; then
        res PASS 7 "image uploaded from URL and embedded in page $PID"
      else res FAIL 7 "upload_image ok but page does not reference an image"; fi
    else res FAIL 7 "upload_image (url) failed: ${TXT:0:120}"; fi

    # 8: staging upload of a 1x1 PNG
    printf 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==' | base64 -d >"$T/px.png" 2>/dev/null
    SU=; SI=
    if tool "$TF1" prepare_upload '{}'; then
      SI=$(printf %s "$TXT" | jq -r '.staging_id // empty' 2>/dev/null); SU=$(printf %s "$TXT" | jq -r '.upload_url // empty' 2>/dev/null)
    fi
    if [ -z "$SI" ] || [ -z "$SU" ]; then res FAIL 8 "prepare_upload failed: ${TXT:0:120}"; else
      case $SU in /*) SU=$URL$SU;; esac
      AUTH=(); case $SU in "$URL"/*) AUTH=(-H "@$TF1");; esac  # never send the token to another host
      c=$(code -X POST "${AUTH[@]}" -F "file=@$T/px.png;type=image/png" "$SU")
      case $c in 2??) ;; *) c= ;; esac
      if [ -z "$c" ]; then res FAIL 8 "staging file POST was rejected"
      elif tool "$TF1" upload_image "$(jq -nc --arg n "$NAME staged" --argjson p "$PID" --arg s "$SI" '{name:$n,uploaded_to:$p,staging_id:$s,embed:true,mime_type:"image/png"}')"; then
        res PASS 8 "staged upload ok"
      else res FAIL 8 "upload_image (staging_id) failed: ${TXT:0:120}"; fi
    fi

    # 9: semantic index picks up the page
    if ! printf '%s\n' "$TOOLS" | grep -qx semantic_search; then res SKIP 9 "semantic_search not enabled"; else
      el=0; found=0
      while :; do
        if tool "$TF1" semantic_search "$(jq -nc --arg q "$MARK" '{query:$q,mode:"standard"}')" && printf %s "$TXT" | grep -qF "$MARK"; then found=1; break; fi
        [ "$el" -ge "$WAIT" ] && break
        sleep "${SMOKE_POLL_SLEEP:-15}"; el=$((el+15))
      done
      if [ $found = 1 ]; then res PASS 9 "temp page found by semantic_search after ~${el}s"
      else res FAIL 9 "page not indexed within ${WAIT}s: webhook not delivered? (delta walk interval default 300s)"; fi
    fi
  fi
fi
cleanup

# --- 10 persistence -----------------------------------------------------------------------
res SKIP 10 "manual: restart the container, then re-run the read-only steps (without --write)"

# --- 11 multi-user isolation --------------------------------------------------------------
if [ -n "$TOK2" ] && [ -n "$PRIV" ] && [ "$TOK1" != "$TOK2" ]; then
  if ! tool "$TF1" get_page "{\"id\":$PRIV}"; then res FAIL 11 "user 1 cannot read page $PRIV: pick a page user 1 can see"
  elif ! tool "$TF2" search_content '{"query":"test"}'; then res FAIL 11 "user 2 token does not work (denial would prove nothing)"
  elif tool "$TF2" get_page "{\"id\":$PRIV}"; then res FAIL 11 "user 2 CAN read page $PRIV: isolation broken"
  else res PASS 11 "user 1 reads page $PRIV, user 2 is denied"; fi
else res SKIP 11 "NOT VERIFIED: needs two real users with different permissions"; fi

say "---"
say "summary: $FAILS failure(s)"
[ "$FAILS" = 0 ]
