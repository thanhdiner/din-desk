//! Direct Gemini Web adapter: account-bound HTTP, independent remote sessions.
mod models;
mod protocol;
pub use models::{Catalog, Options};
#[cfg(test)]
mod tests;

use crate::{
    core::{image::InputImage, turn::TurnEvent},
    infrastructure::outbound_proxy::AccountProxy,
    ports::{GenerationFuture, SessionFuture, TextGenerator},
};
use base64::{engine::general_purpose::STANDARD, Engine};
use reqwest::{
    header::{HeaderMap, HeaderValue, COOKIE, SET_COOKIE},
    Client, Url,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, Mutex, Semaphore};

const ORIGIN: &str = "https://gemini.google.com";
const UPLOAD: &str = "https://content-push.googleapis.com/upload/";
const UA:&str="Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

struct Bootstrap {
    at: String,
    bl: String,
    sid: Option<String>,
    push_id: String,
    checked: Instant,
}
struct Shared {
    http: Client,
    cookie: Mutex<BTreeMap<String, String>>,
    bootstrap: Mutex<Option<Bootstrap>>,
    slots: Semaphore,
    pending: Semaphore,
    origin: Url,
    upload: Url,
    options: Options,
    catalog: Mutex<Option<(Catalog, Instant)>>,
    instance_id: String,
}
pub struct GeminiGenerator(Arc<Shared>);
struct Conversation {
    shared: Arc<Shared>,
    state: Mutex<Value>,
    images: Mutex<BTreeMap<String, Value>>,
}

pub fn validate_cookie(cookie: &str) -> Result<(), String> {
    if cookie.len() > 65536
        || cookie.chars().any(char::is_control)
        || !cookie.split(';').any(|part| {
            part.trim()
                .strip_prefix("__Secure-1PSID=")
                .is_some_and(|s| !s.is_empty())
        })
    {
        return Err("Gemini cookie must contain __Secure-1PSID on one line; include __Secure-1PSIDTS when available".into());
    }
    HeaderValue::from_str(cookie).map_err(|_| "Invalid Gemini cookie header")?;
    Ok(())
}
impl GeminiGenerator {
    pub fn new(cookie: &str, proxy: &AccountProxy) -> Result<Self, String> {
        Self::with_options(cookie, proxy, Options::default())
    }
    pub fn with_options(
        cookie: &str,
        proxy: &AccountProxy,
        options: Options,
    ) -> Result<Self, String> {
        options.validate()?;
        validate_cookie(cookie)?;
        let cookie = cookie
            .split(';')
            .filter_map(|part| part.trim().split_once('='))
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        let http = proxy
            .http_client_with_timeout(Duration::from_secs(150))
            .map_err(|_| "Could not configure Gemini HTTP client")?;
        Ok(Self(Arc::new(Shared {
            http,
            cookie: Mutex::new(cookie),
            bootstrap: Mutex::new(None),
            slots: Semaphore::new(4),
            pending: Semaphore::new(64),
            origin: Url::parse(ORIGIN).unwrap(),
            upload: Url::parse(UPLOAD).unwrap(),
            options,
            catalog: Mutex::new(None),
            instance_id: uuid::Uuid::new_v4().to_string().to_uppercase(),
        })))
    }
    pub async fn check(&self) -> Result<(), String> {
        self.0.bootstrap(true).await?;
        if !self.0.options.model_id.is_empty() {
            self.catalog(true).await?.select(&self.0.options)?;
        }
        Ok(())
    }
    pub async fn catalog(&self, force: bool) -> Result<Catalog, String> {
        self.0.catalog(force).await
    }
    pub fn options(&self) -> &Options {
        &self.0.options
    }
}

