use std::{
    env,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

pub(super) fn matches_key(headers: &HeaderMap, expected: &str) -> bool {
    let bearer = headers
        .get("authorization")
        .and_then(|header| header.to_str().ok())
        .and_then(|header| header.strip_prefix("Bearer "));
    let key = headers
        .get("x-api-key")
        .and_then(|header| header.to_str().ok());
    [bearer, key].into_iter().flatten().any(|provided| {
        let provided = provided.as_bytes();
        let mut difference = provided.len() ^ expected.len();
        for (index, byte) in expected.bytes().enumerate() {
            difference |= usize::from(provided.get(index).copied().unwrap_or(0) ^ byte);
        }
        difference == 0
    })
}

#[derive(Clone)]
pub(super) struct ProxyAuth {
    key: Option<Arc<str>>,
    expiry: Result<Option<u64>, ()>,
}

impl ProxyAuth {
    pub(super) fn from_env() -> Option<Self> {
        let expiry = env::var("MUSE_PROXY_API_KEY_EXPIRES_AT").unwrap_or_default();
        if expiry.is_empty() && env::var("MUSE_DESK_REQUIRE_KEY").as_deref() != Ok("1") {
            return None;
        }
        Some(Self {
            key: env::var("MUSE_PROXY_API_KEY")
                .ok()
                .filter(|key| !key.trim().is_empty())
                .map(Into::into),
            expiry: if expiry.is_empty() {
                Ok(None)
            } else {
                expiry.parse().map(Some).map_err(|_| ())
            },
        })
    }

    fn check(
        &self,
        headers: &HeaderMap,
        now: u64,
    ) -> Result<(), (StatusCode, &'static str, &'static str)> {
        let (Some(key), Ok(expiry)) = (&self.key, self.expiry) else {
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                "proxy_key_not_configured",
                "Proxy API key configuration is unavailable",
            ));
        };
        if !matches_key(headers, key) {
            return Err((
                StatusCode::UNAUTHORIZED,
                "invalid_api_key",
                "Invalid proxy API key",
            ));
        }
        if expiry.is_some_and(|end| now >= end) {
            return Err((
                StatusCode::UNAUTHORIZED,
                "proxy_key_expired",
                "Proxy API key expired. Rotate it in Din Desk.",
            ));
        }
        Ok(())
    }
}

pub(super) async fn require_key(
    State(auth): State<ProxyAuth>,
    request: Request,
    next: Next,
) -> Response {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    match auth.check(request.headers(), now) {
        Ok(()) => next.run(request).await,
        Err((status, code, message)) => {
            let kind = if status == StatusCode::UNAUTHORIZED {
                "authentication_error"
            } else {
                "api_error"
            };
            (
                status,
                Json(json!({"type":"error","error":{"type":kind,"code":code,"message":message}})),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_expiry_and_rotation_take_effect_at_the_boundary() {
        let auth = ProxyAuth {
            key: Some("initial".into()),
            expiry: Ok(Some(100)),
        };
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer initial".parse().unwrap());
        assert!(auth.check(&headers, 99).is_ok());
        assert_eq!(
            auth.check(&headers, 100).unwrap_err().1,
            "proxy_key_expired"
        );
        let renewed = ProxyAuth {
            key: Some("renewed".into()),
            expiry: Ok(None),
        };
        assert_eq!(
            renewed.check(&headers, 101).unwrap_err().1,
            "invalid_api_key"
        );
        headers.remove("authorization");
        headers.insert("x-api-key", "renewed".parse().unwrap());
        assert!(renewed.check(&headers, u64::MAX).is_ok());
        let malformed = ProxyAuth {
            expiry: Err(()),
            ..renewed
        };
        assert_eq!(
            malformed.check(&headers, 101).unwrap_err().0,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
}
