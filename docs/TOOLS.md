# MCP tools and API reference

[Project home](../README.md) · [Configuration](CONFIGURATION.md)

## Available Tools (59 BookStack + 3 semantic = 62)

| Category | Tools |
|----------|-------|
| **Search** | `search_content` |
| **Semantic** | `semantic_search`, `reembed`, `embedding_status` |
| **Shelves** | `list_shelves`, `get_shelf`, `create_shelf`, `update_shelf`, `delete_shelf` |
| **Books** | `list_books`, `get_book`, `create_book`, `update_book`, `delete_book` |
| **Chapters** | `list_chapters`, `get_chapter`, `create_chapter`, `update_chapter`, `delete_chapter` |
| **Pages** | `list_pages`, `get_page`, `create_page`, `update_page`, `delete_page`, `edit_page`, `append_to_page`, `replace_section`, `insert_after` |
| **Move** | `move_page`, `move_chapter`, `move_book_to_shelf` |
| **Attachments** | `list_attachments`, `get_attachment`, `create_attachment`, `update_attachment`, `delete_attachment`, `upload_attachment` |
| **Staging** | `prepare_upload` (used with `upload_image` / `upload_attachment` for local file uploads) |
| **Exports** | `export_page`, `export_chapter`, `export_book` (markdown, plaintext, html) |
| **Comments** | `list_comments`, `get_comment`, `create_comment`, `update_comment`, `delete_comment` |
| **Recycle Bin** | `list_recycle_bin`, `restore_recycle_bin_item`, `destroy_recycle_bin_item` |
| **Users** | `list_users`, `get_user` |
| **Audit Log** | `list_audit_log` |
| **System** | `get_system_info` |
| **Images** | `list_images`, `get_image`, `upload_image`, `update_image`, `delete_image` |
| **Permissions** | `get_content_permissions`, `update_content_permissions` |
| **Roles** | `list_roles`, `get_role` |

Semantic tools (`semantic_search`, `reembed`, `embedding_status`) only appear when `BSMCP_SEMANTIC_SEARCH=true` and an embedder is running. Without semantic search: 59 BookStack tools.

With `BSMCP_BACKEND=libstack` the same 59 names stay and three more appear — `list_collections`, `get_collection`, `create_collection` — see [LibStack backend](CONFIGURATION.md#libstack-backend).

The server is a thin BookStack CRUD facade plus semantic-search enrichment, OAuth, audit, and the reconciliation worker. Personal-memory primitives (journals, identities, reminders) and the v0.8.0/v0.9.0 briefing surface were removed in v0.10.0; v0.11.0 added the optional cross-encoder reranker on the embedder side; v0.13.0 (current) refactors that reranker from a third `semantic_search` mode into a flag on both `semantic_search` and `search_content` (breaking — see the v0.12 → v0.13 migration below). See the migration notes below.

## API Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/mcp/sse` | SSE connection (MCP 2024-11-05) |
| `POST` | `/mcp/sse` | Streamable HTTP (MCP 2025-03-26) |
| `POST` | `/mcp/messages/?sessionId=<id>` | Send MCP JSON-RPC messages (SSE transport) |
| `GET` | `/health` | Health check |
| `POST` | `/webhooks/bookstack` | BookStack webhook receiver (semantic search) |
| `GET` | `/status` | Embedding progress page with live progress bar |
| `GET` | `/.well-known/oauth-authorization-server` | OAuth metadata (RFC 8414) |
| `GET` | `/.well-known/oauth-protected-resource` | Protected resource metadata (RFC 9728) |
| `GET` | `/authorize` | Login form for BookStack API token |
| `POST` | `/authorize` | Validate credentials and issue auth code |
| `POST` | `/token` | OAuth token exchange |
| `POST` | `/register` | Dynamic client registration (RFC 7591) |

## Search Operators

The `search_content` tool supports BookStack's search operators:

- `"exact phrase"` - Exact match
- `{type:page}` - Filter by type (page, chapter, book, shelf)
- `{in_name:term}` - Search within names only
- `{created_by:me}` - Filter by creator
- `[tag_name=value]` - Filter by tag

## Uploading Local Files (Images & Attachments)

The MCP server runs in a container and cannot read files from the client machine's filesystem directly. To upload local images or file attachments, use the two-step **staging upload flow**:

**Step 1:** Call `prepare_upload` — returns a `staging_id` and a full `upload_url`:

```json
{
  "staging_id": "f0103f6c-7c98-46c2-adbe-606ba26937c3",
  "upload_url": "https://your-mcp-host/stage/upload/f0103f6c-7c98-46c2-adbe-606ba26937c3",
  "ttl_seconds": 300
}
```

**Step 2:** POST the file to `upload_url` as multipart form-data. No auth header needed — the `staging_id` (a UUID that can only be generated via an authenticated MCP call) acts as the auth token for the one-time upload:

```bash
curl -X POST -F "file=@/path/to/image.jpg" \
  "https://your-mcp-host/stage/upload/f0103f6c-7c98-46c2-adbe-606ba26937c3"
```

**Step 3:** Call `upload_image` (or `upload_attachment`) with the `staging_id`:

```json
{
  "name": "Banner Logo",
  "uploaded_to": 1908,
  "staging_id": "f0103f6c-7c98-46c2-adbe-606ba26937c3",
  "mime_type": "image/jpeg",
  "embed": true
}
```

The staging slot is **consumed on first use** (destructively removed from the store) and **auto-expires after 5 minutes**. Maximum file size is 50MB.

### The `embed` parameter

`upload_image` accepts an `embed` boolean parameter (default `false`). When `embed=true`, the image is automatically appended to the target page's content after uploading, so you don't need a separate `edit_page` or `append_to_page` call. Works for both markdown and WYSIWYG pages.

### Alternative: `url` parameter

If the file is already hosted at a public URL the MCP server can reach, you can skip the staging flow entirely and pass the `url` parameter directly to `upload_image` or `upload_attachment`. The server will fetch the file and forward it to BookStack.

### Currently Claude Code only

**The staging upload flow currently only works from [Claude Code](https://claude.com/claude-code) (the CLI tool).** It does not work from Claude.ai's web custom connectors or Claude Desktop custom connectors.

The reason: Step 2 requires the MCP client to make an outbound HTTP POST to the MCP server's staging endpoint with the file bytes. Claude Code runs locally and has shell access (via its `Bash` tool), so it can `curl` the file directly. Claude.ai's remote MCP connector runs the MCP client inside Anthropic's sandboxed proxy infrastructure, which does not expose a mechanism for the client to make arbitrary HTTP file uploads to third-party hosts. Claude Desktop has similar limitations today.

If you're using Claude.ai or Claude Desktop, you can still use `upload_image` with the `url` parameter for files that are already web-accessible, or upload through the BookStack web UI directly.

