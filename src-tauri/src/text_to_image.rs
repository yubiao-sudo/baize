//! 文生图能力检测 + 图片生成。
//!
//! 检测策略（「模型名预判 + 运行时探测」两者结合）：
//! 1. 先按当前生效模型的模型名做关键词预判（已知的文生图模型）——快、零成本；
//! 2. 名称未命中时，再向服务端做一次轻量探测兜底：
//!    - 本地 Ollama：`/api/show` 读取模型 `capabilities`；
//!    - 云端：`/v1/models` 列表里扫描是否存在文生图模型。
//! 仅当预判或探测任一认定「支持」时才开启文生图功能，否则前端给出提示。

use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};

use crate::model::{ModelConfig, ModelTier};
use crate::AppState;

/// 文生图能力检测结果（返回给前端）
#[derive(Debug, Clone, Serialize)]
pub struct ImageCapability {
    pub supported: bool,
    pub model: String,
    /// "local" / "cloud" / ""（无可用模型）
    pub tier: String,
    /// "name" = 模型名预判命中；"probe" = 运行时探测命中；"none" = 均未命中
    pub source: String,
    /// 给用户看的提示文案
    pub hint: String,
}

/// 已知「支持文生图」的模型名关键词（小写匹配）
const IMAGE_KEYWORDS: &[&str] = &[
    "dall-e",
    "dalle",
    "gpt-image",
    "imagen",
    "flux",
    "stable-diffusion",
    "sdxl",
    "sd3",
    "kandinsky",
    "playground",
    "midjourney",
    "niji",
    "ideogram",
    "recraft",
    "wanx",
    "hunyuan-image",
    "cogview",
    "seedream",
    "qwen-image",
    "kolors",
    "pixart",
    "auraflow",
    "nano-banana",
    "nanobanana",
    "gemini-2.5-flash-image",
];

/// 模型名是否「看起来像」文生图模型（关键词预判）
fn name_indicates_image(model: &str) -> bool {
    let m = model.to_lowercase();
    IMAGE_KEYWORDS.iter().any(|k| m.contains(k))
}

/// 取得当前真正生效的首要模型（tier + 模型名），与 `ModelConfig::build_providers` 的排序一致
fn active_model(config: &ModelConfig) -> Option<(ModelTier, String)> {
    let cloud_ok = config.cloud_enabled && !config.cloud_api_key.trim().is_empty();
    let cloud_first = config.priority == "cloud";
    if cloud_first {
        if cloud_ok {
            return Some((ModelTier::Cloud, config.cloud_model.clone()));
        }
        if config.local_enabled {
            return Some((ModelTier::Local, config.local_model.clone()));
        }
    } else {
        if config.local_enabled {
            return Some((ModelTier::Local, config.local_model.clone()));
        }
        if cloud_ok {
            return Some((ModelTier::Cloud, config.cloud_model.clone()));
        }
    }
    None
}

/// 本地 Ollama 探测：读取模型 capabilities，判断是否支持文生图
async fn probe_ollama(base_url: &str, model: &str) -> bool {
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(6))
        .build()
    else {
        return false;
    };
    let url = format!("{}/api/show", base_url.trim_end_matches('/'));
    let Ok(resp) = client.post(&url).json(&json!({ "model": model })).send().await else {
        return false;
    };
    if !resp.status().is_success() {
        return false;
    }
    let Ok(v) = resp.json::<Value>().await else {
        return false;
    };
    let Some(caps) = v["capabilities"].as_array() else {
        return false;
    };
    caps.iter().any(|c| {
        let s = c.as_str().unwrap_or("").to_lowercase();
        s.contains("image") || s.contains("diffusion") || s.contains("txt2img") || s.contains("draw")
    })
}

