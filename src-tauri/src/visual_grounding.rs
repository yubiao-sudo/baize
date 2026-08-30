use std::io::Cursor;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::model::ProviderKind;

/// 运行时视觉模型（可被前端配置覆盖，默认读环境变量）
static VISION_MODEL: Mutex<Option<String>> = Mutex::new(None);

/// 视觉后端：Ollama 本地（默认，/api/generate）或 DeepSeek 云端（OpenAI 兼容 vision 接口）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisionProvider {
    Ollama,
    DeepSeekCloud,
}

static VISION_PROVIDER: Mutex<VisionProvider> = Mutex::new(VisionProvider::Ollama);
/// DeepSeek 云端 vision 的 base_url / api_key（复用主 LLM 云配置，启动与配置变更时同步）。
static VISION_CLOUD_BASE_URL: Mutex<String> = Mutex::new(String::new());
static VISION_CLOUD_API_KEY: Mutex<String> = Mutex::new(String::new());
/// 视觉模型总开关：关闭后所有视觉调用（visual_locate / som_select / describe_image）
/// 立即短路，GUI 走 OCR 兜底，图片描述走「无视觉」降级，避免无谓请求与卡顿。
static VISION_ENABLED: Mutex<bool> = Mutex::new(true);

pub fn vision_enabled() -> bool {
    *VISION_ENABLED.lock().unwrap()
}

pub fn set_vision_enabled(enabled: bool) {
    *VISION_ENABLED.lock().unwrap() = enabled;
}

pub fn vision_model() -> String {
    if let Some(m) = VISION_MODEL.lock().unwrap().as_ref() {
        return m.clone();
    }
    std::env::var("BAIZE_VISION_MODEL").unwrap_or_else(|_| "llava".to_string())
}

pub fn set_vision_model(m: String) {
    *VISION_MODEL.lock().unwrap() = Some(m);
}

pub fn vision_provider() -> VisionProvider {
    *VISION_PROVIDER.lock().unwrap()
}

/// 前端下拉值取 "ollama" / "deepseek"，其余值安全回退到 Ollama。
pub fn set_vision_provider(provider: &str) {
    *VISION_PROVIDER.lock().unwrap() = if provider.eq_ignore_ascii_case("deepseek") {
        VisionProvider::DeepSeekCloud
    } else {
        VisionProvider::Ollama
    };
}

/// 同步 DeepSeek 云端 vision 配置（复用主 LLM 的 cloud_base_url / cloud_api_key）。
pub fn set_vision_cloud(base_url: &str, api_key: &str) {
    *VISION_CLOUD_BASE_URL.lock().unwrap() = base_url.trim_end_matches('/').to_string();
    *VISION_CLOUD_API_KEY.lock().unwrap() = api_key.trim().to_string();
}

/// 多模态主模型连接：激活主模型勾选 multimodal 时由配置同步写入 (协议, base_url, api_key, model)。
/// None 表示当前未启用「复用主模型做视觉」。
static MULTIMODAL_MAIN: Mutex<Option<(ProviderKind, String, String, String)>> = Mutex::new(None);

fn multimodal_main() -> Option<(ProviderKind, String, String, String)> {
    MULTIMODAL_MAIN.lock().unwrap().clone()
}

/// 依据模型配置同步「多模态主模型」连接：激活项勾选 multimodal 且有有效凭据时写入，否则清空。
pub fn sync_multimodal_main(config: &crate::model::ModelConfig) {
    *MULTIMODAL_MAIN.lock().unwrap() = config.multimodal_conn();
}

/// 视觉模型熔断：遇到网络层失败（Ollama 未安装 / 未启动 / 模型不存在 / 请求超时）后，
/// 在冷却期内不再发起 HTTP 请求，直接短路返回。避免每次点击都各等 15 秒超时，
/// 导致 click_text 长期持有全局串行锁、整个白泽卡顿无响应。
static VISION_FAILURE_AT: Mutex<Option<Instant>> = Mutex::new(None);
/// 熔断冷却期：失败后 60 秒内跳过视觉模型，仅靠 OCR 一级兜底。
const VISION_COOLDOWN: Duration = Duration::from_secs(60);

/// 熔断是否处于打开状态（近期失败过，应跳过视觉模型调用）。
pub fn vision_circuit_open() -> bool {
    match *VISION_FAILURE_AT.lock().unwrap() {
        Some(at) => at.elapsed() < VISION_COOLDOWN,
        None => false,
    }
}

/// 记录一次网络层失败，进入冷却期。
pub fn record_vision_failure() {
    *VISION_FAILURE_AT.lock().unwrap() = Some(Instant::now());
}

/// 记录一次成功调用，清除熔断。
pub fn record_vision_success() {
    *VISION_FAILURE_AT.lock().unwrap() = None;
}

