use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use actix_web::{web, HttpRequest, HttpResponse, Responder};
use actix_web::dev::ServiceRequest;
use rand::distributions::Alphanumeric;
use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::app_config::UserConfig;

// ── State ─────────────────────────────────────────────────────────────────────

/// Shared auth state: the list of known users and the set of currently-valid
/// tokens issued by `POST /login`.
pub struct AuthState {
    pub users: Vec<UserConfig>,
    token_ttl: Duration,
    /// Maps token → expiry Instant.
    tokens: RwLock<HashMap<String, Instant>>,
}

impl AuthState {
    pub fn new(users: Vec<UserConfig>, token_ttl_seconds: u64) -> Self {
        Self {
            users,
            token_ttl: Duration::from_secs(token_ttl_seconds),
            tokens: RwLock::new(HashMap::new()),
        }
    }

    /// Check whether `token` is present **and** not yet expired.
    pub fn is_valid_token(&self, token: &str) -> bool {
        self.tokens
            .read()
            .unwrap()
            .get(token)
            .map(|&exp| Instant::now() < exp)
            .unwrap_or(false)
    }

    /// Convenience method used by the middleware: extract the Bearer token
    /// from the request headers and validate it in one step.
    ///
    /// Returns `true` when:
    ///   - auth is disabled (no users configured), OR
    ///   - the `Authorization: Bearer <token>` header is present and the token
    ///     is valid (exists and not expired).
    pub fn check_request(&self, req: &ServiceRequest) -> bool {
        if self.is_disabled() {
            return true;
        }
        req.headers()
            .get("Authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| {
                // Accept "Bearer " (standard) case-insensitively, then trim
                // any accidental surrounding whitespace from the token itself.
                let v = v.trim();
                if v.len() > 7 && v[..7].eq_ignore_ascii_case("Bearer ") {
                    Some(v[7..].trim())
                } else {
                    None
                }
            })
            .map(|t| self.is_valid_token(t))
            .unwrap_or(false)
    }

    /// Return `true` when auth is effectively disabled (no users configured).
    pub fn is_disabled(&self) -> bool {
        self.users.is_empty()
    }

    /// Generate a fresh token, store it with its expiry, and return it.
    /// Expired tokens are purged from the map on every call to keep memory bounded.
    fn issue_token(&self) -> String {
        let token = generate_token();
        let expiry = Instant::now() + self.token_ttl;
        let mut tokens = self.tokens.write().unwrap();
        // Evict any tokens that have already expired.
        tokens.retain(|_, exp| Instant::now() < *exp);
        tokens.insert(token.clone(), expiry);
        token
    }

    /// Remove a specific token (used by `POST /logout`).
    fn revoke_token(&self, token: &str) {
        self.tokens.write().unwrap().remove(token);
    }
}

// ── Token generation ──────────────────────────────────────────────────────────

fn generate_token() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(64)
        .map(char::from)
        .collect()
}

// ── Login endpoint ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub token: String,
    /// Seconds until this token expires.
    pub expires_in: u64,
}

/// `POST /login`  –  body: `{"username":"…","password":"…"}`
///
/// Returns `{"token":"…","expires_in":<seconds>}` on success or 401 on bad
/// credentials.  Send the token as `Authorization: Bearer <token>` on every
/// subsequent request.
pub async fn login_handler(
    body: web::Json<LoginRequest>,
    auth: web::Data<AuthState>,
) -> impl Responder {
    let user = auth.users.iter().find(|u| u.username == body.username);

    let hash = match user {
        Some(u) => u.password_hash.clone(),
        // Verify against a dummy hash so response time is constant and
        // usernames cannot be enumerated by timing.
        None => "$2b$12$invalidhashpaddingtomakethisconstanttimeXXXXXXXXXXXXX".to_owned(),
    };

    match bcrypt::verify(&body.password, &hash) {
        Ok(true) if user.is_some() => {
            let token = auth.issue_token();
            let expires_in = auth.token_ttl.as_secs();
            HttpResponse::Ok().json(LoginResponse { token, expires_in })
        }
        _ => HttpResponse::Unauthorized()
            .content_type("text/plain")
            .body("Invalid username or password"),
    }
}

/// `POST /logout`
///
/// Immediately invalidates the bearer token supplied in the `Authorization`
/// header.  Requires a valid token (the middleware enforces this).
pub async fn logout_handler(
    req: HttpRequest,
    auth: web::Data<AuthState>,
) -> impl Responder {
    // The middleware already validated the token; extract it again to revoke it.
    if let Some(token) = req
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            let v = v.trim();
            if v.len() > 7 && v[..7].eq_ignore_ascii_case("Bearer ") {
                Some(v[7..].trim().to_owned())
            } else {
                None
            }
        })
    {
        auth.revoke_token(&token);
    }
    HttpResponse::Ok()
        .content_type("text/plain")
        .body("Logged out")
}
