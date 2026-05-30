use axum::{
    routing::get,
    Router,
    extract::ConnectInfo,
};
use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tower_http::{
    trace::{TraceLayer, OnResponse},
    cors::{CorsLayer, Any},
};
use tracing::{Level, info, Span};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use crate::utils::error::AppError;

#[derive(Parser)]
#[command(name = "jiangtokoto-server")]
#[command(about = "表情包服务器 - Jiangtokoto Meme Server", long_about = None)]
#[command(version)]
struct Cli {
    /// 配置文件路径
    #[arg(short, long, env = "JIANGTOKOTO_CONFIG")]
    config: Option<PathBuf>,
}

#[derive(Clone)]
struct CustomOnResponse;

impl<B> OnResponse<B> for CustomOnResponse {
    fn on_response(self, response: &axum::response::Response<B>, latency: Duration, span: &Span) {
        let status = response.status();
        info!(parent: span,
            status = %status,
            latency = ?latency,
            "Request completed"
        );
    }
}

mod config;
mod handlers;
mod models;
mod services;
mod utils;
mod openapi;
mod metrics;

use std::process;

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("Error: {e}");
        process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // 初始化指标
    metrics::init_metrics();

    // 记录服务启动时间
    let start_time = std::time::SystemTime::now();
    metrics::set_service_start_time(start_time);

    // 加载配置文件
    let config = match &cli.config {
        Some(path) => config::Config::load_from_file(path)?,
        None => config::Config::load_or_create_default("config.yml")?,
    };
    
    // 确保日志目录存在
    std::fs::create_dir_all(&config.logging.directory)?;

    // 设置文件日志appender
    let file_appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(&config.logging.file_prefix)
        .filename_suffix("log")
        .build(&config.logging.directory)  // 使用 build() 方法直接指定目录
        .expect("创建日志文件失败");

    // 初始化日志系统
    let log_level = std::env::var("LOG_LEVEL")
        .unwrap_or_else(|_| "info".to_string());
    
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(log_level))
        .with(tracing_subscriber::fmt::layer()
            .with_writer(file_appender)
            .with_ansi(false)
            .with_file(true)
            .with_line_number(true)
            .with_thread_ids(true)
            .with_thread_names(true)
            .with_target(false))
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stdout))
        .init();

    tracing::info!("Logging system initialized");
    tracing::info!("Configuration loaded successfully");

    // 初始化 MemeService
    let state = services::meme::MemeService::new(
        &config.storage.memes_dir,
        config.cache.max_size,
        config.cache.ttl_secs,
    ).await?;

    // 配置 CORS
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    // 构建应用路由
    let config_clone = Arc::new(config.clone());
    let app = Router::new()
        .route("/", get(|| async { axum::response::Redirect::to("/swagger-ui") }))
        .route("/memes/random", get(handlers::meme::random_meme))
        .route("/memes/list", get(handlers::meme::list_memes))
        .route("/memes/get/:id", get(handlers::meme::get_meme_by_id))
        .route("/memes/health", get(handlers::meme::health_check))
        .route("/memes/count", get(handlers::meme::get_meme_count))
        .route("/statistics", get(handlers::statistics::get_statistics))
        .route("/metrics", get(handlers::meme::get_metrics))
        .merge(openapi::create_swagger_ui(config.swagger.clone()))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(move |request: &axum::http::Request<_>| {
                    let remote_addr = if config_clone.server.proxy.enabled {
                        request
                            .headers()
                            .get(&config_clone.server.proxy.ip_header)
                            .and_then(|h| h.to_str().ok())
                            .map(|s| s.split(',').next().unwrap_or(s).trim().to_string())
                            .unwrap_or_else(|| "unknown".to_string())
                    } else {
                        request
                            .extensions()
                            .get::<ConnectInfo<SocketAddr>>()
                            .map(|ci| ci.0.ip().to_string())
                            .unwrap_or_else(|| "unknown".to_string())
                    };

                    tracing::span!(
                        Level::INFO,
                        "request",
                        method = %request.method(),
                        uri = %request.uri(),
                        ip = %remote_addr,
                    )
                })
                .on_response(CustomOnResponse)
        )
        .layer(cors)
        .with_state(state);

    // 设置服务器地址
    let addr: SocketAddr = format!("{}:{}", config.server.host, config.server.port)
        .parse()
        .map_err(|e| AppError::Internal(format!("Invalid address: {}", e)))?;
    tracing::info!("Server started on {}", addr);

    // 启动服务器
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("Server started on {}", addr);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>()
    ).await?;

    Ok(())
}