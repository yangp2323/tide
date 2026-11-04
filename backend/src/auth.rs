use crate::{exchange::signature, App};
use anyhow::{ensure, Result};
use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{path::Path, sync::Arc};
use subtle::ConstantTimeEq;

pub const COOKIE: &str = "__Host-tide_session";
#[derive(Clone, Serialize, Deserialize)]
pub struct Auth {
    pub username: String,
    pub salt: String,
    pub password_hash: String,
}
impl Auth {
    pub fn load(path: &Path) -> Result<Self> {
        Ok(serde_json::from_slice(&std::fs::read(path)?)?)
    }
    pub fn create(username: String, password: &str) -> Result<Self> {
        ensure!(!password.is_empty(), "密码不能为空");
        let mut salt = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut salt);
        let mut hash = [0u8; 64];
        scrypt::scrypt(
            password.as_bytes(),
            &salt,
            &scrypt::Params::new(14, 8, 1, 64)?,
            &mut hash,
        )?;
        Ok(Self {
            username,
            salt: hex::encode(salt),
            password_hash: hex::encode(hash),
        })
    }
    pub fn verify(&self, username: &str, password: &str) -> Result<bool> {
        let salt = hex::decode(&self.salt)?;
        let expected = hex::decode(&self.password_hash)?;
        let mut hash = [0u8; 64];
        scrypt::scrypt(
            password.as_bytes(),
            &salt,
            &scrypt::Params::new(14, 8, 1, 64)?,
            &mut hash,
        )?;
        Ok(username == self.username && bool::from(hash.as_slice().ct_eq(&expected)))
    }
}
pub fn session(secret: &str) -> String {
    let mut nonce = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut nonce);
    let data = format!(
        "{}.{}",
        chrono::Utc::now().timestamp() + 43200,
        hex::encode(nonce)
    );
    format!("{}.{}", data, signature(secret, &data))
}
pub fn valid(secret: &str, token: &str) -> bool {
    let parts: Vec<_> = token.split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    let exp = parts[0].parse::<i64>().unwrap_or(0);
    let now = chrono::Utc::now().timestamp();
    if exp <= now || exp > now + 43200 || parts[1].len() != 48 || hex::decode(parts[1]).is_err() {
        return false;
    }
    let Ok(sig) = hex::decode(parts[2]) else {
        return false;
    };
    let expected = hex::decode(signature(secret, &format!("{}.{}", parts[0], parts[1]))).unwrap();
    bool::from(sig.ct_eq(&expected))
}
pub async fn protect(State(app): State<Arc<App>>, req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let api = path.starts_with("/api/");
    let is_login = path == "/api/auth/login";
    let method = req.method().as_str();
    if !matches!(method, "GET" | "HEAD" | "OPTIONS")
        && req.headers().get("origin").and_then(|h| h.to_str().ok()) != Some(app.origin.as_str())
    {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"detail":"请求来源无效"})),
        )
            .into_response();
    }
    let token = req
        .headers()
        .get(header::COOKIE)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| {
            s.split(';')
                .find_map(|p| p.trim().strip_prefix(&format!("{}=", COOKIE)))
        });
    let public = path == "/healthz"
        || path == "/login"
        || path == "/login/"
        || path.starts_with("/_next/")
        || path.starts_with("/fonts/")
        || path == "/favicon.ico"
        || is_login;
    if !public && !token.is_some_and(|t| valid(&app.secret, t)) {
        if api {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({"detail":"登录已过期，请重新登录"})),
            )
                .into_response();
        }
        return (StatusCode::SEE_OTHER, [(header::LOCATION, "/login/")]).into_response();
    }
    let mut response = next.run(req).await;
    let headers = response.headers_mut();
    headers.insert("x-frame-options", "DENY".parse().unwrap());
    headers.insert("x-content-type-options", "nosniff".parse().unwrap());
    headers.insert("referrer-policy", "same-origin".parse().unwrap());
    headers.insert(
        "cache-control",
        if api || !public {
            "no-store"
        } else {
            "no-cache"
        }
        .parse()
        .unwrap(),
    );
    response
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tampered_and_expired_sessions_fail() {
        let t = session("secret");
        assert!(valid("secret", &t));
        assert!(!valid("other", &t));
        assert!(!valid(
            "secret",
            &format!("0.{}", t.split_once('.').unwrap().1)
        ));
        assert!(!valid("secret", &(t + "x")));
    }
    #[test]
    fn legacy_scrypt_credentials_are_compatible() {
        let a = Auth::create("admin".into(), "test-password").unwrap();
        assert!(a.verify("admin", "test-password").unwrap());
        assert!(!a.verify("admin", "wrong").unwrap());
    }
}
