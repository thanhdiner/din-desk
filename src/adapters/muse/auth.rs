//! Bootstrap a Muse VM connection from a user-provided browser cookie header.
use std::time::Duration;

use reqwest::{
    header::{
        HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, COOKIE, ORIGIN, REFERER,
        USER_AGENT,
    },
    Client, StatusCode, Url,
};
use serde::{Deserialize, Serialize};

const MUSE_ORIGIN: &str = "https://muse.ai";
const GATEWAY_ORIGIN: &str = "wss://hatch.metaaivm.com/v1/noise";

/// Secrets deliberately do not implement Debug or Display.
pub struct MuseCredentials {
    cookie: HeaderValue,
}

impl MuseCredentials {
    pub fn from_cookie_header(value: &str) -> Result<Self, reqwest::header::InvalidHeaderValue> {
        let mut cookie = HeaderValue::from_str(value)?;
        cookie.set_sensitive(true);
        Ok(Self { cookie })
    }
}

pub struct VmSession {
    pub vm_id: String,
    token: String,
}

impl VmSession {
    /// Contains a secret token; never write this URL to logs.
    pub fn gateway_url(&self) -> Url {
        let mut url = Url::parse(GATEWAY_ORIGIN).expect("constant gateway URL is valid");
        url.query_pairs_mut()
            .append_pair("vm_id", &self.vm_id)
            .append_pair("auth_token", &self.token);
        url
    }
}

#[derive(Debug)]
pub enum AuthError {
    Request(reqwest::Error),
    InvalidHeader,
    UpstreamStatus {
        stage: &'static str,
        status: StatusCode,
    },
    VmUnavailable,
    MissingToken,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Request(_) => f.write_str("Muse authentication request failed"),
            Self::InvalidHeader => f.write_str("Muse authentication returned an invalid token"),
            Self::UpstreamStatus { stage, status } => {
                write!(f, "Muse {stage} returned HTTP {status}")
            }
            Self::VmUnavailable => f.write_str("Muse VM is unavailable or missing"),
            Self::MissingToken => f.write_str("Muse authentication returned an empty token"),
        }
    }
}
impl std::error::Error for AuthError {}
impl From<reqwest::Error> for AuthError {
    fn from(e: reqwest::Error) -> Self {
        Self::Request(e.without_url())
    }
}

#[derive(Deserialize)]
struct AccessResponse {
    access_token: String,
}
#[derive(Deserialize)]
struct SessionResponse {
    vm_id: Option<String>,
    vm_state: Option<String>,
}
#[derive(Deserialize)]
struct HatchResponse {
    token: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HatchRequest<'a> {
    vm_address: String,
    vm_name: &'a str,
}

pub struct MuseAuth {
    client: Client,
    base_url: Url,
}

