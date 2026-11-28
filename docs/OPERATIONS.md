# Operations: webhook, backup and smoke test

[Project home](../README.md) · [Installation](INSTALLATION.md) · [Configuration](CONFIGURATION.md)

## Webhook through your reverse proxy

**Why a proxy.** The MCP authenticates webhooks with the header `X-Webhook-Secret`. BookStack's webhook form has no field for custom headers (checked against v26.09.3: a webhook only has a name, endpoint and timeout), so BookStack's own requests are rejected with HTTP 401. Instead of putting the secret in the URL (it would land in access logs), let the reverse proxy add the header on the webhook path.

> **Security rule: the header must only be added for requests that come from BookStack itself.** If the proxy adds the secret for every caller, anyone on the Internet who can reach `/webhooks/bookstack` is authenticated by the proxy and can send forged events (`page_delete`, `chapter_delete`, full re-index requests). Restrict the path by source address, as below. Do not skip that line.

This guide is for a BookStack and an MCP on the **same host**. If BookStack runs elsewhere, restrict the path to that host's address instead of loopback.

**1. Make BookStack reach the proxy over loopback.** From the host itself, the public name often arrives with the public address as source (NAT hairpin: check the `remote address` your access log shows for a request made from the host). To get a stable `127.0.0.1` source that does not depend on any public or LAN address, resolve the MCP name to loopback on this host only:

```bash
echo "127.0.0.1 mcp.example.com" | sudo tee -a /etc/hosts      # keep a copy of /etc/hosts first
```

This affects every local program, not containers (they have their own resolver settings). The webhook keeps its normal public URL.

**2. Add the header on `/webhooks/bookstack`, loopback only.** Put the rule in its own file readable only by root (it contains the secret), and include it from the MCP vhost. Use your real `BSMCP_WEBHOOK_SECRET` value; never paste it into tickets or the repository.

Apache (`mod_headers`), file `/etc/apache2/bsmcp-webhook.conf`, mode `640`, owner root:

```apache
<Location "/webhooks/bookstack">
  Require ip 127.0.0.1 ::1
  RequestHeader set X-Webhook-Secret "YOUR_BSMCP_WEBHOOK_SECRET"
</Location>
```

Then `Include /etc/apache2/bsmcp-webhook.conf` inside the vhosts for `:80` and `:443`. With Virtualmin do it through its own command so the setting survives panel saves, and check the result before relying on it:

```bash
virtualmin modify-web --domain mcp.example.com --add-directive "Include /etc/apache2/bsmcp-webhook.conf"
apachectl configtest        # Virtualmin already reloads Apache after the change
```

Nginx, next to the existing `location /` block (same rule: only loopback may reach it):

```nginx
location = /webhooks/bookstack {
  allow 127.0.0.1;
  allow ::1;
  deny all;
  proxy_pass http://127.0.0.1:8080;
  proxy_set_header X-Webhook-Secret "YOUR_BSMCP_WEBHOOK_SECRET";
  proxy_set_header Host $host;
}
```

**3. Keep the MCP port off the network.** Publish it on localhost only (`127.0.0.1:8080:8080`, see `scripts/install.sh --help`) so nothing reaches the MCP unproxied.

**4. Configure the webhook in BookStack** (Settings > Webhooks): endpoint `https://mcp.example.com/webhooks/bookstack`, events Page Create, Page Update, Page Delete (more are handled: see [Configuration](CONFIGURATION.md)), **Active** on.
- Set a **short timeout** (a few seconds). With BookStack's default synchronous queue the webhook runs while the page is being saved, so a hung MCP would otherwise delay every save for the whole timeout.
- If `ALLOWED_SSRF_HOSTS` is set in BookStack's `.env`, it must allow the endpoint host.

**5. Test it from both sides.**

```bash
# From OUTSIDE the host: must be refused, with or without a header (never 202).
curl -s -o /dev/null -w "%{http_code}\n" -X POST https://mcp.example.com/webhooks/bookstack \
  -H 'Content-Type: application/json' -d '{"event":"smoketest_noop"}'            # expect 403
curl -s -o /dev/null -w "%{http_code}\n" -X POST https://mcp.example.com/webhooks/bookstack \
  -H 'X-Webhook-Secret: wrong' -H 'Content-Type: application/json' -d '{"event":"smoketest_noop"}'   # expect 403

# From the host itself (goes over loopback): accepted, the test event is ignored.
curl -s -o /dev/null -w "%{http_code}\n" -X POST https://mcp.example.com/webhooks/bookstack \
  -H 'Content-Type: application/json' -d '{"event":"smoketest_noop"}'            # expect 202
```

