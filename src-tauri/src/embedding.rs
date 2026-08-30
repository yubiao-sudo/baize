use std::sync::Mutex;
use std::time::Duration;

use serde_json::{json, Value};

/// 运行时 embedding 模型（可被前端配置覆盖，默认读环境变量）
static EMBED_MODEL: Mutex<Option<String>> = Mutex::new(None);

pub fn embed_model() -> String {
    if let Some(m) = EMBED_MODEL.lock().unwrap().as_ref() {
        return m.clone();
    }
    std::env::var("BAIZE_EMBED_MODEL").unwrap_or_else(|_| "nomic-embed-text".to_string())
}

pub fn set_embed_model(m: String) {
    *EMBED_MODEL.lock().unwrap() = Some(m);
}

/// 用 Ollama 生成 embedding 向量（阻塞，独立线程执行，避免在 async 上下文 drop 运行时）
pub fn embed(text: &str) -> Result<Vec<f32>, String> {
    let text = text.to_string();
    let model = embed_model();
    let base_url = std::env::var("BAIZE_OLLAMA_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());

    std::thread::spawn(move || -> Result<Vec<f32>, String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;
        let resp = client
            .post(format!("{}/api/embed", base_url.trim_end_matches('/')))
            .json(&json!({ "model": model, "input": text }))
            .send()
            .map_err(|e| format!("embedding 请求失败: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("embedding HTTP {}", resp.status()));
        }
        let v: Value = resp.json().map_err(|e| format!("解析响应失败: {e}"))?;
        let arr = v["embeddings"][0]
            .as_array()
            .ok_or_else(|| "响应缺少 embeddings".to_string())?;
        Ok(arr.iter().filter_map(|x| x.as_f64()).map(|x| x as f32).collect())
    })
    .join()
    .map_err(|_| "embedding 线程异常退出".to_string())?
}

/// 余弦相似度
pub fn cosine(a: &[f32], b: &[f32]) -> f64 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0f64, 0f64, 0f64);
    for i in 0..a.len() {
        let x = a[i] as f64;
        let y = b[i] as f64;
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}
