# Operations: webhook, backup and smoke test

[Project home](../README.md) · [Installation](INSTALLATION.md) · [Configuration](CONFIGURATION.md)

## Webhook through your reverse proxy

**Why a proxy.** The MCP authenticates webhooks with the header `X-Webhook-Secret`. BookStack's webhook form has no field for custom headers (checked against v26.09.3: a webhook only has a name, endpoint and timeout), so BookStack's own requests are rejected with HTTP 401. Instead of putting the secret in the URL (it would land in access logs), let the reverse proxy in front of the MCP add the header on the webhook path only. The secret then never leaves your server.

**1. Add the header on `/webhooks/bookstack` only.** Use your real `BSMCP_WEBHOOK_SECRET` value; do not paste it into tickets or the repository.

Apache (`mod_headers`), inside the vhost of the MCP host, before the catch-all `ProxyPass`:

```apache
<Location "/webhooks/bookstack">
  RequestHeader set X-Webhook-Secret "YOUR_BSMCP_WEBHOOK_SECRET"
</Location>
```

Nginx, next to the existing `location /` block:

```nginx
location = /webhooks/bookstack {
  proxy_pass http://127.0.0.1:8080;
  proxy_set_header X-Webhook-Secret "YOUR_BSMCP_WEBHOOK_SECRET";
  proxy_set_header Host $host;
}
```

Restrict read access to that config file (`chmod 640`, root-owned): it now contains the secret.

**2. Close the direct port.** A caller that reaches the MCP port directly skips the proxy and, without the header, gets 401, which is safe. But if the proxy is the only thing adding the secret, publish the port on localhost only (`127.0.0.1:8080:8080`, see `scripts/install.sh --help`) so nothing else on the network can reach the MCP unproxied.

**3. Create the webhook in BookStack** (Settings > Webhooks): endpoint `https://your-mcp-host/webhooks/bookstack`, events Page Create, Page Update, Page Delete (more are handled: see [Configuration](CONFIGURATION.md)). BookStack must reach that URL; if it runs on the same host as the proxy and the public name does not resolve from inside it, use the internal URL of the proxy.

**4. Test it.**

```bash
# Through the proxy the header is added, so the MCP accepts the (ignored) test event:
curl -s -o /dev/null -w "%{http_code}\n" -X POST https://your-mcp-host/webhooks/bookstack \
  -H 'Content-Type: application/json' -d '{"event":"smoketest_noop"}'     # expect 202

# Straight to the MCP port, bypassing the proxy, there is no header: it must refuse.
curl -s -o /dev/null -w "%{http_code}\n" -X POST http://127.0.0.1:8080/webhooks/bookstack \
  -H 'Content-Type: application/json' -d '{"event":"smoketest_noop"}'     # expect 401
```

Then edit a page in BookStack and open `/status` (admin): **Last webhook received** must show a few seconds ago. **Last webhook rejected** means a request arrived without the right header: the proxy rule is not matching that path.

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