/// 统一视觉调用：根据配置的 provider 决定请求端点与格式，返回模型文本回复。
/// 网络层失败（连接 / 超时 / 非 2xx / 解析失败）会记录失败进入熔断；2xx 成功会清除熔断。
pub fn vision_generate_raw(base64: &str, prompt: &str, timeout: Duration) -> Result<String, String> {
    if !vision_enabled() {
        return Err("视觉模型已关闭（可在设置 → 运行时模型中开启）".to_string());
    }
    if vision_circuit_open() {
        return Err("视觉模型暂不可用（近期调用失败已熔断）".to_string());
    }

    // 优先复用「多模态主模型」：勾选后直接让主模型看图，不再走独立视觉模型
    if let Some((kind, base_url, api_key, model)) = multimodal_main() {
        return vision_generate_multimodal(kind, &base_url, &api_key, &model, base64, prompt, timeout);
    }

    let provider = vision_provider();
    let model = vision_model();
    let (url, body, bearer): (String, Value, Option<String>) = match provider {
        VisionProvider::Ollama => {
            let base_url = std::env::var("BAIZE_OLLAMA_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());
            (
                format!("{}/api/generate", base_url.trim_end_matches('/')),
                json!({ "model": model, "prompt": prompt, "images": [base64], "stream": false }),
                None,
            )
        }
        VisionProvider::DeepSeekCloud => {
            let base_url = VISION_CLOUD_BASE_URL.lock().unwrap().clone();
            let api_key = VISION_CLOUD_API_KEY.lock().unwrap().clone();
            if base_url.is_empty() || api_key.is_empty() {
                return Err("未配置 DeepSeek 云端（请在模型设置里启用云端并填写 API Key）".to_string());
            }
            (
                format!("{}/chat/completions", base_url),
                json!({
                    "model": model,
                    "messages": [{
                        "role": "user",
                        "content": [
                            { "type": "text", "text": prompt },
                            { "type": "image_url", "image_url": { "url": format!("data:image/png;base64,{base64}") } }
                        ]
                    }],
                    "stream": false
                }),
                Some(api_key),
            )
        }
    };

    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| {
            record_vision_failure();
            format!("创建 HTTP 客户端失败: {e}")
        })?;

    let mut req = client.post(&url).json(&body);
    if let Some(key) = bearer {
        req = req.bearer_auth(key);
    }
    let resp = req.send().map_err(|e| {
        record_vision_failure();
        format!("视觉模型请求失败: {e}")
    })?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().unwrap_or_default();
        record_vision_failure();
        return Err(format!(
            "视觉模型 HTTP {status}: {}",
            text.chars().take(300).collect::<String>()
        ));
    }
    let v: Value = resp.json().map_err(|e| {
        record_vision_failure();
        format!("解析响应失败: {e}")
    })?;
    // 正常拿到响应，清除熔断；模型回复「NONE / 0 / 坐标无法解析」不算网络故障，不熔断。
    record_vision_success();
    match provider {
        VisionProvider::Ollama => Ok(v["response"].as_str().unwrap_or("").to_string()),
        VisionProvider::DeepSeekCloud => Ok(v["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .to_string()),
    }
}

/// 复用「多模态主模型」直接做视觉推理（按协议构造带图请求）。
/// 网络层失败同样进入熔断；2xx 成功清除熔断。
fn vision_generate_multimodal(
    kind: ProviderKind,
    base_url: &str,
    api_key: &str,
    model: &str,
    base64: &str,
    prompt: &str,
    timeout: Duration,
) -> Result<String, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| {
            record_vision_failure();
            format!("创建 HTTP 客户端失败: {e}")
        })?;
    let base = base_url.trim_end_matches('/');
    let (url, body, bearer): (String, Value, Option<String>) = match kind {
        ProviderKind::Ollama => (
            format!("{base}/api/generate"),
            json!({ "model": model, "prompt": prompt, "images": [base64], "stream": false }),
            None,
        ),
        ProviderKind::OpenAI => (
            format!("{base}/chat/completions"),
            json!({
                "model": model,
                "messages": [{
                    "role": "user",
                    "content": [
                        { "type": "text", "text": prompt },
                        { "type": "image_url", "image_url": { "url": format!("data:image/png;base64,{base64}") } }
                    ]
                }],
                "stream": false
            }),
            Some(api_key.to_string()),
        ),
        ProviderKind::Anthropic => (
            format!("{base}/messages"),
            json!({
                "model": model,
                "max_tokens": 4096,
                "messages": [{
                    "role": "user",
                    "content": [
                        { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": base64 } },
                        { "type": "text", "text": prompt }
                    ]
                }]
            }),
            None,
        ),
        ProviderKind::Gemini => (
            format!("{base}/models/{model}:generateContent?key={api_key}"),
            json!({
                "contents": [{
                    "role": "user",
                    "parts": [
                        { "inline_data": { "mime_type": "image/png", "data": base64 } },
                        { "text": prompt }
                    ]
                }]
            }),
            None,
        ),
    };

    let mut req = client.post(&url).json(&body);
    if let Some(key) = bearer {
        req = req.bearer_auth(key);
    }
    if matches!(kind, ProviderKind::Anthropic) {
        req = req
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01");
    }
    let resp = req.send().map_err(|e| {
        record_vision_failure();
        format!("多模态模型请求失败: {e}")
    })?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().unwrap_or_default();
        record_vision_failure();
        return Err(format!(
            "多模态模型 HTTP {status}: {}",
            text.chars().take(300).collect::<String>()
        ));
    }
    let v: Value = resp.json().map_err(|e| {
        record_vision_failure();
        format!("解析响应失败: {e}")
    })?;
    record_vision_success();
    match kind {
        ProviderKind::Ollama => Ok(v["response"].as_str().unwrap_or("").to_string()),
        ProviderKind::OpenAI => Ok(v["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .to_string()),
        ProviderKind::Anthropic => {
            let mut text = String::new();
            if let Some(blocks) = v["content"].as_array() {
                for b in blocks {
                    if b["type"] == "text" {
                        if let Some(t) = b["text"].as_str() {
                            text.push_str(t);
                        }
                    }
                }
            }
            Ok(text)
        }
        ProviderKind::Gemini => {
            let parts = &v["candidates"][0]["content"]["parts"];
            let mut text = String::new();
            if let Some(arr) = parts.as_array() {
                for p in arr {
                    if let Some(t) = p["text"].as_str() {
                        text.push_str(t);
                    }
                }
            }
            Ok(text)
        }
    }
}

/// 视觉 grounding 二级：用视觉模型在截图中定位目标控件中心坐标
pub fn visual_locate(image_path: &str, target: &str) -> Result<(f64, f64), String> {
    let (base64, scale) = load_base64_downscaled(image_path, 896);
    if base64.is_empty() {
        return Err("读取或缩放截图失败".to_string());
    }
    let prompt = format!(
        "图中有一个叫「{target}」的控件（按钮/输入框等）。请只回复它中心点的像素坐标，格式 x,y。找不到就回复 NONE。"
    );

    let result = std::thread::spawn(move || -> Result<(f64, f64), String> {
        let text = vision_generate_raw(&base64, &prompt, Duration::from_secs(15))?;
        parse_coord(&text).map(|(x, y)| (x / scale, y / scale))
    })
    .join();

    match result {
        Ok(r) => r,
        Err(_) => {
            // spawn 线程 panic 也视为视觉模型异常，进入熔断。
            record_vision_failure();
            Err("视觉 grounding 线程异常退出".to_string())
        }
    }
}

/// 读取图片并按最长边缩放到 max_side 以内，返回 (base64 PNG, 缩放比例 scale = small/original)。
/// 视觉模型按小图推理，可大幅降低图像 token 与推理耗时；模型返回的坐标需 /scale 映射回原图。
pub fn load_base64_downscaled(path: &str, max_side: u32) -> (String, f64) {
    let Ok(img) = image::open(path) else {
        return (String::new(), 1.0);
    };
    let img = img.to_rgb8();
    let (w, h) = img.dimensions();
    let long = w.max(h);
    let scale = if long > max_side {
        max_side as f64 / long as f64
    } else {
        1.0
    };
    let nw = ((w as f64) * scale).max(1.0) as u32;
    let nh = ((h as f64) * scale).max(1.0) as u32;
    let small = image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Triangle);
    let mut buf = Vec::new();
    match small.write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Png) {
        Ok(_) => (base64_encode(&buf), scale),
        Err(_) => (String::new(), 1.0),
    }
}