Then edit a page in BookStack and open `/status` (admin): **Last webhook received** must show a few seconds ago. **Last webhook rejected** means a request arrived without the right header: the rule is not matching, or BookStack is not arriving from loopback.

**Roll back.** Remove the line from `/etc/hosts`, run `virtualmin modify-web --domain mcp.example.com --remove-directive "Include /etc/apache2/bsmcp-webhook.conf"`, and delete the file.

**Limits to know.**
- BookStack does not retry a failed webhook. An event lost while the MCP is down is lost; the periodic delta walk (`BSMCP_INDEX_DELTA_INTERVAL_SECONDS`) catches modified pages and, since the deletion sweep, deleted pages (hourly). Chapter/book/shelf deletions and permission changes still rely on webhooks or the next full walk.
- Where you cannot put a proxy in front of the MCP, leave the webhook disabled and rely on the periodic walks. Do not send unauthenticated requests or put the secret in the query string (it is still accepted but deprecated, and it leaks into logs).

## Backup and restore

SQLite deployments: the server writes a consistent snapshot every `BSMCP_BACKUP_INTERVAL` hours (set it; it is off by default) to `BSMCP_BACKUP_PATH` (`/data/backups`, inside the `bsmcp-data` volume) as `bookstack-mcp-backup-YYYYMMDDTHHMMSS.db`. `/status` shows the age of the newest one. `scripts/update.sh` also takes a backup before touching anything.

PostgreSQL deployments: the server does **not** back up (the call is a no-op). Dump externally:

```bash
docker compose -f docker/docker-compose.yml exec -T bsmcp-postgres pg_dump -U bsmcp bsmcp > bsmcp-$(date +%F).sql
```

Copy backups off the host: a backup in the same volume does not survive losing the disk. The model cache (`bsmcp-models`) needs no backup, it is downloaded again.

**Restore (SQLite).** This replaces the live database, so stop the services that use it first; OAuth sessions and settings go back to the backup's state.

```bash
cd /opt/bookstack-mcp
docker compose stop bsmcp-server bsmcp-embedder bsmcp-worker
docker volume ls | grep bsmcp-data          # note the exact name, usually <project>_bsmcp-data
docker run --rm -v <that-volume>:/data alpine sh -c \
  'ls -l /data/backups && cp /data/backups/bookstack-mcp-backup-YYYYMMDDTHHMMSS.db /data/bookstack-mcp.db && rm -f /data/bookstack-mcp.db-wal /data/bookstack-mcp.db-shm'
docker compose up -d bsmcp-server bsmcp-embedder bsmcp-worker
```

**Restore (PostgreSQL).** Stop server, embedder and worker, then `docker compose exec -T bsmcp-postgres psql -U bsmcp bsmcp < bsmcp-YYYY-MM-DD.sql` into an empty database, then start them again.

These restore steps are documented from the code and compose files, not yet exercised on a real server: try them once on a test copy before you need them. Never use `docker compose down -v`: it deletes the volumes, backups included.

## Smoke test (optional)

`scripts/smoke-test.sh` checks a running installation from the outside: health, OAuth metadata, MCP connection, read access, webhook authentication, and (only with `--write`) creating a temporary page, uploading an image from a URL and through staging, and waiting for semantic indexing. It needs `bash`, `curl` and `jq`. Run `scripts/smoke-test.sh --help` for the flags.

- Read-only by default. Everything that writes needs `--write` plus a typed confirmation (or `--yes`), uses resources named `[bsmcp-smoketest] <timestamp>`, and deletes them on exit; deleted pages stay in the BookStack recycle bin.
- Tokens are read from environment variables named on the command line, never from arguments.
- **Multi-user isolation is only reported as verified when two real users with different permissions are supplied** (`--token2-env` and `--private-page-id`); with one account the step is `SKIP ... NOT VERIFIED`.
- Not covered: attachment upload, ACL reconciliation after a permission change, persistence after a restart (restart the container and re-run the read-only steps yourself).
