use muse_api::config::Config;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Account-bound inspector used by the desktop model picker. Credentials are
    // inherited in environment, never arguments; no listener or chat is created.
    if std::env::args().any(|arg| arg == "--gemini-models") {
        let proxy = muse_api::infrastructure::outbound_proxy::AccountProxy::from_env()
            .map_err(|_| "Invalid Gemini account proxy")?;
        let generator = muse_api::adapters::gemini::GeminiGenerator::with_options(
            &std::env::var("GEMINI_COOKIE").map_err(|_| "Gemini cookie is not configured")?,
            &proxy,
            muse_api::adapters::gemini::Options::from_env()?,
        )?;
        println!(
            "{}",
            serde_json::to_string(&generator.catalog(true).await?)?
        );
        return Ok(());
    }
    // Khởi tạo logging; đọc mức log từ RUST_LOG, mặc định là muse_api=info.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "muse_api=info".into()),
        )
        .init();

    // Đọc cấu hình và mở cổng lắng nghe.
    let config = Config::from_env()?;
    if std::env::var("MUSE_BACKEND").as_deref() == Ok("gemini") {
        muse_api::adapters::gemini::Options::from_env()?;
    }
    if std::env::var("MUSE_BACKEND")
        .is_ok_and(|value| !matches!(value.as_str(), "muse" | "gemini"))
    {
        return Err("MUSE_BACKEND must be muse or gemini".into());
    }
    // Refuse invalid outbound proxy config before opening any Muse connection.
    muse_api::adapters::muse::proxy::AccountProxy::from_env()
        .map_err(|_| "Invalid MUSE_UPSTREAM_PROXY; expected an HTTP/HTTPS proxy URL")?;
    let listener = tokio::net::TcpListener::bind(config.bind_addr).await?;
    info!(address = %listener.local_addr()?, "server listening");

    // Chạy ứng dụng và dừng server khi nhận Ctrl+C.
    axum::serve(listener, muse_api::app())
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
