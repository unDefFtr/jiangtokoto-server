use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::{Duration, SystemTime, Instant},
    path::PathBuf,
};
use tokio::sync::{RwLock, broadcast};
use crate::utils::error::{Result, AppError};
use crate::models::meme::Meme;
use crate::metrics::{CACHE_HIT_RATE, CACHE_SIZE, CACHE_HITS, CACHE_MISSES, TOTAL_MEMES};
use tracing::{info, error, debug};
use notify::{RecursiveMode, Watcher};
use std::sync::atomic::{AtomicU64, Ordering};
use parking_lot::Mutex;
use sha2::{Sha256, Digest};

const REQUEST_HISTORY_WINDOW: Duration = Duration::from_secs(60 * 15); // 扩展到15分钟
const ONE_MINUTE: Duration = Duration::from_secs(60);
const FIVE_MINUTES: Duration = Duration::from_secs(60 * 5);
const FIFTEEN_MINUTES: Duration = Duration::from_secs(60 * 15);

#[derive(Debug)]
pub struct MemeService {
    memes: HashMap<u32, Meme>,
    // 预计算的ID向量，避免每次随机选择时重新收集
    meme_ids: Vec<u32>,
    total_count: u32,
    content_cache: moka::future::Cache<u32, Vec<u8>>,
    // 添加压缩图片缓存
    resized_cache: moka::future::Cache<String, Vec<u8>>,
    memes_dir: PathBuf,
    reload_tx: broadcast::Sender<()>,
    _watcher: notify::RecommendedWatcher,
    request_count: AtomicU64,
    cache_hits: AtomicU64,
    cache_misses: AtomicU64,
    start_time: SystemTime,
    request_timestamps: Mutex<VecDeque<Instant>>,
    last_updated: Mutex<SystemTime>,
}

