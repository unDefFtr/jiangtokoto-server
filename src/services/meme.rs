use std::{collections::HashMap, sync::Arc, time::Duration, path::PathBuf};
use tokio::sync::{RwLock, broadcast};
use crate::utils::error::{Result, AppError};
use crate::models::meme::Meme;
use tracing::{info, error};
use notify::{RecursiveMode, Watcher};

#[derive(Debug)]
pub struct MemeService {
    memes: HashMap<u32, Meme>,
    total_count: u32,
    content_cache: moka::future::Cache<u32, Vec<u8>>,
    memes_dir: PathBuf,
    reload_tx: broadcast::Sender<()>,
    _watcher: notify::RecommendedWatcher,
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
                        info!("检测到文件变更: {}", path.display());
                    }
                    if let Err(e) = reload_tx_clone.send(()) {
                        error!("发送重载信号失败: {}", e);
                    }
                }
                Err(e) => error!("监控文件出错: {}", e),
            }
        })?;

        // 开始监控目录
        watcher.watch(&memes_dir, RecursiveMode::Recursive)?;
        info!("开始监控目录: {:?}", memes_dir);

        // 初始化缓存
        let content_cache = moka::future::Cache::builder()
            .max_capacity(max_size)
            .time_to_live(Duration::from_secs(ttl_secs))
            .build();

        // 创建服务实例
        let service = Arc::new(RwLock::new(Self {
            memes: HashMap::new(),
            total_count: 0,
            content_cache,
            memes_dir: memes_dir.clone(),
            reload_tx,
            _watcher: watcher,
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

                let meme = Meme {
                    id: count,
                    path,
                    mime_type,
                };
                
                memes.insert(count, meme);
                count += 1;
            }
        }

        if count == 0 {
            return Err(AppError::Internal("No memes found".to_string()));
        }

        // 更新服务状态
        self.memes = memes;
        self.total_count = count;
        self.content_cache.invalidate_all();

        info!("重新加载了 {} 个表情包", count);
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
                    info!("正在重新加载表情包...");
                    if let Err(e) = service.write().await.reload_memes().await {
                        error!("重新加载表情包失败: {}", e);
                    }
                }

                // 如果 channel 关闭，等待一段时间后重试
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });
    }

    pub async fn get_random(&self) -> Result<(&Meme, Vec<u8>)> {
        let meme_id = fastrand::u32(..self.total_count);
        let meme = self.memes.get(&meme_id)
            .ok_or_else(|| AppError::NotFound("Meme not found".to_string()))?;

        // 尝试从缓存获取
        if let Some(content) = self.content_cache.get(&meme_id).await {
            tracing::debug!("Cache hit for meme {}", meme_id);
            return Ok((meme, content));
        }

        // 如果缓存未命中，从文件读取
        tracing::debug!("Cache miss for meme {}, reading from disk", meme_id);
        let content = tokio::fs::read(&meme.path).await?;
        self.content_cache.insert(meme_id, content.clone()).await;
        
        Ok((meme, content))
    }
}