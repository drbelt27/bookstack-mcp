#!/usr/bin/env bash
# Plain-bash tests for scripts/smoke-test.sh using a stub `curl` on PATH (no network).
# Run: bash scripts/tests/test-smoke.sh
set -u
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
S=$ROOT/scripts/smoke-test.sh
W=$(mktemp -d); trap 'rm -rf "$W"' EXIT
FAILS=0 PASS=0
ok() { PASS=$((PASS+1)); echo "ok   - $1"; }
bad() { FAILS=$((FAILS+1)); echo "FAIL - $1"; }
check() { local n=$1; shift; if "$@" >/dev/null 2>&1; then ok "$n"; else bad "$n"; fi; }
command -v jq >/dev/null || { echo "jq required"; exit 1; }

# --- stub curl: behaves like the server; logs "METHOD-OR-TOOL url" per call ---------------------
mkdir -p "$W/bin"
cat >"$W/bin/curl" <<'STUB'
#!/usr/bin/env bash
out=; wfmt=; dump=; data=; url=; hdrs=(); form=0
while [ $# -gt 0 ]; do
  case $1 in
    -o) out=$2; shift;; -w) wfmt=$2; shift;; -D) dump=$2; shift;;
    -H) hdrs+=("$2"); shift;; --data|-d) data=$2; shift;;
    -F) form=1; shift;; -X|-m) shift;; -sS|-s|-S) ;;
    http*) url=$1;;
  esac
  shift