/// 用视觉模型描述图片内容（供附件图片解析：OCR 无文字时兜底理解图像）
pub fn describe_image(image_path: &str, hint: &str) -> Result<String, String> {
    let bytes = std::fs::read(image_path).map_err(|e| format!("读图片失败: {e}"))?;
    let base64 = base64_encode(&bytes);
    let prompt = if hint.trim().is_empty() {
        "请用简洁的中文描述这张图片的内容（主体、场景，图中文字如有请原文转述）。".to_string()
    } else {
        format!("用户上传了一张图片，相关背景：{hint}。请用简洁的中文描述图片内容（主体、场景，图中文字如有请原文转述）。")
    };

    std::thread::spawn(move || -> Result<String, String> {
        vision_generate_raw(&base64, &prompt, Duration::from_secs(30))
            .map(|s| s.trim().to_string())
    })
    .join()
    .map_err(|_| "视觉模型线程异常退出".to_string())?
}

/// 从模型回复里解析 "x,y" 坐标
fn parse_coord(text: &str) -> Result<(f64, f64), String> {
    let text = text.trim();
    if text.eq_ignore_ascii_case("NONE") || text.is_empty() {
        return Err("未找到目标".to_string());
    }
    let nums: Vec<f64> = text
        .split(|c: char| !c.is_ascii_digit() && c != '.' && c != '-')
        .filter_map(|s| s.parse::<f64>().ok())
        .collect();
    if nums.len() >= 2 {
        Ok((nums[0], nums[1]))
    } else {
        Err(format!("无法解析坐标: {text}"))
    }
}

/// 极简 base64 编码（避免额外依赖）
pub fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}