/// 云端探测：`/v1/models` 列表中是否包含文生图模型名（作为名称未命中时的兜底）
async fn probe_cloud(base_url: &str, api_key: &str) -> bool {
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(6))
        .build()
    else {
        return false;
    };
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let Ok(resp) = client.get(&url).bearer_auth(api_key).send().await else {
        return false;
    };
    if !resp.status().is_success() {
        return false;
    }
    let Ok(v) = resp.json::<Value>().await else {
        return false;
    };
    let Some(list) = v["data"].as_array() else {
        return false;
    };
    list.iter().any(|m| {
        let id = m["id"].as_str().unwrap_or("").to_lowercase();
        name_indicates_image(&id)
    })
}

/// 检测当前配置的模型是否支持文生图
pub async fn detect(state: &AppState) -> ImageCapability {
    let config = crate::load_model_config(&state.store);
    let Some((tier, model)) = active_model(&config) else {
        return ImageCapability {
            supported: false,
            model: String::new(),
            tier: String::new(),
            source: "none".into(),
            hint: "未配置可用的模型，请先在设置中启用本地或云端模型。".into(),
        };
    };

    let tier_str = match tier {
        ModelTier::Local => "local",
        ModelTier::Cloud => "cloud",
    };
    let model_display = model_name_for_display(tier_str, &model);

    // 1) 模型名预判
    if name_indicates_image(&model) {
        return ImageCapability {
            supported: true,
            model,
            tier: tier_str.into(),
            source: "name".into(),
            hint: format!("当前模型「{model_display}」支持文生图"),
        };
    }

    // 2) 运行时探测兜底
    let probed = match tier {
        ModelTier::Local => probe_ollama(&config.local_url, &model).await,
        ModelTier::Cloud => probe_cloud(&config.cloud_base_url, &config.cloud_api_key).await,
    };
    if probed {
        return ImageCapability {
            supported: true,
            model,
            tier: tier_str.into(),
            source: "probe".into(),
            hint: format!("已通过运行时探测确认「{model_display}」支持文生图"),
        };
    }

    ImageCapability {
        supported: false,
        model,
        tier: tier_str.into(),
        source: "none".into(),
        hint: format!(
            "当前模型「{model_display}」不支持文生图，请在设置中切换到支持文生图的模型。"
        ),
    }
}

fn model_name_for_display(tier: &str, model: &str) -> String {
    if tier == "local" {
        format!("{model}（本地 Ollama）")
    } else {
        model.to_string()
    }
}

/// 生成图片：目前文生图通过云端 OpenAI 兼容的 `/images/generations` 接口完成，
/// 返回 `data:image/...;base64,...` 或图片 URL 字符串，前端可直接作为 `<img src>` 渲染。
pub async fn generate(state: &AppState, prompt: &str, size: Option<&str>) -> Result<String, String> {
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return Err("请输入要生成的图片描述".to_string());
    }

    let config = crate::load_model_config(&state.store);
    let cap = detect(state).await;
    if !cap.supported {
        return Err(cap.hint);
    }
    // 目前只有云端提供文生图接口
    if !(config.cloud_enabled && !config.cloud_api_key.trim().is_empty()) {
        return Err("当前为本地模型，暂不支持文生图；请在设置中启用并配置云端文生图模型。".to_string());
    }

    let url = format!(
        "{}/images/generations",
        config.cloud_base_url.trim_end_matches('/')
    );
    let body = json!({
        "model": config.cloud_model,
        "prompt": prompt,
        "n": 1,
        "size": size.unwrap_or("1024x1024"),
        "response_format": "b64_json",
    });

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;
    let resp = client
        .post(&url)
        .bearer_auth(config.cloud_api_key.trim())
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("文生图请求失败: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| format!("读取响应失败: {e}"))?;
    if !status.is_success() {
        return Err(format!("文生图失败 {status}: {text}"));
    }
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("解析响应失败: {e}"))?;
    let data = v["data"].as_array().and_then(|a| a.first()).ok_or("响应中无图片数据")?;
    if let Some(b64) = data["b64_json"].as_str() {
        if !b64.is_empty() {
            return Ok(format!("data:image/png;base64,{b64}"));
        }
    }
    if let Some(img_url) = data["url"].as_str() {
        if !img_url.is_empty() {
            return Ok(img_url.to_string());
        }
    }
    Err("响应中未包含可用的图片数据".to_string())
}