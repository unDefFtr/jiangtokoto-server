use axum::{
    extract::{State, Path, Query},
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn, error};
use serde::Serialize;
use serde::Deserialize;

use utoipa::ToSchema;

use crate::services::meme::MemeService;
use crate::utils::error::AppError;
use crate::metrics::{REQUEST_COUNTER, RESPONSE_TIME};

#[derive(Deserialize, ToSchema, utoipa::IntoParams)]
pub struct RandomMemeQuery {
    #[schema(example = false)]
    redirect: Option<bool>,
    #[schema(example = 300)]
    width: Option<u32>,
    #[schema(example = 300)]
    height: Option<u32>,
}

#[derive(Deserialize, ToSchema, utoipa::IntoParams)]
pub struct GetMemeQuery {
    #[schema(example = 300)]
    width: Option<u32>,
    #[schema(example = 300)]
    height: Option<u32>,
}

#[derive(Serialize, ToSchema)]
pub struct MemeListItem {
    #[schema(example = 1)]
    pub id: u32,
    #[schema(example = "image/jpeg")]
    pub mime_type: String,
    #[schema(example = "funny_meme.jpg")]
    pub filename: String,
    #[schema(example = 1024)]
    pub size_bytes: u64,
}

#[derive(Serialize, ToSchema)]
pub struct MemeCount {
    #[schema(example = 100)]
    pub count: usize,
}

/// 获取随机表情包
#[utoipa::path(
    get,
    path = "/memes/random",
    tag = "memes",
    params(RandomMemeQuery),
    responses(
        (status = 200, description = "成功返回随机表情包图片", content_type = "image/*"),
        (status = 302, description = "重定向到指定表情包", headers(
            ("Location" = String, description = "重定向URL")
        )),
        (status = 500, description = "服务器内部错误")
    )
)]
pub async fn random_meme(
    State(state): State<Arc<RwLock<MemeService>>>,
    Query(query): Query<RandomMemeQuery>,
) -> impl IntoResponse {
    REQUEST_COUNTER.inc();
    let _timer = crate::metrics::Timer::new(&RESPONSE_TIME);
    let state = state.read().await;
    
    match state.get_random().await {
        Ok((meme, content)) => {
            // 如果设置了 redirect 参数，则重定向到 get 端点
            if query.redirect.unwrap_or(false) {
                let mut headers = HeaderMap::new();
                let mut redirect_url = format!("/memes/get/{}", meme.id);
                
                // 添加压缩参数到重定向 URL（不包含 redirect 参数）
                if query.width.is_some() || query.height.is_some() {
                    redirect_url.push('?');
                    let mut params = Vec::new();
                    if let Some(width) = query.width {
                        params.push(format!("width={}", width));
                    }
                    if let Some(height) = query.height {
                        params.push(format!("height={}", height));
                    }
                    redirect_url.push_str(&params.join("&"));
                }
                
                headers.insert(
                    header::LOCATION,
                    redirect_url.parse().unwrap()
                );
                return (StatusCode::FOUND, headers, Vec::new());
            }

            let mut resp_headers = HeaderMap::new();
            
            // 使用优化的压缩图片方法
            let (final_meme, content) = if query.width.is_some() || query.height.is_some() {
                match state.get_resized_image(meme.id, query.width, query.height).await {
                    Ok((resized_meme, resized_content)) => {
                        resp_headers.insert(header::CONTENT_TYPE, "image/png".parse().unwrap());
                        (resized_meme, resized_content)
                    }
                    Err(e) => {
                        error!("Failed to get compressed image: {}", e);
                        return (StatusCode::INTERNAL_SERVER_ERROR, HeaderMap::new(), Vec::new());
                    }
                }
            } else {
                resp_headers.insert(header::CONTENT_TYPE, meme.mime_type.parse().unwrap());
                (meme, content)
            };

            // 记录访问信息
            info!(
                meme_id = final_meme.id,
                mime_type = %final_meme.mime_type,
                file_size = final_meme.size_bytes,
                cache_used = query.width.is_some() || query.height.is_some(),
                "Serving random meme"
            );

            (StatusCode::OK, resp_headers, content)
        }
        Err(_) => {
            error!("Failed to get meme");
            (StatusCode::INTERNAL_SERVER_ERROR, HeaderMap::new(), Vec::new())
        }
    }
}