async fn bounded(mut response: reqwest::Response, maximum: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Gemini response connection ended unexpectedly")?
    {
        if bytes.len() + chunk.len() > maximum {
            return Err("Gemini response exceeds allowed size".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
fn status(response: &reqwest::Response) -> Result<(), String> {
    match response.status().as_u16() {
        200..=299 => Ok(()),
        401 | 403 => {
            Err("Gemini session expired or access denied; update the Gemini cookie".into())
        }
        413 => Err(crate::ports::UPSTREAM_INPUT_TOO_LARGE.into()),
        429 => Err(
            "Gemini account quota/rate limit reached; try later or select another account manually"
                .into(),
        ),
        _ => Err(
            "Gemini request failed; no automatic retry or provider fallback was performed".into(),
        ),
    }
}
impl Shared {
    fn account_url(&self, path: &str) -> Url {
        self.origin
            .join(&if self.options.account_index == 0 {
                path.to_owned()
            } else {
                format!("u/{}/{path}", self.options.account_index)
            })
            .unwrap()
    }
    fn app_url(&self) -> Url {
        self.origin
            .join(&if self.options.account_index == 0 {
                "app".into()
            } else {
                format!("u/{}/app", self.options.account_index)
            })
            .unwrap()
    }
    async fn catalog(&self, force: bool) -> Result<Catalog, String> {
        let mut cache = self.catalog.lock().await;
        if !force {
            if let Some((catalog, checked)) = cache.as_ref() {
                if checked.elapsed() < Duration::from_secs(900) {
                    return Ok(catalog.clone());
                }
            }
        }
        let (at, bl, sid, _) = self.bootstrap(false).await?;
        let mut url = self.account_url("_/BardChatUi/data/batchexecute");
        url.query_pairs_mut()
            .append_pair("rpcids", "otAQ7b")
            .append_pair("source-path", "/app")
            .append_pair("hl", "vi")
            .append_pair("rt", "c")
            .append_pair("bl", &bl)
            .append_pair("authuser", &self.options.account_index.to_string());
        if let Some(sid) = sid {
            url.query_pairs_mut().append_pair("f.sid", &sid);
        }
        let body = json!([[["otAQ7b", "[1]", null, "generic"]]]).to_string();
        let fetch = async {
            let response = self
                .http
                .post(url)
                .header(COOKIE, self.cookie_header().await?)
                .header("User-Agent", UA)
                .header("Origin", self.origin.as_str().trim_end_matches('/'))
                .header("Referer", self.app_url().as_str())
                .form(&[("at", at), ("f.req", body)])
                .send()
                .await
                .map_err(|_| "Gemini model catalog network/proxy failed")?;
            status(&response)?;
            self.update_cookies(response.headers()).await;
            let bytes = bounded(response, 1024 * 1024).await?;
            models::parse_rpc(&bytes, self.options.account_index)
        };
        let catalog = tokio::time::timeout(Duration::from_secs(25), fetch)
            .await
            .map_err(|_| "Gemini model catalog timed out")??;
        *cache = Some((catalog.clone(), Instant::now()));
        Ok(catalog)
    }
    async fn cookie_header(&self) -> Result<HeaderValue, String> {
        let value = self
            .cookie
            .lock()
            .await
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ");
        let mut value = HeaderValue::from_str(&value).map_err(|_| "Invalid Gemini cookie")?;
        value.set_sensitive(true);
        Ok(value)
    }
    async fn update_cookies(&self, headers: &HeaderMap) {
        let mut cookies = self.cookie.lock().await;
        for header in headers.get_all(SET_COOKIE) {
            if let Some((name, value)) = header
                .to_str()
                .ok()
                .and_then(|h| h.split(';').next())
                .and_then(|h| h.split_once('='))
            {
                if let Some(old) = cookies.get_mut(name) {
                    *old = value.to_owned();
                }
            }
        }
    }
    async fn bootstrap(
        &self,
        force: bool,
    ) -> Result<(String, String, Option<String>, String), String> {
        let mut cache = self.bootstrap.lock().await;
        if force
            || cache
                .as_ref()
                .is_none_or(|b| b.checked.elapsed() > Duration::from_secs(900))
        {
            let response = tokio::time::timeout(
                Duration::from_secs(20),
                self.http
                    .get(self.app_url())
                    .header("X-Goog-AuthUser", self.options.account_index.to_string())
                    .header(COOKIE, self.cookie_header().await?)
                    .header("User-Agent", UA)
                    .send(),
            )
            .await
            .map_err(|_| "Gemini authentication timed out")?
            .map_err(|_| "Gemini authentication network/proxy failed")?;
            status(&response)?;
            self.update_cookies(response.headers()).await;
            let body =
                tokio::time::timeout(Duration::from_secs(20), bounded(response, 8 * 1024 * 1024))
                    .await
                    .map_err(|_| "Gemini authentication read timed out")??;
            let html =
                std::str::from_utf8(&body).map_err(|_| "Invalid Gemini authentication response")?;
            let at=protocol::bootstrap_field(html,"SNlM0e").ok_or("Gemini cookie is not signed in; update __Secure-1PSID/__Secure-1PSIDTS (no anonymous fallback)")?;
            let bl = protocol::bootstrap_field(html, "cfb2h")
                .filter(|s| s.starts_with("boq_"))
                .ok_or("Gemini web protocol changed: build label not found")?;
            *cache = Some(Bootstrap {
                at,
                bl,
                sid: protocol::bootstrap_field(html, "FdrFJe"),
                push_id: protocol::bootstrap_field(html, "qKIAYe")
                    .unwrap_or_else(|| "feeds/mcudyrk2a4khkz".into()),
                checked: Instant::now(),
            });
        }
        let b = cache.as_ref().unwrap();
        Ok((b.at.clone(), b.bl.clone(), b.sid.clone(), b.push_id.clone()))
    }
}

impl TextGenerator for GeminiGenerator {
    fn generate(&self, prompt: String) -> GenerationFuture<'_> {
        self.generate_with_images(prompt, Vec::new())
    }
    fn generate_with_images(
        &self,
        prompt: String,
        images: Vec<InputImage>,
    ) -> GenerationFuture<'_> {
        Box::pin(async move {
            let bound = self.open_session().await?.unwrap();
            bound.generate_with_images(prompt, images).await
        })
    }
    fn open_session(&self) -> SessionFuture<'_> {
        Box::pin(async move {
            Ok(Some(Arc::new(Conversation {
                shared: self.0.clone(),
                state: Mutex::new(protocol::fresh_metadata()),
                images: Mutex::new(BTreeMap::new()),
            }) as Arc<dyn TextGenerator>))
        })
    }
}
impl Conversation {
    async fn run(&self, prompt: String, images: Vec<InputImage>) -> Result<String, String> {
        let _pending = self
            .shared
            .pending
            .try_acquire()
            .map_err(|_| "Gemini request queue is full")?;
        let _slot = tokio::time::timeout(Duration::from_secs(15), self.shared.slots.acquire())
            .await
            .map_err(|_| "Gemini request queue wait exceeded 15 seconds")?
            .map_err(|_| "Gemini is stopping")?;
        let mut metadata = self.state.lock().await;
        let (at, bl, sid, push_id) = self.shared.bootstrap(false).await?;
        let selection = if self.shared.options.model_id.is_empty() {
            None
        } else {
            Some(
                self.shared
                    .catalog(false)
                    .await?
                    .select(&self.shared.options)?,
            )
        };
        let mut refs = Vec::new();
        let mut uploaded = self.images.lock().await;
        for image in images {
            if let Some(reference) = uploaded.get(&image.id) {
                refs.push(reference.clone());
                continue;
            }
            let bytes = STANDARD
                .decode(image.data_base64.as_ref())
                .map_err(|_| "Invalid Gemini image encoding")?;
            let extension = match image.media_type.as_str() {
                "image/jpeg" => "jpg",
                "image/gif" => "gif",
                "image/webp" => "webp",
                _ => "png",
            };
            let filename = format!("{}.{}", image.id, extension);
            let file = reqwest::multipart::Part::bytes(bytes)
                .file_name(filename.clone())
                .mime_str(&image.media_type)
                .map_err(|_| "Invalid Gemini image MIME type")?;
            // Upload host never receives account cookies or local API keys.
            let response = self
                .shared
                .http
                .post(self.shared.upload.clone())
                .header("User-Agent", UA)
                .header("Push-ID", &push_id)
                .multipart(reqwest::multipart::Form::new().part("file", file))
                .send()
                .await
                .map_err(|_| "Gemini image upload failed; no generation submitted")?;
            status(&response)?;
            let data = bounded(response, 65536).await?;
            let reference = std::str::from_utf8(&data)
                .map_err(|_| "Invalid Gemini image upload response")?
                .trim();
            if reference.is_empty() || reference.len() > 65535 {
                return Err(
                    "Gemini did not acknowledge image upload; no generation submitted".into(),
                );
            }
            let reference = json!([[reference], filename]);
            uploaded.insert(image.id, reference.clone());
            refs.push(reference);
        }
        drop(uploaded);
        let request_id = uuid::Uuid::new_v4().to_string().to_uppercase();
        let mut body = protocol::generate_body(&prompt, &metadata, &refs, &request_id);
        if let Some(selection) = &selection {
            selection.apply_body(&mut body)?;
        }
        let body = body.to_string();
        let mut url = self
            .shared
            .account_url("_/BardChatUi/data/assistant.lamda.BardFrontendService/StreamGenerate");
        url.query_pairs_mut()
            .append_pair("bl", &bl)
            .append_pair("hl", "en")
            .append_pair(
                "_reqid",
                &format!("{}", uuid::Uuid::new_v4().as_u128() % 900000 + 100000),
            )
            .append_pair("rt", "c");
        url.query_pairs_mut()
            .append_pair("authuser", &self.shared.options.account_index.to_string());
        if let Some(sid) = sid {
            url.query_pairs_mut().append_pair("f.sid", &sid);
        }
        let mut request = self
            .shared
            .http
            .post(url)
            .header(
                "X-Goog-AuthUser",
                self.shared.options.account_index.to_string(),
            )
            .header(COOKIE, self.shared.cookie_header().await?)
            .header("Origin", self.shared.origin.as_str().trim_end_matches('/'))
            .header("Referer", self.shared.app_url().as_str())
            .header("User-Agent", UA)
            .header("X-Same-Domain", "1")
            .header(
                "x-goog-ext-525005358-jspb",
                json!([request_id, 1]).to_string(),
            );
        if let Some(selection) = selection {
            request = request
                .header(
                    "x-goog-ext-525001261-jspb",
                    selection.header(&self.shared.instance_id, &bl),
                )
                .header("x-goog-ext-73010989-jspb", "[0]")
                .header("x-goog-ext-73010990-jspb", "[0,0,0]");
        }
        if let Some(sapisid) = self.shared.cookie.lock().await.get("SAPISID").cloned() {
            use sha1::{Digest, Sha1};
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let digest = Sha1::digest(
                format!(
                    "{timestamp} {sapisid} {}",
                    self.shared.origin.as_str().trim_end_matches('/')
                )
                .as_bytes(),
            );
            let mut auth = HeaderValue::from_str(&format!("SAPISIDHASH {timestamp}_{digest:x}"))
                .map_err(|_| "Gemini authorization header failed")?;
            auth.set_sensitive(true);
            request = request.header("Authorization", auth);
        }
        let response = request
            .form(&[("f.req", body), ("at", at)])
            .send()
            .await
            .map_err(|_| {
                "Gemini generation connection failed; request may have been accepted, not retried"
            })?;
        status(&response)?;
        self.shared.update_cookies(response.headers()).await;
        let mut response = response;
        let mut frames = protocol::Frames::default();
        let mut reply = protocol::Reply::default();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "Gemini stream disconnected; no tools dispatched")?
        {
            for frame in frames.push(&chunk)? {
                reply.accept(frame)?;
            }
        }
        frames.finish()?;
        let (text, next_metadata) = reply.finish()?;
        *metadata = next_metadata;
        Ok(text)
    }
}
impl TextGenerator for Conversation {
    fn supports_context_upload(&self) -> bool {
        true
    }
    fn generate(&self, prompt: String) -> GenerationFuture<'_> {
        self.generate_with_images(prompt, Vec::new())
    }
    fn generate_with_images(
        &self,
        prompt: String,
        images: Vec<InputImage>,
    ) -> GenerationFuture<'_> {
        Box::pin(async move {
            let (sender, receiver) = mpsc::channel(2);
            let result = self.run(prompt, images).await;
            match result {
                Ok(text) => {
                    let _ = sender.send(Ok(TurnEvent::Text(text))).await;
                    let _ = sender.send(Ok(TurnEvent::Completed)).await;
                }
                Err(error) => {
                    let _ = sender.send(Err(error)).await;
                }
            }
            Ok(receiver)
        })
    }
}
