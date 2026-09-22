//! The web admin panel: an HTTP API plus a small single-page UI served from
//! the server binary itself, so there is nothing extra to deploy.
//!
//! Access control is role based (see [`auth::Role`]) and independent of
//! in-game op. The owner account is created through a one-time setup link
//! printed on first start; opped players can get a login link with `/panel`.

pub mod api;
pub mod auth;

use crate::api::{AdminHandle, AdminRequest, AdminResponse};
use crate::auth::{Auth, Role, Session};
use anyhow::Result;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct AdminConfig {
    pub bind: String,
    pub port: u16,
    /// How players reach the panel, e.g. `https://panel.example.com`. Used in
    /// the links we print and send in game. Defaults to `http://<bind>:<port>`.
    pub public_url: Option<String>,
    pub secure_cookies: bool,
}

struct AppState {
    handle: AdminHandle,
    auth: Arc<Auth>,
    config: AdminConfig,
}

/// Handle kept by the game server after the panel starts.
#[derive(Clone)]
pub struct Panel {
    auth: Arc<Auth>,
    config: AdminConfig,
}

impl Panel {
    pub fn public_url(&self) -> String {
        self.config.public_url.clone().unwrap_or_else(|| {
            let host = if self.config.bind == "0.0.0.0" { "localhost" } else { &self.config.bind };
            format!("http://{host}:{}", self.config.port)
        })
    }

    /// One-time login link for an in-game player (`/panel`).
    pub fn login_link(&self, username: &str, role: Role) -> String {
        let token = self.auth.issue_one_time(username, role);
        format!("{}/api/token-login?token={token}", self.public_url())
    }

    /// Setup link, only meaningful while no accounts exist.
    pub fn setup_link(&self) -> Option<String> {
        if self.auth.has_users() {
            return None;
        }
        let token = self.auth.issue_one_time("*setup*", Role::Owner);
        Some(format!("{}/setup?token={token}", self.public_url()))
    }

    pub fn has_users(&self) -> bool {
        self.auth.has_users()
    }
}

/// Starts the panel in the background and returns a handle for the server.
pub async fn start(config: AdminConfig, handle: AdminHandle, users_path: &std::path::Path) -> Result<Panel> {
    let auth = Arc::new(Auth::load(users_path)?);
    let state = Arc::new(AppState {
        handle,
        auth: Arc::clone(&auth),
        config: config.clone(),
    });

    let app = Router::new()
        .route("/", get(ui_index))
        .route("/setup", get(ui_index))
        .route("/app.js", get(ui_js))
        .route("/style.css", get(ui_css))
        .route("/logo.png", get(ui_logo))
        .route("/api/setup/status", get(setup_status))
        .route("/api/setup", post(setup_create))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/token-login", get(token_login))
        .route("/api/me", get(me))
        .route("/api/status", get(status))
        .route("/api/players", get(players))
        .route("/api/players/{uuid}/kick", post(kick))
        .route("/api/players/{uuid}/ban", post(ban_player))
        .route("/api/players/{uuid}/gamemode", post(gamemode))
        .route("/api/players/{uuid}/op", post(op))
        .route("/api/players/{uuid}/deop", post(deop))
        .route("/api/players/{uuid}/message", post(message))
        .route("/api/players/{uuid}/teleport", post(teleport))
        .route("/api/players/{uuid}/voice-mute", post(voice_mute))
        .route("/api/broadcast", post(broadcast))
        .route("/api/bans", get(bans).post(ban_name))
        .route("/api/bans/{target}", axum::routing::delete(pardon))
        .route("/api/whitelist", get(whitelist).post(whitelist_add))
        .route("/api/whitelist/{name}", axum::routing::delete(whitelist_remove))
        .route("/api/console", get(console_history))
        .route("/api/console/ws", get(console_ws))
        .route("/api/command", post(command))
        .route("/api/mods", get(mods))
        .route("/api/mods/reload", post(mods_reload))
        .route("/api/mods/{id}/enable", post(mod_enable))
        .route("/api/mods/{id}/disable", post(mod_disable))
        .route("/api/config", get(config_get).put(config_put))
        .route("/api/audit", get(audit))
        .route("/api/violations", get(violations))
        .route("/api/backups", get(backups).post(backup_create))
        .route("/api/save", post(save_world))
        .route("/api/stop", post(stop))
        .route("/api/users", get(users_list).post(users_create))
        .route("/api/users/{name}", axum::routing::delete(users_delete))
        .route("/api/users/{name}/password", post(users_password))
        .with_state(state);

    let addr: SocketAddr = format!("{}:{}", config.bind, config.port).parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("admin panel listening on http://{addr}");
    tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await {
            tracing::error!("admin panel stopped: {err}");
        }
    });
    Ok(Panel { auth, config })
}

