//! Admin-only "System" section of `/status`: versions, last index/sweep
//! runs, last webhook (received and rejected), backups, and a button that
//! runs the doctor checks. Non-admins keep the page exactly as before.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use bsmcp_common::db::IndexDb;
use bsmcp_common::settings::hash_token_id;

use crate::doctor::{self, Status};
use crate::settings_ui;
use crate::sse::{self, AppState};

const ADMIN_CACHE_TTL: Duration = Duration::from_secs(300);
/// Unauthenticated webhook rejections write to the DB at most this often, so
/// a flood of bad requests cannot turn into a flood of writes.
const REJECT_WRITE_GAP_SECS: i64 = 60;

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// "5 min ago" for a unix timestamp; the raw (escaped) value if it is not one.
fn ago(raw: &str) -> String {
    let Ok(ts) = raw.trim().parse::<i64>() else {
        return esc(raw);
    };
    let d = (now() - ts).max(0);
    match d {
        0..=89 => format!("{d}s ago"),
        90..=5399 => format!("{} min ago", d / 60),
        5400..=129_599 => format!("{} h ago", d / 3600),
        _ => format!("{} days ago", d / 86400),
    }
}

// --- Webhook bookkeeping (called from handle_webhook) ---

pub async fn record_webhook_accepted(db: &dyn IndexDb, payload: &serde_json::Value) {
    let event: String = payload
        .get("event")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(40)
        .collect();
    let _ = db
        .set_index_meta("last_webhook_at", &now().to_string())
        .await;
    let _ = db.set_index_meta("last_webhook_event", &event).await;
}

pub async fn record_webhook_rejected(db: &dyn IndexDb) {
    static LAST_WRITE: AtomicI64 = AtomicI64::new(0);
    let t = now();
    let last = LAST_WRITE.load(Ordering::Relaxed);
    if t - last < REJECT_WRITE_GAP_SECS
        || LAST_WRITE
            .compare_exchange(last, t, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
    {
        return;
    }
    let _ = db
        .set_index_meta("last_webhook_rejected_at", &t.to_string())
        .await;
}

// --- Admin detection ---

/// Credentials of the caller: Bearer token or settings-session cookie.
async fn caller_credentials(state: &AppState, headers: &HeaderMap) -> Option<(String, String)> {
    if let Ok(c) = sse::resolve_credentials(
        headers,
        state.db.as_ref(),
        &state.known_urls,
        state.backend.kind,
    )
    .await
    {
        return Some(c);
    }
    settings_ui::resolve_session(headers, &state.settings_sessions).await
}

/// Admin verdict, cached per token so an auto-refreshing page does not
/// spend the token's API budget on `is_admin` every 5 seconds.
async fn is_admin(state: &AppState, headers: &HeaderMap) -> bool {
    static CACHE: OnceLock<Mutex<HashMap<String, (Instant, bool)>>> = OnceLock::new();
    let Some((id, secret)) = caller_credentials(state, headers).await else {
        return false;
    };
    let key = hash_token_id(&id);
    let cache = CACHE.get_or_init(Default::default);
    if let Some((at, v)) = cache.lock().unwrap().get(&key) {
        if at.elapsed() < ADMIN_CACHE_TTL {
            return *v;
        }
    }
    let verdict = state
        .backend_client(&id, &secret)
        .is_admin()
        .await
        .unwrap_or(false);
    let mut map = cache.lock().unwrap();
    if map.len() > 256 {
        map.clear();
    }
    map.insert(key, (Instant::now(), verdict));
    verdict
}

// --- Rendering ---

fn row(label: &str, value: &str) -> String {
    format!(
        r#"<div class="stat-row"><span class="stat-label">{}</span><span class="stat-value">{}</span></div>"#,
        esc(label),
        value
    )
}

async fn newest_backup(dir: std::path::PathBuf) -> Option<i64> {
    tokio::task::spawn_blocking(move || {
        std::fs::read_dir(dir)
            .ok()?
            .filter_map(|e| e.ok()?.metadata().ok()?.modified().ok())
            .max()
            .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
    })
    .await
    .ok()
    .flatten()
}

/// The System card, or an empty string for non-admins.
pub async fn render(state: &AppState, headers: &HeaderMap) -> String {
    if !is_admin(state, headers).await {
        return String::new();
    }
    let meta =
        |k: &'static str| async move { state.index_db.get_index_meta(k).await.ok().flatten() };
    let mut out = String::from(
        r#"<div class="card"><h2 style="font-size:1rem;margin-bottom:.75rem">System (admin)</h2>"#,
    );

    out.push_str(&row("Server", &format!("v{}", env!("CARGO_PKG_VERSION"))));
    out.push_str(&row("Backend", &esc(state.backend.kind.as_str())));

    if let Some(sem) = &state.semantic {
        match sem.embedder_health().await {
            Some(h) => {
                let s = |k: &str| h.get(k).and_then(|v| v.as_str()).unwrap_or("-").to_string();
                out.push_str(&row(
                    "Embedder",
                    &format!("v{} ({})", esc(&s("version")), esc(&s("role"))),
                ));
                let load = h
                    .pointer("/load/state")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-");
                out.push_str(&row("Embedder state", &esc(load)));
                out.push_str(&row(
                    "Model",
                    &format!(
                        "{} ({} dims)",
                        esc(&s("model")),
                        h.get("dimensions").and_then(|v| v.as_i64()).unwrap_or(0)
                    ),
                ));
                let rr = h
                    .pointer("/reranker/model")
                    .and_then(|v| v.as_str())
                    .map(esc)
                    .unwrap_or_else(|| "none".into());
                out.push_str(&row("Reranker", &rr));
            }
            None => out.push_str(&row(
                "Embedder",
                "<span style=\"color:#f87171\">unreachable</span>",
            )),
        }
        if let Some(n) = sem.acl_page_count().await {
            out.push_str(&row("Pages with stored ACL", &n.to_string()));
        }
    }

    for (label, key) in [
        ("Last full walk", "last_full_walk_at"),
        ("Last delta walk", "last_delta_walk_at"),
        ("Last deletion sweep", "last_deletion_sweep_at"),
    ] {
        let v = meta(key)
            .await
            .map(|v| ago(&v))
            .unwrap_or_else(|| "never".into());
        out.push_str(&row(label, &v));
    }

    let received = meta("last_webhook_at").await;
    let event = meta("last_webhook_event").await.unwrap_or_default();
    out.push_str(&row(
        "Last webhook received",
        &received
            .map(|v| format!("{} ({})", ago(&v), esc(&event)))
            .unwrap_or_else(|| "never".into()),
    ));
    if let Some(v) = meta("last_webhook_rejected_at").await {
        out.push_str(&row(
            "Last webhook rejected (bad/missing secret)",
            &format!("<span style=\"color:#fbbf24\">{}</span>", ago(&v)),
        ));
    }

    let (interval, dir) = state.backup_info();
    let backup = match (interval, newest_backup(dir).await) {
        (None, _) => "disabled".to_string(),
        (Some(h), Some(ts)) => format!("every {h} h, last {}", ago(&ts.to_string())),
        (Some(h), None) => format!("every {h} h, none found yet"),
    };
    out.push_str(&row("Backups", &esc(&backup)));

    out.push_str(
        r##"<form onsubmit="event.preventDefault();var o=document.getElementById('chk');o.textContent='Running...';fetch('/status/checks',{method:'POST',credentials:'same-origin'}).then(function(r){return r.text()}).then(function(t){o.innerHTML=t});" style="margin-top:.75rem"><button type="submit">Run diagnostics</button></form><div id="chk" style="margin-top:.75rem"></div></div>"##,
    );
    out
}

fn status_colour(s: Status) -> &'static str {
    match s {
        Status::Pass => "#4ade80",
        Status::Warn => "#fbbf24",
        Status::Fail => "#f87171",
        Status::Skip => "#94a3b8",
    }
}

