use std::path::PathBuf;
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meme {
    pub id: u32,
    pub path: PathBuf,
    pub mime_type: String,
    pub filename: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct MemeResponse {
    pub id: u32,
    pub mime_type: String,
}