// ---------- static UI ----------

async fn ui_index() -> Response {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], include_str!("../ui/index.html")).into_response()
}

async fn ui_js() -> Response {
    ([(header::CONTENT_TYPE, "application/javascript")], include_str!("../ui/app.js")).into_response()
}

async fn ui_css() -> Response {
    ([(header::CONTENT_TYPE, "text/css")], include_str!("../ui/style.css")).into_response()
}

async fn ui_logo() -> Response {
    ([(header::CONTENT_TYPE, "image/png")], include_bytes!("../ui/logo.png").as_slice()).into_response()
}

// ---------- auth helpers ----------

type Shared = State<Arc<AppState>>;

fn session_token(headers: &HeaderMap) -> Option<String> {
    if let Some(auth) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        if let Some(token) = auth.strip_prefix("Bearer ") {
            return Some(token.trim().to_owned());
        }
    }
    let cookies = headers.get(header::COOKIE)?.to_str().ok()?;
    cookies
        .split(';')
        .map(str::trim)
        .find_map(|c| c.strip_prefix("garnet_session=").map(str::to_owned))
}

/// Rejects the request unless the caller has at least `role`.
fn require(state: &AppState, headers: &HeaderMap, role: Role) -> Result<Session, Response> {
    let token = session_token(headers).ok_or_else(|| api_error(StatusCode::UNAUTHORIZED, "not logged in"))?;
    let session = state
        .auth
        .session(&token)
        .ok_or_else(|| api_error(StatusCode::UNAUTHORIZED, "session expired"))?;
    if session.role < role {
        return Err(api_error(StatusCode::FORBIDDEN, format!("requires the {} role", role.name())));
    }
    Ok(session)
}

fn api_error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

fn cookie_header(state: &AppState, token: &str, max_age: i64) -> HeaderValue {
    let secure = if state.config.secure_cookies { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "garnet_session={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={max_age}{secure}"
    ))
    .unwrap()
}

fn client_ip(headers: &HeaderMap, addr: SocketAddr) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|| addr.ip().to_string())
}

/// Turns a server answer into an HTTP response.
fn respond(response: AdminResponse) -> Response {
    match response {
        AdminResponse::Error(message) => api_error(StatusCode::BAD_REQUEST, message),
        AdminResponse::Ok => Json(json!({ "ok": true })).into_response(),
        other => Json(other).into_response(),
    }
}

// ---------- setup and login ----------

#[derive(Deserialize)]
struct TokenQuery {
    token: String,
}

async fn setup_status(State(state): Shared, Query(q): Query<HashMap<String, String>>) -> Response {
    let token_ok = q.get("token").map(|t| state.auth.peek(t)).unwrap_or(false);
    Json(json!({ "needs_setup": !state.auth.has_users(), "token_valid": token_ok })).into_response()
}

#[derive(Deserialize)]
struct SetupBody {
    token: String,
    username: String,
    password: String,
}