/// 获取表情包列表
#[utoipa::path(
    get,
    path = "/memes/list",
    tag = "memes",
    responses(
        (status = 200, description = "成功返回表情包列表", body = Vec<MemeListItem>)
    )
)]
pub async fn list_memes(
    State(state): State<Arc<RwLock<MemeService>>>,
) -> Json<Vec<MemeListItem>> {
    let service = state.read().await;
    let memes = service.get_all_memes();
    
    let mut meme_list: Vec<MemeListItem> = memes.into_iter()
        .map(|(id, meme)| MemeListItem {
            id: *id,
            mime_type: meme.mime_type.clone(),
            filename: meme.filename.clone(),
            size_bytes: meme.size_bytes,
        })
        .collect();
    
    // 按 id 排序
    meme_list.sort_by_key(|meme| meme.id);
    
    Json(meme_list)
}

/// 根据ID获取表情包
#[utoipa::path(
    get,
    path = "/memes/get/{id}",
    tag = "memes",
    params(
        ("id" = u32, Path, description = "表情包ID"),
        GetMemeQuery
    ),
    responses(
        (status = 200, description = "成功返回指定表情包图片", content_type = "image/*"),
        (status = 404, description = "表情包不存在"),
        (status = 500, description = "服务器内部错误")
    )
)]
pub async fn get_meme_by_id(
    State(state): State<Arc<RwLock<MemeService>>>,
    Path(id): Path<u32>,
    Query(query): Query<GetMemeQuery>,
) -> impl IntoResponse {
    REQUEST_COUNTER.inc();
    let _timer = crate::metrics::Timer::new(&RESPONSE_TIME);
    let state = state.read().await;
    
    // 使用优化的压缩图片方法
    let result = if query.width.is_some() || query.height.is_some() {
        state.get_resized_image(id, query.width, query.height).await
    } else {
        state.get_by_id(id).await
    };
    
    match result {
        Ok((meme, content)) => {
            let mut resp_headers = HeaderMap::new();
            
            // 根据是否压缩设置正确的Content-Type
            if query.width.is_some() || query.height.is_some() {
                resp_headers.insert(header::CONTENT_TYPE, "image/png".parse().unwrap());
            } else {
                resp_headers.insert(header::CONTENT_TYPE, meme.mime_type.parse().unwrap());
            }
            
            // 记录访问信息
            info!(
                meme_id = meme.id,
                mime_type = %meme.mime_type,
                file_size = meme.size_bytes,
                cache_used = query.width.is_some() || query.height.is_some(),
                "Serving meme by ID"
            );

            (StatusCode::OK, resp_headers, content)
        }
        Err(AppError::NotFound(msg)) => {
            warn!("Meme not found: {}", msg);
            (StatusCode::NOT_FOUND, HeaderMap::new(), Vec::new())
        }
        Err(_) => {
            error!("Failed to get meme");
            (StatusCode::INTERNAL_SERVER_ERROR, HeaderMap::new(), Vec::new())
        }
    }
}

/// 获取表情包总数
#[utoipa::path(
    get,
    path = "/memes/count",
    tag = "memes",
    responses(
        (status = 200, description = "成功返回表情包总数", body = MemeCount)
    )
)]
pub async fn get_meme_count(
    State(state): State<Arc<RwLock<MemeService>>>,
) -> Json<MemeCount> {
    let service = state.read().await;
    Json(MemeCount {
        count: service.get_total_memes(),
    })
}

/// 健康检查
#[utoipa::path(
    get,
    path = "/memes/health",
    tag = "memes",
    responses(
        (status = 200, description = "服务健康")
    )
)]
pub async fn health_check() -> impl IntoResponse {
    StatusCode::OK
}

/// 获取Prometheus指标
#[utoipa::path(
    get,
    path = "/metrics",
    tag = "monitoring",
    responses(
        (status = 200, description = "Prometheus metrics", content_type = "text/plain")
    )
)]
pub async fn get_metrics() -> impl IntoResponse {
    let metrics = crate::metrics::get_metrics();
    (StatusCode::OK, [("Content-Type", "text/plain; charset=utf-8")], metrics)
}