done
hv=; for h in "${hdrs[@]:-}"; do case $h in @*) hv+=$(cat "${h#@}");; *) hv+="$h";; esac; hv+=$'\n'; done
user=0; case $hv in *SECRETONE*) user=1;; *SECRETTWO*) user=2;; esac
reply() { # reply status body
  [ -n "$dump" ] && printf 'HTTP/1.1 %s\r\nMcp-Session-Id: sess1\r\n\r\n' "$1" >"$dump"
  if [ -n "$out" ]; then printf %s "$2" >"$out"; else printf %s "$2"; fi
  [ -n "$wfmt" ] && printf %s "$1"
  exit 0
}
txt() { reply 200 "$(jq -nc --arg t "$1" '{jsonrpc:"2.0",id:1,result:{content:[{type:"text",text:$t}]}}')"; }
err() { reply 200 '{"jsonrpc":"2.0","id":1,"result":{"isError":true,"content":[{"type":"text","text":"boom"}]}}'; }
path=${url#*://*/}; path=/${path%%\?*}
case $path in
  /health) echo "GET $path" >>"$STUB_LOG"; reply 200 ok;;
  /.well-known/*) echo "GET $path" >>"$STUB_LOG"; reply 200 '{}';;
  /webhooks/bookstack)
    echo "POST $path" >>"$STUB_LOG"
    case $hv in *"X-Webhook-Secret: whsec"*) reply 202 '';; *) reply 401 '';; esac;;
  /staging/*) echo "STAGING-UPLOAD $path form=$form" >>"$STUB_LOG"; reply 200 '{}';;
  /mcp/sse)
    [ "$user" = 0 ] && { echo "RPC-UNAUTH" >>"$STUB_LOG"; reply 401 ''; }
    m=$(jq -r .method <<<"$data"); n=$(jq -r '.params.name // ""' <<<"$data")
    echo "RPC user=$user $m $n $(jq -c '.params.arguments // {}' <<<"$data")" >>"$STUB_LOG"
    case $m in
      initialize) reply 200 '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26"}}';;
      tools/list)
        t='"search_content","create_page","get_page","delete_page","upload_image","prepare_upload"'
        [ "${S_SEMANTIC:-0}" = 1 ] && t+=',"semantic_search"'
        reply 200 "{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[$(jq -nc "[$t]|map({name:.})" | sed 's/^\[//;s/\]$//')]}}";;
      tools/call)
        case $n in
          search_content) txt '{"results":[]}';;
          create_page) txt '{"id":42,"name":"x"}';;
          delete_page) txt '{"deleted":true}';;
          upload_image) [ "${S_FAIL_UPLOAD:-0}" = 1 ] && err; txt '{"id":7}';;
          prepare_upload) txt '{"staging_id":"stg1","upload_url":"http://stub.test/staging/stg1"}';;
          get_page)
            id=$(jq -r .params.arguments.id <<<"$data")
            if [ "$id" = 99 ]; then [ "$user" = 2 ] && [ "${S_USER2_READ:-0}" != 1 ] && err; txt '{"id":99,"markdown":"secret"}'; fi
            txt '{"id":42,"html":"<p><img src=\"/uploads/x.png\"></p>","markdown":"![x](/uploads/x.png)"}';;
          semantic_search) q=$(jq -r .params.arguments.query <<<"$data"); txt "{\"results\":[{\"page\":\"$q\"}]}";;
        esac;;
    esac;;
esac
reply 404 ''
STUB
chmod +x "$W/bin/curl"
export PATH="$W/bin:$PATH" STUB_LOG="$W/curl.log" SMOKE_POLL_SLEEP=0
export T1='id1:SECRETONE' T2='id2:SECRETTWO' WHS=whsec
URL=http://stub.test

run() { : >"$STUB_LOG"; OUT=$("$S" --url "$URL" --token-env T1 "$@" 2>&1 </dev/null); RC=$?; }
called() { grep -q "$1" "$STUB_LOG"; }
has() { grep -q -- "$1" <<<"$OUT"; }
WRITE_RE='RPC user=. tools/call (create_page|delete_page|upload_image)'

# (f) syntax + help
check "bash -n" bash -n "$S"
check "bash -n tests" bash -n "$0"
OUT=$("$S" --help 2>&1); [ $? = 0 ] && has "Not covered" && ok "--help" || bad "--help"

# (a) read-only
run --webhook-secret-env WHS
[ $RC = 0 ] && ok "(a) read-only exit 0" || { bad "(a) read-only exit $RC"; echo "$OUT"; }
grep -Eq "$WRITE_RE" "$STUB_LOG" && bad "(a) no write tool called" || ok "(a) no write tool called"
has "SKIP  6  needs --write" && ok "(a) write steps skipped" || bad "(a) write steps skipped"
has "PASS  5  webhook without secret -> 401" && has "PASS  5  webhook with secret -> 202" && ok "(a) webhook 401/202" || bad "(a) webhook 401/202"

# webhook stub really rejects: a wrong/missing secret must be SKIP-with-secret-absent, not PASS
run
has "SKIP  5  webhook with secret: no --webhook-secret-env" && ok "webhook secret absent -> SKIP" || bad "webhook secret absent -> SKIP"

# bad token -> FAIL (stub returns 401 on unauthenticated RPC)
OUT=$(T1=nocolon:nope "$S" --url "$URL" --token-env T1 2>&1 </dev/null); RC=$?
[ $RC -ne 0 ] && ok "unknown token fails" || bad "unknown token fails"

# (b) --write without --yes and no stdin
run --write --book-id 5
[ $RC -ne 0 ] && ok "(b) refuses without confirmation" || bad "(b) refuses without confirmation"
grep -Eq "$WRITE_RE" "$STUB_LOG" && bad "(b) no write tool called" || ok "(b) no write tool called"
OUT=$("$S" --url "$URL" --token-env T1 --write --book-id 5 2>&1 <<<"no"); [ $? -ne 0 ] && ok "(b) 'no' refuses" || bad "(b) 'no' refuses"

# (c) --write --yes
run --write --yes --book-id 5
[ $RC = 0 ] && ok "(c) write run exit 0" || { bad "(c) write run exit $RC"; echo "$OUT"; }
grep -q 'create_page {.*"name":"\[bsmcp-smoketest\] ' "$STUB_LOG" && ok "(c) page has [bsmcp-smoketest] prefix" || bad "(c) page prefix"
grep -q 'delete_page {"id":42}' "$STUB_LOG" && ok "(c) same page id 42 deleted" || bad "(c) same page id deleted"
has "recycle bin" && ok "(c) recycle bin note" || bad "(c) recycle bin note"
has "SKIP  9  semantic_search not enabled" && ok "(c) semantic SKIP when disabled" || bad "(c) semantic SKIP"
has "PASS  8" && ok "(c) staging upload" || bad "(c) staging upload"
S_SEMANTIC=1 run --write --yes --book-id 5; has "PASS  9" && ok "(c) semantic PASS" || bad "(c) semantic PASS"

# (c) failure mid-way: upload_image errors -> exit 1 but delete_page still called
S_FAIL_UPLOAD=1 run --write --yes --book-id 5
[ $RC = 1 ] && ok "(c) failing step -> exit 1" || bad "(c) failing step -> exit $RC"
grep -q 'delete_page {"id":42}' "$STUB_LOG" && ok "(c) cleanup ran after failure" || bad "(c) cleanup ran after failure"
has "FAIL  7" && ok "(c) FAIL line printed" || bad "(c) FAIL line printed"

# (d) isolation
run
has "SKIP  11  NOT VERIFIED" && ok "(d) single token -> NOT VERIFIED" || bad "(d) single token -> NOT VERIFIED"
has "PASS  11" && bad "(d) never PASS from one account" || ok "(d) never PASS from one account"
run --token2-env T2
has "NOT VERIFIED" && ok "(d) token2 without page id -> NOT VERIFIED" || bad "(d) token2 without page id"
run --token2-env T2 --private-page-id 99
has "PASS  11" && [ $RC = 0 ] && ok "(d) user 2 denied -> PASS" || { bad "(d) user 2 denied -> PASS"; echo "$OUT"; }
S_USER2_READ=1 run --token2-env T2 --private-page-id 99
has "FAIL  11" && [ $RC = 1 ] && ok "(d) user 2 can read -> FAIL" || bad "(d) user 2 can read -> FAIL"

# (e) secrets never printed
run --token2-env T2 --private-page-id 99 --webhook-secret-env WHS --write --yes --book-id 5
case $OUT in *SECRETONE*|*SECRETTWO*|*whsec*) bad "(e) no secrets in output";; *) ok "(e) no secrets in output";; esac
grep -q 'SECRET\|whsec' "$STUB_LOG" && bad "(e) no secrets in argv/log" || ok "(e) no secrets in argv/log"

echo "---"; echo "passed: $PASS  failed: $FAILS"
[ "$FAILS" = 0 ]