async fn setup_create(State(state): Shared, Json(body): Json<SetupBody>) -> Response {
    if state.auth.has_users() {
        return api_error(StatusCode::FORBIDDEN, "setup already completed");
    }
    match state.auth.redeem(&body.token) {
        Some((name, _, _)) if name == "*setup*" => {}
        _ => return api_error(StatusCode::FORBIDDEN, "invalid or expired setup link; restart the server for a new one"),
    }
    if let Err(err) = state.auth.create_user(&body.username, &body.password, Role::Owner) {
        return api_error(StatusCode::BAD_REQUEST, err.to_string());
    }
    match state.auth.login(&body.username, &body.password, "setup") {
        Ok(token) => {
            let mut headers = HeaderMap::new();
            headers.insert(header::SET_COOKIE, cookie_header(&state, &token, 12 * 3600));
            (headers, Json(json!({ "ok": true }))).into_response()
        }
        Err(err) => api_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

#[derive(Deserialize)]
struct LoginBody {
    username: String,
    password: String,
}

async fn login(
    State(state): Shared,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<LoginBody>,
) -> Response {
    let ip = client_ip(&headers, addr);
    match state.auth.login(&body.username, &body.password, &ip) {
        Ok(token) => {
            let mut out = HeaderMap::new();
            out.insert(header::SET_COOKIE, cookie_header(&state, &token, 12 * 3600));
            (out, Json(json!({ "ok": true }))).into_response()
        }
        Err(err) => api_error(StatusCode::UNAUTHORIZED, err.to_string()),
    }
}

async fn logout(State(state): Shared, headers: HeaderMap) -> Response {
    if let Some(token) = session_token(&headers) {
        state.auth.logout(&token);
    }
    let mut out = HeaderMap::new();
    out.insert(header::SET_COOKIE, cookie_header(&state, "", 0));
    (out, Json(json!({ "ok": true }))).into_response()
}

async fn token_login(State(state): Shared, Query(q): Query<TokenQuery>) -> Response {
    match state.auth.redeem(&q.token) {
        Some((_, _, Some(session))) => {
            let mut out = HeaderMap::new();
            out.insert(header::SET_COOKIE, cookie_header(&state, &session, 12 * 3600));
            (out, Redirect::to("/")).into_response()
        }
        _ => api_error(StatusCode::FORBIDDEN, "this login link is invalid or has expired"),
    }
}

async fn me(State(state): Shared, headers: HeaderMap) -> Response {
    match require(&state, &headers, Role::Viewer) {
        Ok(s) => Json(json!({ "username": s.username, "role": s.role })).into_response(),
        Err(r) => r,
    }
}

// ---------- read-only endpoints (viewer) ----------

async fn status(State(state): Shared, headers: HeaderMap) -> Response {
    if let Err(r) = require(&state, &headers, Role::Viewer) {
        return r;
    }
    respond(state.handle.call(AdminRequest::Status).await)
}

async fn players(State(state): Shared, headers: HeaderMap) -> Response {
    if let Err(r) = require(&state, &headers, Role::Viewer) {
        return r;
    }
    respond(state.handle.call(AdminRequest::Players).await)
}

async fn bans(State(state): Shared, headers: HeaderMap) -> Response {
    if let Err(r) = require(&state, &headers, Role::Viewer) {
        return r;
    }
    respond(state.handle.call(AdminRequest::Bans).await)
}

async fn whitelist(State(state): Shared, headers: HeaderMap) -> Response {
    if let Err(r) = require(&state, &headers, Role::Viewer) {
        return r;
    }
    respond(state.handle.call(AdminRequest::Whitelist).await)
}

async fn console_history(State(state): Shared, headers: HeaderMap) -> Response {
    if let Err(r) = require(&state, &headers, Role::Viewer) {
        return r;
    }
    respond(state.handle.call(AdminRequest::ConsoleHistory).await)
}

async fn mods(State(state): Shared, headers: HeaderMap) -> Response {
    if let Err(r) = require(&state, &headers, Role::Viewer) {
        return r;
    }
    respond(state.handle.call(AdminRequest::Mods).await)
}

async fn audit(State(state): Shared, headers: HeaderMap) -> Response {
    if let Err(r) = require(&state, &headers, Role::Moderator) {
        return r;
    }
    respond(state.handle.call(AdminRequest::Audit).await)
}

async fn violations(State(state): Shared, headers: HeaderMap) -> Response {
    if let Err(r) = require(&state, &headers, Role::Moderator) {
        return r;
    }
    respond(state.handle.call(AdminRequest::Violations).await)
}

async fn backups(State(state): Shared, headers: HeaderMap) -> Response {
    if let Err(r) = require(&state, &headers, Role::Admin) {
        return r;
    }
    respond(state.handle.call(AdminRequest::Backups).await)
}

async fn config_get(State(state): Shared, headers: HeaderMap) -> Response {
    if let Err(r) = require(&state, &headers, Role::Admin) {
        return r;
    }
    respond(state.handle.call(AdminRequest::ConfigText).await)
}

// ---------- moderation (moderator) ----------

#[derive(Deserialize, Default)]
struct ReasonBody {
    #[serde(default)]
    reason: String,
    hours: Option<u64>,
}

fn parse_uuid(s: &str) -> Result<Uuid, Response> {
    Uuid::parse_str(s).map_err(|_| api_error(StatusCode::BAD_REQUEST, "bad uuid"))
}

async fn kick(State(state): Shared, headers: HeaderMap, Path(uuid): Path<String>, body: Option<Json<ReasonBody>>) -> Response {
    let session = match require(&state, &headers, Role::Moderator) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let uuid = match parse_uuid(&uuid) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let reason = body.map(|b| b.0.reason).unwrap_or_default();
    respond(state.handle.call(AdminRequest::Kick { uuid, reason, actor: session.username }).await)
}

async fn ban_player(State(state): Shared, headers: HeaderMap, Path(uuid): Path<String>, body: Option<Json<ReasonBody>>) -> Response {
    let session = match require(&state, &headers, Role::Moderator) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let body = body.map(|b| b.0).unwrap_or_default();
    respond(
        state
            .handle
            .call(AdminRequest::Ban {
                target: uuid,
                reason: body.reason,
                hours: body.hours,
                actor: session.username,
            })
            .await,
    )
}

#[derive(Deserialize)]
struct BanNameBody {
    target: String,
    #[serde(default)]
    reason: String,
    hours: Option<u64>,
    #[serde(default)]
    ip: bool,
}

async fn ban_name(State(state): Shared, headers: HeaderMap, Json(body): Json<BanNameBody>) -> Response {
    let session = match require(&state, &headers, Role::Moderator) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let request = if body.ip {
        AdminRequest::BanIp {
            ip: body.target,
            reason: body.reason,
            actor: session.username,
        }
    } else {
        AdminRequest::Ban {
            target: body.target,
            reason: body.reason,
            hours: body.hours,
            actor: session.username,
        }
    };
    respond(state.handle.call(request).await)
}

async fn pardon(State(state): Shared, headers: HeaderMap, Path(target): Path<String>) -> Response {
    let session = match require(&state, &headers, Role::Moderator) {
        Ok(s) => s,
        Err(r) => return r,
    };
    respond(state.handle.call(AdminRequest::Pardon { target, actor: session.username }).await)
}

#[derive(Deserialize)]
struct TextBody {
    text: String,
}

async fn message(State(state): Shared, headers: HeaderMap, Path(uuid): Path<String>, Json(body): Json<TextBody>) -> Response {
    let session = match require(&state, &headers, Role::Moderator) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let uuid = match parse_uuid(&uuid) {
        Ok(u) => u,
        Err(r) => return r,
    };
    respond(
        state
            .handle
            .call(AdminRequest::Message {
                uuid: Some(uuid),
                text: body.text,
                actor: session.username,
            })
            .await,
    )
}

async fn broadcast(State(state): Shared, headers: HeaderMap, Json(body): Json<TextBody>) -> Response {
    let session = match require(&state, &headers, Role::Moderator) {
        Ok(s) => s,
        Err(r) => return r,
    };
    respond(
        state
            .handle
            .call(AdminRequest::Message {
                uuid: None,
                text: body.text,
                actor: session.username,
            })
            .await,
    )
}

#[derive(Deserialize)]
struct MutedBody {
    muted: bool,
}

async fn voice_mute(State(state): Shared, headers: HeaderMap, Path(uuid): Path<String>, Json(body): Json<MutedBody>) -> Response {
    let session = match require(&state, &headers, Role::Moderator) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let uuid = match parse_uuid(&uuid) {
        Ok(u) => u,
        Err(r) => return r,
    };
    respond(
        state
            .handle
            .call(AdminRequest::VoiceMute {
                uuid,
                muted: body.muted,
                actor: session.username,
            })
            .await,
    )
}

// ---------- admin ----------

#[derive(Deserialize)]
struct ModeBody {
    mode: String,
}

async fn gamemode(State(state): Shared, headers: HeaderMap, Path(uuid): Path<String>, Json(body): Json<ModeBody>) -> Response {
    let session = match require(&state, &headers, Role::Admin) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let uuid = match parse_uuid(&uuid) {
        Ok(u) => u,
        Err(r) => return r,
    };
    respond(
        state
            .handle
            .call(AdminRequest::SetGameMode {
                uuid,
                mode: body.mode,
                actor: session.username,
            })
            .await,
    )
}

async fn op(State(state): Shared, headers: HeaderMap, Path(uuid): Path<String>) -> Response {
    set_op(state, headers, uuid, true).await
}

async fn deop(State(state): Shared, headers: HeaderMap, Path(uuid): Path<String>) -> Response {
    set_op(state, headers, uuid, false).await
}

async fn set_op(state: Arc<AppState>, headers: HeaderMap, uuid: String, op: bool) -> Response {
    let session = match require(&state, &headers, Role::Admin) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let uuid = match parse_uuid(&uuid) {
        Ok(u) => u,
        Err(r) => return r,
    };
    respond(state.handle.call(AdminRequest::SetOp { uuid, op, actor: session.username }).await)
}

#[derive(Deserialize)]
struct TeleportBody {
    x: f64,
    y: f64,
    z: f64,
}

async fn teleport(State(state): Shared, headers: HeaderMap, Path(uuid): Path<String>, Json(body): Json<TeleportBody>) -> Response {
    let session = match require(&state, &headers, Role::Admin) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let uuid = match parse_uuid(&uuid) {
        Ok(u) => u,
        Err(r) => return r,
    };
    respond(
        state
            .handle
            .call(AdminRequest::Teleport {
                uuid,
                x: body.x,
                y: body.y,
                z: body.z,
                actor: session.username,
            })
            .await,
    )
}

#[derive(Deserialize)]
struct NameBody {
    name: String,
}

async fn whitelist_add(State(state): Shared, headers: HeaderMap, Json(body): Json<NameBody>) -> Response {
    let session = match require(&state, &headers, Role::Admin) {
        Ok(s) => s,
        Err(r) => return r,
    };
    respond(state.handle.call(AdminRequest::WhitelistAdd { name: body.name, actor: session.username }).await)
}

async fn whitelist_remove(State(state): Shared, headers: HeaderMap, Path(name): Path<String>) -> Response {
    let session = match require(&state, &headers, Role::Admin) {
        Ok(s) => s,
        Err(r) => return r,
    };
    respond(state.handle.call(AdminRequest::WhitelistRemove { name, actor: session.username }).await)
}

#[derive(Deserialize)]
struct CommandBody {
    command: String,
}

async fn command(State(state): Shared, headers: HeaderMap, Json(body): Json<CommandBody>) -> Response {
    let session = match require(&state, &headers, Role::Admin) {
        Ok(s) => s,
        Err(r) => return r,
    };
    respond(state.handle.call(AdminRequest::Command { command: body.command, actor: session.username }).await)
}

async fn mods_reload(State(state): Shared, headers: HeaderMap) -> Response {
    let session = match require(&state, &headers, Role::Admin) {
        Ok(s) => s,
        Err(r) => return r,
    };
    respond(state.handle.call(AdminRequest::ReloadMods { actor: session.username }).await)
}

async fn mod_enable(State(state): Shared, headers: HeaderMap, Path(id): Path<String>) -> Response {
    set_mod(state, headers, id, true).await
}

async fn mod_disable(State(state): Shared, headers: HeaderMap, Path(id): Path<String>) -> Response {
    set_mod(state, headers, id, false).await
}

async fn set_mod(state: Arc<AppState>, headers: HeaderMap, id: String, enabled: bool) -> Response {
    let session = match require(&state, &headers, Role::Admin) {
        Ok(s) => s,
        Err(r) => return r,
    };
    respond(
        state
            .handle
            .call(AdminRequest::SetModEnabled {
                id,
                enabled,
                actor: session.username,
            })
            .await,
    )
}

async fn backup_create(State(state): Shared, headers: HeaderMap) -> Response {
    let session = match require(&state, &headers, Role::Admin) {
        Ok(s) => s,
        Err(r) => return r,
    };
    respond(state.handle.call(AdminRequest::CreateBackup { actor: session.username }).await)
}

async fn save_world(State(state): Shared, headers: HeaderMap) -> Response {
    let session = match require(&state, &headers, Role::Admin) {
        Ok(s) => s,
        Err(r) => return r,
    };
    respond(state.handle.call(AdminRequest::SaveWorld { actor: session.username }).await)
}

// ---------- owner ----------

async fn config_put(State(state): Shared, headers: HeaderMap, body: String) -> Response {
    let session = match require(&state, &headers, Role::Owner) {
        Ok(s) => s,
        Err(r) => return r,
    };
    respond(state.handle.call(AdminRequest::SaveConfigText { text: body, actor: session.username }).await)
}

async fn stop(State(state): Shared, headers: HeaderMap) -> Response {
    let session = match require(&state, &headers, Role::Owner) {
        Ok(s) => s,
        Err(r) => return r,
    };
    respond(state.handle.call(AdminRequest::Stop { actor: session.username }).await)
}

async fn users_list(State(state): Shared, headers: HeaderMap) -> Response {
    if let Err(r) = require(&state, &headers, Role::Owner) {
        return r;
    }
    let users: Vec<_> = state
        .auth
        .list_users()
        .into_iter()
        .map(|(username, role)| json!({ "username": username, "role": role }))
        .collect();
    Json(users).into_response()
}

#[derive(Deserialize)]
struct UserBody {
    username: String,
    password: String,
    role: String,
}

async fn users_create(State(state): Shared, headers: HeaderMap, Json(body): Json<UserBody>) -> Response {
    if let Err(r) = require(&state, &headers, Role::Owner) {
        return r;
    }
    let Some(role) = Role::parse(&body.role) else {
        return api_error(StatusCode::BAD_REQUEST, "role must be viewer, moderator, admin or owner");
    };
    match state.auth.create_user(&body.username, &body.password, role) {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(err) => api_error(StatusCode::BAD_REQUEST, err.to_string()),
    }
}

async fn users_delete(State(state): Shared, headers: HeaderMap, Path(name): Path<String>) -> Response {
    let session = match require(&state, &headers, Role::Owner) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if session.username.eq_ignore_ascii_case(&name) {
        return api_error(StatusCode::BAD_REQUEST, "you cannot delete your own account");
    }
    match state.auth.delete_user(&name) {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(err) => api_error(StatusCode::BAD_REQUEST, err.to_string()),
    }
}

#[derive(Deserialize)]
struct PasswordBody {
    password: String,
}

async fn users_password(State(state): Shared, headers: HeaderMap, Path(name): Path<String>, Json(body): Json<PasswordBody>) -> Response {
    let session = match require(&state, &headers, Role::Viewer) {
        Ok(s) => s,
        Err(r) => return r,
    };
    // Anyone may change their own password; only owners may change others'.
    if !session.username.eq_ignore_ascii_case(&name) && session.role < Role::Owner {
        return api_error(StatusCode::FORBIDDEN, "only owners can change other users' passwords");
    }
    match state.auth.set_password(&name, &body.password) {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(err) => api_error(StatusCode::BAD_REQUEST, err.to_string()),
    }
}

// ---------- live console ----------

async fn console_ws(State(state): Shared, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    let session = match require(&state, &headers, Role::Viewer) {
        Ok(s) => s,
        Err(r) => return r,
    };
    ws.on_upgrade(move |socket| console_session(state, session, socket))
}

async fn console_session(state: Arc<AppState>, session: Session, mut socket: WebSocket) {
    // Replay recent history first so the page is not blank.
    if let AdminResponse::Lines(lines) = state.handle.call(AdminRequest::ConsoleHistory).await {
        for line in lines {
            if socket.send(Message::Text(serde_json::to_string(&line).unwrap().into())).await.is_err() {
                return;
            }
        }
    }
    let mut logs = state.handle.logs.subscribe();
    loop {
        tokio::select! {
            line = logs.recv() => match line {
                Ok(line) => {
                    if socket.send(Message::Text(serde_json::to_string(&line).unwrap().into())).await.is_err() {
                        return;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    if session.role < Role::Admin {
                        let _ = socket.send(Message::Text(json!({
                            "time": "", "level": "ERROR", "target": "panel",
                            "message": "running commands requires the admin role"
                        }).to_string().into())).await;
                        continue;
                    }
                    let command = text.trim().trim_start_matches('/').to_owned();
                    if command.is_empty() {
                        continue;
                    }
                    let answer = state.handle.call(AdminRequest::Command { command, actor: session.username.clone() }).await;
                    if let AdminResponse::Error(message) = answer {
                        let _ = socket.send(Message::Text(json!({
                            "time": "", "level": "ERROR", "target": "panel", "message": message
                        }).to_string().into())).await;
                    }
                }
                Some(Ok(Message::Close(_))) | None => return,
                Some(Ok(_)) => {}
                Some(Err(_)) => return,
            }
        }
    }
}