impl MemeService {
    pub async fn new(memes_dir: &str, max_size: u64, ttl_secs: u64) -> Result<Arc<RwLock<Self>>> {
        let memes_dir = PathBuf::from(memes_dir);
        let (reload_tx, _) = broadcast::channel(1);
        
        // 创建文件监控
        let reload_tx_clone = reload_tx.clone();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            match res {
                Ok(event) => {
                    // 只输出变更的文件路径
                    for path in event.paths {
                        info!("File change detected: {}", path.display());
                    }
                    if let Err(e) = reload_tx_clone.send(()) {
                        error!("Failed to send reload signal: {}", e);
                    }
                }
                Err(e) => error!("File watch error: {}", e),
            }
        })?;

        // 开始监控目录
        watcher.watch(&memes_dir, RecursiveMode::Recursive)?;
        info!("Started watching directory: {:?}", memes_dir);

        // 初始化缓存 - 增加缓存容量
        let content_cache = moka::future::Cache::builder()
            .max_capacity(max_size)
            .time_to_live(Duration::from_secs(ttl_secs))
            .build();
            
        // 初始化压缩图片缓存
        let resized_cache = moka::future::Cache::builder()
            .max_capacity(max_size * 2) // 压缩图片缓存容量更大
            .time_to_live(Duration::from_secs(ttl_secs * 2)) // 压缩图片缓存时间更长
            .build();

        // 创建服务实例
        let service = Arc::new(RwLock::new(Self {
            memes: HashMap::new(),
            meme_ids: Vec::new(),
            total_count: 0,
            content_cache,
            resized_cache,
            memes_dir: memes_dir.clone(),
            reload_tx,
            _watcher: watcher,
            request_count: AtomicU64::new(0),
            cache_hits: AtomicU64::new(0),
            cache_misses: AtomicU64::new(0),
            start_time: SystemTime::now(),
            request_timestamps: Mutex::new(VecDeque::with_capacity(2000)), // 增加容量
            last_updated: Mutex::new(SystemTime::now()),
        }));

        // 初始加载表情包
        service.write().await.reload_memes().await?;

        // 启动重载监听器
        Self::start_reload_listener(Arc::clone(&service));

        Ok(service)
    }

    async fn reload_memes(&mut self) -> Result<()> {
        let mut memes = HashMap::new();
        let mut count = 0;

        let mut entries = tokio::fs::read_dir(&self.memes_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            if entry.file_type().await?.is_file() {
                let path = entry.path();
                let mime_type = mime_guess::from_path(&path)
                    .first_or_octet_stream()
                    .to_string();

                // 使用 to_string_lossy 来处理包含 emoji 或其他 Unicode 字符的文件名
                // 这样可以避免在 macOS 和 Linux 上因为 Unicode 规范化差异导致的问题
                let filename = path.file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| "unknown".to_string());

                let size_bytes = tokio::fs::metadata(&path)
                    .await
                    .map(|metadata| metadata.len())
                    .unwrap_or(0);

                // 计算文件名的 SHA-256 哈希值
                let mut hasher = Sha256::new();
                hasher.update(filename.as_bytes());
                let hash = hasher.finalize();
                
                // 使用哈希值的前 4 个字节作为 ID
                let id = u32::from_be_bytes([
                    hash[0],
                    hash[1],
                    hash[2],
                    hash[3],
                ]);

                let meme = Meme {
                    id,
                    path,
                    mime_type,
                    filename,
                    size_bytes,
                };
                
                memes.insert(id, meme);
                count += 1;
            }
        }

        if count == 0 {
            return Err(AppError::Internal("No memes found".to_string()));
        }

        // 更新服务状态
        self.memes = memes;
        // 预计算ID向量以提高随机选择性能
        self.meme_ids = self.memes.keys().copied().collect();
        self.total_count = count;
        self.content_cache.invalidate_all();
        self.resized_cache.invalidate_all();
        *self.last_updated.lock() = SystemTime::now();
        
        // 更新 Prometheus 指标
        TOTAL_MEMES.set(count as f64);

        info!("Reloaded {} memes", count);
        Ok(())
    }

    fn start_reload_listener(service: Arc<RwLock<Self>>) {
        tokio::spawn(async move {
            loop {
                let mut rx = {
                    let service = service.read().await;
                    service.reload_tx.subscribe()
                };

                // 等待重载信号
                while let Ok(()) = rx.recv().await {
                    info!("Reloading memes...");
                    if let Err(e) = service.write().await.reload_memes().await {
                        error!("Failed to reload memes: {}", e);
                    }
                }

                // 如果 channel 关闭，等待一段时间后重试
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });
    }

    pub async fn get_random(&self) -> Result<(&Meme, Vec<u8>)> {
        // 增加请求计数并记录时间戳
        self.request_count.fetch_add(1, Ordering::Relaxed);
        self.record_request();
        
        // 使用预计算的ID向量进行随机选择，避免每次重新收集
        if self.meme_ids.is_empty() {
            return Err(AppError::NotFound("No memes available".to_string()));
        }
        
        let random_index = fastrand::usize(..self.meme_ids.len());
        let meme_id = self.meme_ids[random_index];
        
        let meme = self.memes.get(&meme_id)
            .ok_or_else(|| AppError::NotFound("Meme not found".to_string()))?;

        // 尝试从缓存获取
        if let Some(content) = self.content_cache.get(&meme_id).await {
            self.cache_hits.fetch_add(1, Ordering::Relaxed);
            CACHE_HITS.inc(); // 更新 Prometheus 计数器
            self.update_cache_metrics();
            debug!(
                meme_id = meme_id,
                cache_type = "content",
                "Cache hit"
            );
            return Ok((meme, content));
        }

        // 如果缓存未命中，从文件读取
        self.cache_misses.fetch_add(1, Ordering::Relaxed);
        CACHE_MISSES.inc(); // 更新 Prometheus 计数器
        self.update_cache_metrics();
        debug!(
            meme_id = meme_id,
            cache_type = "content",
            "Cache miss"
        );
        let content = tokio::fs::read(&meme.path).await?;
        self.content_cache.insert(meme_id, content.clone()).await;
        
        Ok((meme, content))
    }

    pub fn get_request_count(&self) -> u64 {
        self.request_count.load(Ordering::Relaxed)
    }

    pub fn get_total_memes(&self) -> usize {
        self.memes.len()
    }

    pub fn get_start_time(&self) -> SystemTime {
        self.start_time
    }

    fn record_request(&self) {
        let mut timestamps = self.request_timestamps.lock();
        let now = Instant::now();
        
        // 移除超过一分钟的时间戳
        while timestamps.front()
            .map(|&t| now.duration_since(t) > REQUEST_HISTORY_WINDOW)
            .unwrap_or(false) 
        {
            timestamps.pop_front();
        }
        
        timestamps.push_back(now);
    }

    pub fn get_requests_in_window(&self, window: Duration) -> u64 {
        let now = Instant::now();
        let mut timestamps = self.request_timestamps.lock();
        
        // 清理超过窗口时间的记录
        while let Some(timestamp) = timestamps.front() {
            if now.duration_since(*timestamp) > REQUEST_HISTORY_WINDOW {
                timestamps.pop_front();
            } else {
                break;
            }
        }
        
        // 计算指定窗口内的请求数
        timestamps.iter()
            .filter(|&timestamp| now.duration_since(*timestamp) <= window)
            .count() as u64
    }

    pub fn get_requests_last_minute(&self) -> u64 {
        self.get_requests_in_window(ONE_MINUTE)
    }

    pub fn get_requests_last_5_minutes(&self) -> u64 {
        self.get_requests_in_window(FIVE_MINUTES)
    }

    pub fn get_requests_last_15_minutes(&self) -> u64 {
        self.get_requests_in_window(FIFTEEN_MINUTES)
    }

    pub fn get_last_updated(&self) -> SystemTime {
        *self.last_updated.lock()
    }

    pub fn get_cache_stats(&self) -> (u64, u64) {
        let hits = self.cache_hits.load(Ordering::Relaxed);
        let misses = self.cache_misses.load(Ordering::Relaxed);
        (hits, misses)
    }

    pub fn get_all_memes(&self) -> Vec<(&u32, &Meme)> {
        self.memes.iter().collect()
    }

    fn update_cache_metrics(&self) {
        let hits = self.cache_hits.load(Ordering::Relaxed);
        let misses = self.cache_misses.load(Ordering::Relaxed);
        let total = hits + misses;
        
        if total > 0 {
            let hit_rate = hits as f64 / total as f64;
            CACHE_HIT_RATE.set(hit_rate);
        }
        
        CACHE_SIZE.set(self.content_cache.entry_count() as f64);
    }

    pub async fn get_by_id(&self, id: u32) -> Result<(&Meme, Vec<u8>)> {
        // 增加请求计数并记录时间戳
        self.request_count.fetch_add(1, Ordering::Relaxed);
        self.record_request();
        
        let meme = self.memes.get(&id)
            .ok_or_else(|| AppError::NotFound(format!("Meme with id {} not found", id)))?;

        // 尝试从缓存获取
        if let Some(content) = self.content_cache.get(&id).await {
            self.cache_hits.fetch_add(1, Ordering::Relaxed);
            CACHE_HITS.inc(); // 更新 Prometheus 计数器
            self.update_cache_metrics();
            debug!(
                meme_id = id,
                cache_type = "content",
                "Cache hit"
            );
            return Ok((meme, content));
        }

        // 如果缓存未命中，从文件读取
        self.cache_misses.fetch_add(1, Ordering::Relaxed);
        CACHE_MISSES.inc(); // 更新 Prometheus 计数器
        self.update_cache_metrics();
        debug!(
            meme_id = id,
            cache_type = "content",
            "Cache miss"
        );
        let content = tokio::fs::read(&meme.path).await?;
        self.content_cache.insert(id, content.clone()).await;
        
        Ok((meme, content))
    }

    /// 获取压缩后的图片，支持缓存
    pub async fn get_resized_image(&self, id: u32, width: Option<u32>, height: Option<u32>) -> Result<(&Meme, Vec<u8>)> {
        let meme = self.memes.get(&id)
            .ok_or_else(|| AppError::NotFound(format!("Meme with id {} not found", id)))?;

        // 如果没有指定尺寸，直接返回原图
        if width.is_none() && height.is_none() {
            return self.get_by_id(id).await;
        }

        // 生成缓存键
        let cache_key = format!("{}:{}x{}", id, width.unwrap_or(0), height.unwrap_or(0));
        
        // 尝试从压缩图片缓存获取
        if let Some(content) = self.resized_cache.get(&cache_key).await {
            self.cache_hits.fetch_add(1, Ordering::Relaxed);
            CACHE_HITS.inc(); // 更新 Prometheus 计数器
            self.update_cache_metrics();
            debug!(
                meme_id = id,
                cache_type = "resized",
                cache_key = cache_key,
                "Cache hit"
            );
            return Ok((meme, content));
        }

        // 获取原图
        let (_, original_content) = self.get_by_id(id).await?;
        
        // 压缩图片
        let resized_content = tokio::task::spawn_blocking(move || {
            use image::{ImageFormat, imageops::FilterType};
            use std::io::Cursor;
            
            let img = image::load_from_memory(&original_content)
                .map_err(|e| AppError::Internal(format!("Failed to load image: {}", e)))?;
            
            let target_width = width.unwrap_or(img.width());
            let target_height = height.unwrap_or(img.height());
            
            // 使用更快的滤波器进行缩放
            let resized = img.resize(target_width, target_height, FilterType::Triangle);
            
            let mut cursor = Cursor::new(Vec::new());
            resized.write_to(&mut cursor, ImageFormat::Png)
                .map_err(|e| AppError::Internal(format!("Failed to encode image: {}", e)))?;
            
            Ok::<Vec<u8>, AppError>(cursor.into_inner())
        }).await
        .map_err(|e| AppError::Internal(format!("Task join error: {}", e)))??;

        // 缓存压缩后的图片
        self.resized_cache.insert(cache_key.clone(), resized_content.clone()).await;
        self.cache_misses.fetch_add(1, Ordering::Relaxed);
        self.update_cache_metrics();
        debug!(
            meme_id = id,
            cache_type = "resized",
            cache_key = cache_key,
            "Cache miss"
        );
        
        Ok((meme, resized_content))
    }
}