/// `POST /status/checks`: run the doctor and return an HTML fragment.
/// Admin + settings-session only, same-origin, one run at a time.
pub async fn handle_checks(State(state): State<AppState>, headers: HeaderMap) -> Response {
    static RUNNING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    if !settings_ui::settings_post_origin_allowed(&headers, &state.known_urls)
        || settings_ui::resolve_session(&headers, &state.settings_sessions)
            .await
            .is_none()
        || !is_admin(&state, &headers).await
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Ok(_guard) = RUNNING.try_lock() else {
        return Html("Diagnostics already running, try again shortly.".to_string()).into_response();
    };
    let checks = doctor::collect_checks().await;
    let mut html = String::new();
    for c in &checks {
        html.push_str(&format!(
            r#"<div style="margin-bottom:.5rem"><span style="color:{};font-weight:600">{}</span> <b>{}</b>{} &mdash; {}"#,
            status_colour(c.status),
            c.status.as_str(),
            esc(&c.id),
            if c.kind == doctor::Kind::Checked { "" } else { " <i>(hypothesis)</i>" },
            esc(&c.cause),
        ));
        for (label, text) in [("Impact", &c.impact), ("Fix", &c.fix)] {
            if !text.is_empty() {
                html.push_str(&format!("<br><small>{label}: {}</small>", esc(text)));
            }
        }
        html.push_str("</div>");
    }
    html.push_str("<small>Host-level checks (firewall, Docker, reverse proxy) are not possible from inside the container.</small>");
    Html(html).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ago_formats_unix_and_escapes_everything_else() {
        let t = now();
        assert_eq!(ago(&(t - 5).to_string()), "5s ago");
        assert_eq!(ago(&(t - 600).to_string()), "10 min ago");
        assert_eq!(ago(&(t - 7200).to_string()), "2 h ago");
        assert_eq!(ago(&(t - 3 * 86400).to_string()), "3 days ago");
        assert_eq!(ago("<script>"), "&lt;script&gt;");
    }

    fn temp_db() -> bsmcp_db_sqlite::SqliteDb {
        let path = std::env::temp_dir().join(format!("bsmcp-status-{}.db", uuid::Uuid::new_v4()));
        bsmcp_db_sqlite::SqliteDb::open(&path, "test-encryption-key-thirty-two-chars-long")
    }

    #[tokio::test]
    async fn webhook_bookkeeping_sanitizes_the_event_and_throttles_rejections() {
        let db = temp_db();
        record_webhook_accepted(&db, &serde_json::json!({"event": "page_update<script>"})).await;
        assert_eq!(
            db.get_index_meta("last_webhook_event")
                .await
                .unwrap()
                .as_deref(),
            Some("page_updatescript")
        );
        assert!(db
            .get_index_meta("last_webhook_at")
            .await
            .unwrap()
            .is_some());

        // First rejection is recorded; an immediate second one must not rewrite it.
        record_webhook_rejected(&db).await;
        let first = db.get_index_meta("last_webhook_rejected_at").await.unwrap();
        assert!(first.is_some());
        db.set_index_meta("last_webhook_rejected_at", "1")
            .await
            .unwrap();
        record_webhook_rejected(&db).await;
        assert_eq!(
            db.get_index_meta("last_webhook_rejected_at")
                .await
                .unwrap()
                .as_deref(),
            Some("1"),
            "throttled: no second write within the gap"
        );
    }

    #[test]
    fn esc_covers_html_and_attribute_metacharacters() {
        assert_eq!(
            esc(r#"<a href="x">&'"#),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#x27;"
        );
    }
}
