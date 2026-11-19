//! One explicit outbound route for a provider account. Never fall back to direct.
use base64::{engine::general_purpose::STANDARD, Engine};
use reqwest::Url;
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
};
use tokio_rustls::{
    rustls::{pki_types::ServerName, ClientConfig, RootCertStore},
    TlsConnector,
};

pub trait IoStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> IoStream for T {}
pub type Stream = Box<dyn IoStream>;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Contains credentials: deliberately no Debug/Display implementation.
#[derive(Clone, Default)]
pub struct AccountProxy(Option<Url>);
impl AccountProxy {
    pub fn from_env() -> Result<Self> {
        match std::env::var("MUSE_UPSTREAM_PROXY") {
            Ok(value) => Self::parse(&value),
            Err(std::env::VarError::NotPresent) => Ok(Self::default()),
            Err(_) => Err("MUSE_UPSTREAM_PROXY must be valid Unicode".into()),
        }
    }
    pub fn parse(value: &str) -> Result<Self> {
        let value = value.trim();
        if value.is_empty() {
            return Ok(Self::default());
        }
        let invalid =
            "Proxy must be http(s)://[user:password@]host:port, without a path, query or fragment";
        if value.len() > 4096 || value.chars().any(char::is_control) {
            return Err(invalid.into());
        }
        let url = Url::parse(value).map_err(|_| invalid)?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || url.port_or_known_default().is_none()
            || url.port_or_known_default() == Some(0)
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
            || (url.username().is_empty() && url.password().is_some())
        {
            return Err(invalid.into());
        }
        Ok(Self(Some(url)))
    }
    pub fn http_client(&self) -> Result<reqwest::Client> {
        self.http_client_with_timeout(Duration::from_secs(20))
    }
    pub fn http_client_with_timeout(&self, timeout: Duration) -> Result<reqwest::Client> {
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none());
        if let Some(url) = &self.0 {
            builder = builder
                .proxy(reqwest::Proxy::all(url.as_str()).map_err(|_| "Invalid account proxy")?);
        }
        builder
            .build()
            .map_err(|_| "Could not create account HTTP client".into())
    }
    pub async fn connect(&self, host: &str, port: u16) -> Result<Stream> {
        tokio::time::timeout(Duration::from_secs(20), self.connect_inner(host, port))
            .await
            .map_err(|_| "Account proxy connection timed out")?
    }
    async fn connect_inner(&self, host: &str, port: u16) -> Result<Stream> {
        let Some(proxy) = &self.0 else {
            return Ok(Box::new(
                TcpStream::connect((host.trim_matches(['[', ']']), port))
                    .await
                    .map_err(|_| "Muse direct connection failed")?,
            ));
        };
        let proxy_host = proxy
            .host_str()
            .ok_or("Invalid account proxy host")?
            .trim_matches(['[', ']']);
        let tcp = TcpStream::connect((proxy_host, proxy.port_or_known_default().unwrap()))
            .await
            .map_err(|_| "Cannot connect to account proxy")?;
        let mut stream: Stream = if proxy.scheme() == "https" {
            let roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let config = ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth();
            let name = ServerName::try_from(proxy_host.to_owned())
                .map_err(|_| "Invalid HTTPS proxy host")?;
            Box::new(
                TlsConnector::from(Arc::new(config))
                    .connect(name, tcp)
                    .await
                    .map_err(|_| "HTTPS proxy certificate or TLS connection failed")?,
            )
        } else {
            Box::new(tcp)
        };
        let authority = if host.contains(':') {
            format!("[{}]:{port}", host.trim_matches(['[', ']']))
        } else {
            format!("{host}:{port}")
        };
        let mut request = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
        if !proxy.username().is_empty() {
            let decode = |s: &str| {
                percent_encoding::percent_decode_str(s)
                    .decode_utf8()
                    .map(|s| s.into_owned())
            };
            let username =
                decode(proxy.username()).map_err(|_| "Invalid proxy username encoding")?;
            let password = decode(proxy.password().unwrap_or(""))
                .map_err(|_| "Invalid proxy password encoding")?;
            request.push_str(&format!(
                "Proxy-Authorization: Basic {}\r\n",
                STANDARD.encode(format!("{username}:{password}"))
            ));
        }
        request.push_str("\r\n");
        stream
            .write_all(request.as_bytes())
            .await
            .map_err(|_| "Proxy CONNECT write failed")?;
        // Read only the headers: do not consume any bytes belonging to inner TLS.
        let mut headers = Vec::new();
        loop {
            if headers.len() >= 16384 {
                return Err("Proxy CONNECT response headers too large".into());
            }
            headers.push(
                stream
                    .read_u8()
                    .await
                    .map_err(|_| "Proxy CONNECT response ended early")?,
            );
            if headers.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let status = std::str::from_utf8(&headers)
            .ok()
            .and_then(|s| s.lines().next())
            .and_then(|s| {
                let mut p = s.split_whitespace();
                let version = p.next()?;
                if !matches!(version, "HTTP/1.0" | "HTTP/1.1") {
                    return None;
                }
                p.next()?.parse::<u16>().ok()
            });
        match status {
            Some(200..=299) => Ok(stream),
            Some(407) => Err("Proxy authentication failed (407); check username/password".into()),
            _ => Err("Proxy refused CONNECT; no direct connection was attempted".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn invalid_proxy_errors_never_include_credentials() {
        for value in [
            "socks5://private-user:private-pass@host:1080",
            "http://user:private-pass@host/path",
            "http://host?token=private-pass",
            "http://host/#x",
            "http://:private-pass@host",
            "http://host:0",
        ] {
            let error = AccountProxy::parse(value)
                .err()
                .expect("reject")
                .to_string();
            assert!(!error.contains("private-pass"));
        }
        assert!(AccountProxy::parse("").is_ok());
        assert!(AccountProxy::parse("https://u:p%40ss@localhost:8080").is_ok());
        assert!(AccountProxy::parse("http://[::1]:8080").is_ok());
    }

    async fn headers(stream: &mut TcpStream) -> String {
        let mut bytes = Vec::new();
        while !bytes.ends_with(b"\r\n\r\n") {
            assert!(bytes.len() < 16384);
            bytes.push(stream.read_u8().await.unwrap());
        }
        String::from_utf8(bytes).unwrap()
    }

    #[tokio::test]
    async fn http_client_and_websocket_route_both_use_account_proxy() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for attempt in 0..2 {
                let (mut peer, _) = listener.accept().await.unwrap();
                let request = headers(&mut peer).await;
                assert!(
                    request.contains("Proxy-Authorization: Basic dXNlcjpwQHNz")
                        || request.contains("proxy-authorization: Basic dXNlcjpwQHNz")
                );
                if attempt == 0 {
                    assert!(request.starts_with("CONNECT account-host.invalid:443 HTTP/1.1"));
                } else {
                    assert!(request.starts_with("CONNECT http-auth.invalid:443 HTTP/1.1"));
                }
                assert!(!request.contains("Cookie:"));
                peer.write_all(
                    b"HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\n\r\n",
                )
                .await
                .unwrap();
            }
        });
        let proxy = AccountProxy::parse(&format!("http://user:p%40ss@{address}")).unwrap();
        let error = proxy
            .connect("account-host.invalid", 443)
            .await
            .err()
            .expect("407")
            .to_string();
        assert!(error.contains("407"));
        assert!(!error.contains("p%40ss"));
        assert!(proxy
            .http_client()
            .unwrap()
            .get("https://http-auth.invalid/")
            .send()
            .await
            .is_err());
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn rejects_failed_and_oversized_connect_without_direct_fallback() {
        for reply in [
            b"HTTP/1.1 502 Bad Gateway\r\n\r\n".to_vec(),
            vec![b'x'; 16385],
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let peer = tokio::spawn(async move {
                let (mut peer, _) = listener.accept().await.unwrap();
                headers(&mut peer).await;
                peer.write_all(&reply).await.unwrap();
            });
            let proxy = AccountProxy::parse(&format!("http://{address}")).unwrap();
            assert!(proxy.connect("origin.invalid", 443).await.is_err());
            peer.await.unwrap();
        }
    }

    #[tokio::test]
    async fn https_proxy_requires_a_valid_tls_certificate() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let (mut peer, _) = listener.accept().await.unwrap();
            let mut hello = [0; 5];
            peer.read_exact(&mut hello).await.unwrap();
            assert_eq!(hello[0], 22); // TLS handshake, no plaintext CONNECT/auth.
            peer.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
        });
        let proxy = AccountProxy::parse(&format!("https://private:secret@{address}")).unwrap();
        let error = proxy
            .connect("muse.ai", 443)
            .await
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("TLS"));
        assert!(!error.contains("secret"));
        peer.await.unwrap();
    }
}