impl MuseAuth {
    pub fn new() -> Result<Self, AuthError> {
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(20))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            base_url: Url::parse(MUSE_ORIGIN).expect("constant Muse URL is valid"),
        })
    }

    pub fn with_proxy(
        proxy: &super::proxy::AccountProxy,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self {
            client: proxy.http_client()?,
            base_url: Url::parse(MUSE_ORIGIN).expect("constant"),
        })
    }

    fn headers(credentials: &MuseCredentials) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(COOKIE, credentials.cookie.clone());
        headers.insert(ORIGIN, HeaderValue::from_static(MUSE_ORIGIN));
        headers.insert(REFERER, HeaderValue::from_static("https://muse.ai/"));
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(USER_AGENT, HeaderValue::from_static("Mozilla/5.0"));
        headers.insert("sec-fetch-site", HeaderValue::from_static("same-origin"));
        headers.insert("sec-fetch-mode", HeaderValue::from_static("cors"));
        headers.insert("sec-fetch-dest", HeaderValue::from_static("empty"));
        headers
    }

    /// Performs a fresh bootstrap. The caller manages renewal and the socket lifetime.
    pub async fn bootstrap(&self, credentials: &MuseCredentials) -> Result<VmSession, AuthError> {
        let headers = Self::headers(credentials);
        let response = self
            .client
            .post(self.endpoint("/api/auth/check"))
            .headers(headers.clone())
            .body("")
            .send()
            .await?;
        let access: AccessResponse = checked(response, "auth/check").await?.json().await?;
        if access.access_token.is_empty() {
            return Err(AuthError::MissingToken);
        }
        let response = self
            .client
            .get(self.endpoint("/api/session"))
            .headers(headers.clone())
            .send()
            .await?;
        let session: SessionResponse = checked(response, "session").await?.json().await?;
        if session.vm_state.as_deref() == Some("DISABLED") {
            return Err(AuthError::VmUnavailable);
        }
        let vm_id = session
            .vm_id
            .filter(|id| {
                !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
            .ok_or(AuthError::VmUnavailable)?;
        // Muse Web wakes an assigned VM before dialing the gateway, even when
        // its cached inventory says RUNNING. A VM can sleep while cookies and
        // tokens remain valid; retrying the WebSocket alone cannot wake it.
        // This idempotent control-plane request carries no conversation or prompt.
        let response = self
            .client
            .post(self.endpoint("/api/hatch/vm/wake"))
            .headers(headers.clone())
            .json(&serde_json::json!({"vm_id": vm_id, "retry_count": 0}))
            .timeout(Duration::from_secs(10))
            .send()
            .await?;
        let mut response = checked(response, "vm/wake").await?;
        let mut wake_bytes = 0usize;
        while let Some(chunk) = response.chunk().await? {
            wake_bytes += chunk.len();
            if wake_bytes > 64 * 1024 {
                return Err(AuthError::VmUnavailable);
            }
        }
        let mut auth = HeaderValue::from_str(&format!("Bearer {}", access.access_token))
            .map_err(|_| AuthError::InvalidHeader)?;
        auth.set_sensitive(true);
        let body = HatchRequest {
            vm_address: format!("wss://{vm_id}.metaaivm.com/"),
            vm_name: &vm_id,
        };
        let response = self
            .client
            .post(self.endpoint("/api/hatch/token"))
            .headers(headers)
            .header(AUTHORIZATION, auth)
            .json(&body)
            .send()
            .await?;
        let token: HatchResponse = checked(response, "hatch/token").await?.json().await?;
        if token.token.is_empty() {
            return Err(AuthError::MissingToken);
        }
        Ok(VmSession {
            vm_id,
            token: token.token,
        })
    }

    fn endpoint(&self, path: &str) -> Url {
        self.base_url.join(path).expect("constant path is valid")
    }
}

async fn checked(
    response: reqwest::Response,
    stage: &'static str,
) -> Result<reqwest::Response, AuthError> {
    if !response.status().is_success() {
        return Err(AuthError::UpstreamStatus {
            stage,
            status: response.status(),
        });
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::{Json, State},
        http::HeaderMap,
        routing::{get, post},
        Router,
    };
    use serde_json::{json, Value};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[tokio::test]
    async fn bootstrap_uses_cookie_and_access_token_for_hatch() {
        async fn auth(State(order): State<Arc<AtomicUsize>>, headers: HeaderMap) -> Json<Value> {
            assert_eq!(order.fetch_add(1, Ordering::SeqCst), 0);
            assert_eq!(headers.get(COOKIE).unwrap(), "hatch_sess=test-cookie");
            Json(json!({"access_token": "test-access"}))
        }
        async fn session(State(order): State<Arc<AtomicUsize>>) -> Json<Value> {
            assert_eq!(order.fetch_add(1, Ordering::SeqCst), 1);
            // Inventory can still say RUNNING after the gateway goes to sleep.
            Json(json!({"vm_id": "vm-123", "vm_state":"RUNNING"}))
        }
        async fn wake(
            State(order): State<Arc<AtomicUsize>>,
            headers: HeaderMap,
            Json(body): Json<Value>,
        ) -> &'static str {
            assert_eq!(order.fetch_add(1, Ordering::SeqCst), 2);
            assert_eq!(headers.get(COOKIE).unwrap(), "hatch_sess=test-cookie");
            assert!(!headers.contains_key(AUTHORIZATION));
            assert_eq!(body, json!({"vm_id":"vm-123","retry_count":0}));
            "awake"
        }
        async fn hatch(
            State(order): State<Arc<AtomicUsize>>,
            headers: HeaderMap,
            Json(body): Json<Value>,
        ) -> Json<Value> {
            assert_eq!(order.fetch_add(1, Ordering::SeqCst), 3);
            assert_eq!(headers.get(AUTHORIZATION).unwrap(), "Bearer test-access");
            assert_eq!(body["vmName"], "vm-123");
            assert_eq!(body["vmAddress"], "wss://vm-123.metaaivm.com/");
            Json(json!({"token": "token with &"}))
        }
        let order = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route("/api/auth/check", post(auth))
            .route("/api/session", get(session))
            .route("/api/hatch/vm/wake", post(wake))
            .route("/api/hatch/token", post(hatch))
            .with_state(order.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut auth = MuseAuth::new().unwrap();
        auth.base_url = Url::parse(&format!("http://{address}")).unwrap();
        let session = auth
            .bootstrap(&MuseCredentials::from_cookie_header("hatch_sess=test-cookie").unwrap())
            .await
            .unwrap();
        let url = session.gateway_url();
        let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(params["vm_id"], "vm-123");
        assert_eq!(params["auth_token"], "token with &");
        assert_eq!(order.load(Ordering::SeqCst), 4);
        server.abort();
    }

    #[tokio::test]
    async fn rejected_wake_stops_bootstrap_before_token_or_chat() {
        for status in [401, 403, 429, 503] {
            let token_calls = Arc::new(AtomicUsize::new(0));
            let calls = token_calls.clone();
            let app = Router::new()
                .route(
                    "/api/auth/check",
                    post(|| async { Json(json!({"access_token":"fixture-private"})) }),
                )
                .route(
                    "/api/session",
                    get(|| async { Json(json!({"vm_id":"vm-123"})) }),
                )
                .route(
                    "/api/hatch/vm/wake",
                    post(move || async move { StatusCode::from_u16(status).unwrap() }),
                )
                .route(
                    "/api/hatch/token",
                    post(move || {
                        let calls = calls.clone();
                        async move {
                            calls.fetch_add(1, Ordering::SeqCst);
                            Json(json!({"token":"must-not-mint"}))
                        }
                    }),
                );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let mut auth = MuseAuth::new().unwrap();
            auth.base_url = Url::parse(&format!("http://{address}")).unwrap();
            let error = auth
                .bootstrap(
                    &MuseCredentials::from_cookie_header("hatch_sess=fixture-private").unwrap(),
                )
                .await
                .err()
                .expect("wake must fail");
            assert!(
                matches!(error,AuthError::UpstreamStatus{stage:"vm/wake",status: actual} if actual.as_u16()==status)
            );
            assert!(!error.to_string().contains("fixture-private"));
            assert_eq!(token_calls.load(Ordering::SeqCst), 0);
            server.abort();
        }
    }

    #[tokio::test]
    async fn disabled_or_invalid_vm_is_not_woken() {
        for session in [
            json!({"vm_id":"vm-123","vm_state":"DISABLED"}),
            json!({"vm_id":"../invalid"}),
            json!({"vm_id":null}),
        ] {
            let wake_calls = Arc::new(AtomicUsize::new(0));
            let calls = wake_calls.clone();
            let app = Router::new()
                .route(
                    "/api/auth/check",
                    post(|| async { Json(json!({"access_token":"fixture-private"})) }),
                )
                .route(
                    "/api/session",
                    get(move || {
                        let session = session.clone();
                        async move { Json(session) }
                    }),
                )
                .route(
                    "/api/hatch/vm/wake",
                    post(move || {
                        let calls = calls.clone();
                        async move {
                            calls.fetch_add(1, Ordering::SeqCst);
                            StatusCode::NO_CONTENT
                        }
                    }),
                );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let mut auth = MuseAuth::new().unwrap();
            auth.base_url = Url::parse(&format!("http://{address}")).unwrap();
            assert!(matches!(
                auth.bootstrap(
                    &MuseCredentials::from_cookie_header("hatch_sess=fixture-private").unwrap()
                )
                .await,
                Err(AuthError::VmUnavailable)
            ));
            assert_eq!(wake_calls.load(Ordering::SeqCst), 0);
            server.abort();
        }
    }
}
