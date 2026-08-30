//! TTS 语音模型（云端语音合成）。
//!
//! 双后端设计：
//!  - local：浏览器 speechSynthesis（前端直调，零配置）
//!  - cloud：OpenAI 兼容 /audio/speech 接口（豆包同源音色 / CosyVoice 等，
//!    如硅基流动 https://api.siliconflow.cn/v1 + FunAudioLLM/CosyVoice2-0.5B）
//!  - kokoro：本地 Kokoro-82M（F:\kokoro-tts，OpenAI 兼容本地服务，免费离线 52 音色）
//!
//! 云端合成由后端请求 → 音频落盘临时目录 → 前端 <audio> 播放（可做真实频谱律动）。

use serde::{Deserialize, Serialize};
use std::sync::RwLock;

/// 语音模型配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsConfig {
    /// "local" = 浏览器内置语音 | "cloud" = OpenAI 兼容语音模型 | "doubao" = 豆包语音合成（火山引擎）
    pub provider: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub voice: String,
    // ── 豆包（火山引擎）专用 ──
    #[serde(default)]
    pub db_app_id: String,
    #[serde(default)]
    pub db_token: String,
    /// 音色 ID（speaker），如 zh_female_xiaohe_uranus_bigtts / S_xxx 复刻音色
    #[serde(default)]
    pub db_speaker: String,
    /// 语速 -50~100，默认 0
    #[serde(default)]
    pub db_speech_rate: i32,
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            provider: "local".into(),
            base_url: String::new(),
            api_key: String::new(),
            model: String::new(),
            voice: String::new(),
            db_app_id: String::new(),
            db_token: String::new(),
            db_speaker: String::new(),
            db_speech_rate: 0,
        }
    }
}

static CFG: RwLock<Option<TtsConfig>> = RwLock::new(None);

pub fn set_config(cfg: TtsConfig) {
    *CFG.write().unwrap() = Some(cfg);
}

pub fn current() -> TtsConfig {
    CFG.read()
        .unwrap()
        .clone()
        .unwrap_or_default()
}

/// 从持久化恢复进程内缓存（AppState 初始化时调用）
pub fn restore(json: &str) {
    if let Ok(cfg) = serde_json::from_str::<TtsConfig>(json) {
        set_config(cfg);
    }
}

fn load_from_store(state: &tauri::State<'_, crate::AppState>) -> TtsConfig {
    state
        .store
        .get_setting("tts_config")
        .ok()
        .flatten()
        .and_then(|json| serde_json::from_str::<TtsConfig>(&json).ok())
        .unwrap_or_default()
}

#[tauri::command]
pub fn get_tts_config(state: tauri::State<'_, crate::AppState>) -> TtsConfig {
    let cfg = load_from_store(&state);
    set_config(cfg.clone());
    cfg
}

#[tauri::command]
pub async fn set_tts_config(
    state: tauri::State<'_, crate::AppState>,
    config: TtsConfig,
) -> Result<(), String> {
    let json = serde_json::to_string(&config).map_err(|e| e.to_string())?;
    state.store.set_setting("tts_config", &json)?;
    set_config(config);
    Ok(())
}

/// 云端语音合成：按后端类型分发，音频写入临时目录，返回文件路径（前端 convertFileSrc 播放）
#[tauri::command]
pub async fn tts_synthesize(
    state: tauri::State<'_, crate::AppState>,
    text: String,
) -> Result<String, String> {
    let cfg = load_from_store(&state);
    if cfg.provider == "local" {
        return Err("当前语音后端为本地浏览器语音，无需云端合成".into());
    }
    match cfg.provider.as_str() {
        "doubao" => doubao_synthesize(&cfg, text).await,
        "kokoro" => kokoro_synthesize(&cfg, &text).await,
        _ => openai_synthesize(&state, &cfg, &text).await,
    }
}

/// 本地 Kokoro TTS 安装目录（server.py + venv，由安装脚本落盘）
const KOKORO_DIR: &str = "F:\\kokoro-tts";
/// 本地 Kokoro 默认服务地址（start_server.bat / 自动拉起均用 9800 端口）
const KOKORO_DEFAULT_BASE: &str = "http://127.0.0.1:9800/v1";

/// 确保本地 Kokoro 服务已启动：先探测健康检查，未运行则从 KOKORO_DIR 拉起并等待就绪。
/// 在 blocking 线程池执行（进程拉起 + 最长 3 分钟轮询），不阻塞异步运行时。
async fn kokoro_ensure_running(origin: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;
        let health = format!("{origin}/health");
        let up = client
            .get(&health)
            .send()
            .map(|r| r.status().is_success())
            .unwrap_or(false);
        if up {
            return Ok(());
        }
        // 未运行：从安装目录拉起本地服务（无窗口后台进程）
        let dir = std::path::Path::new(KOKORO_DIR);
        let py = dir.join("venv").join("Scripts").join("python.exe");
        if !py.exists() {
            return Err(format!(
                "本地 Kokoro 未安装（缺少 {KOKORO_DIR}\\venv）。请先完成安装，或临时切换其他语音后端"
            ));
        }
        let script = dir.join("server.py");
        let mut cmd = std::process::Command::new(&py);
        cmd.arg(&script).arg("--port").arg("9800");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        cmd.stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("启动本地 Kokoro 服务失败: {e}"))?;
        println!("[KokoroTTS] 本地服务已拉起，等待就绪（首次需加载模型）…");
        for _ in 0..90 {
            std::thread::sleep(std::time::Duration::from_millis(2000));
            let up = client
                .get(&health)
                .send()
                .map(|r| r.status().is_success())
                .unwrap_or(false);
            if up {
                println!("[KokoroTTS] 本地服务已就绪");
                return Ok(());
            }
        }
        Err(format!(
            "本地 Kokoro 服务启动超时（首次启动需下载模型约 800MB）。可先手动运行 {KOKORO_DIR}\\start_server.bat 完成预热"
        ))
    })
    .await
    .map_err(|e| format!("Kokoro 启动任务失败: {e}"))?
}

/// 本地 Kokoro 语音合成：OpenAI 兼容本地服务（v1.1-zh 中文微调版 100 中文音色），返回 wav。
/// 保存的音色名在该模型上不可用时（如旧版 zf_xiaobei 等历史音色名），自动回退默认中文女声 zf_001 重试。
async fn kokoro_synthesize(cfg: &TtsConfig, text: &str) -> Result<String, String> {
    let base = if cfg.base_url.trim().is_empty() {
        KOKORO_DEFAULT_BASE
    } else {
        cfg.base_url.trim().trim_end_matches('/')
    };
    let origin = base.strip_suffix("/v1").unwrap_or(base).to_string();
    kokoro_ensure_running(origin).await?;
    let voice = if cfg.voice.trim().is_empty() {
        "zf_001"
    } else {
        cfg.voice.trim()
    };
    let client = reqwest::Client::new();
    let synth = |v: &str| {
        client
            .post(format!("{base}/audio/speech"))
            .timeout(std::time::Duration::from_secs(120))
            .json(&serde_json::json!({
                "model": "kokoro",
                "input": text,
                "voice": v,
                "speed": 1.0,
                "response_format": "wav",
            }))
            .send()
    };
    let resp = synth(voice).await.map_err(|e| format!("请求本地 Kokoro 失败: {e}"))?;
    // 400 多为「音色不存在」：回退默认音色重试一次，保证朗读不中断
    let resp = if resp.status().as_u16() == 400 && voice != "zf_001" {
        println!("[KokoroTTS] 音色 {voice} 不可用，回退默认 zf_001");
        synth("zf_001")
            .await
            .map_err(|e| format!("请求本地 Kokoro 失败: {e}"))?
    } else {
        resp
    };
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let brief: String = body.chars().take(200).collect();
        return Err(format!("本地 Kokoro 返回 {status}: {brief}"));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("读取音频数据失败: {e}"))?;
    if bytes.is_empty() {
        return Err("本地 Kokoro 未返回音频数据".into());
    }
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(format!("data:audio/wav;base64,{b64}"))
}

/// 获取本地 Kokoro 全量音色列表（含 v1.1-zh 中文微调版 100+ 音色），供设置页动态加载
#[tauri::command]
pub async fn get_kokoro_voices() -> Result<Vec<serde_json::Value>, String> {
    let base = KOKORO_DEFAULT_BASE;
    let origin = base.strip_suffix("/v1").unwrap_or(base).to_string();
    kokoro_ensure_running(origin).await?;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/voices"))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("请求本地 Kokoro 音色列表失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("本地 Kokoro 返回 {}", resp.status()));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("解析音色列表失败: {e}"))?;
    Ok(body
        .get("voices")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default())
}

/// 豆包（火山引擎）语音合成 2.0：V3 单向流式 HTTP 接口，分块 NDJSON 中拼 base64 音频
async fn doubao_synthesize(cfg: &TtsConfig, text: String) -> Result<String, String> {
    let app_id = cfg.db_app_id.trim();
    let token = cfg.db_token.trim();
    let speaker = cfg.db_speaker.trim();
    if app_id.is_empty() || token.is_empty() || speaker.is_empty() {
        return Err("豆包语音未配置完整（需 App ID / Access Token / 音色 ID）".into());
    }
    let client = reqwest::Client::new();
    let resp = client
        .post("https://openspeech.bytedance.com/api/v3/tts/unidirectional")
        .header("X-Api-App-Id", app_id)
        .header("X-Api-Access-Key", token)
        .header("X-Api-Resource-Id", "seed-tts-2.0")
        .header("X-Api-Request-Id", uuid::Uuid::new_v4().to_string())
        .timeout(std::time::Duration::from_secs(60))
        .json(&serde_json::json!({
            "user": { "uid": "baize" },
            "req_params": {
                "text": text,
                "speaker": speaker,
                "audio_params": {
                    "format": "mp3",
                    "sample_rate": 24000,
                    "speech_rate": cfg.db_speech_rate,
                },
            },
        }))
        .send()
        .await
        .map_err(|e| format!("请求豆包语音失败: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let brief: String = body.chars().take(200).collect();
        // 高频错误 → 可操作指引（403 resource not granted = 应用未开通语音合成 2.0 服务）
        if brief.contains("resource not granted") || brief.contains("45000030") {
            return Err(
                "豆包语音返回 403：该 App ID 未被授予「语音合成 2.0（seedtts）」服务。\
                 请到火山引擎控制台 → 豆包语音 → 语音合成大模型(2.0)：① 开通服务（免费额度需领取/0元下单）；\
                 ② 在应用管理里确认该 App ID 已勾选/绑定「语音合成 2.0」能力；③ 确认 Access Token 填的是该应用详情页的 Token（不是 IAM 密钥）"
                    .into(),
            );
        }
        return Err(format!("豆包语音返回 {status}: {brief}"));
    }
    // 响应为逐行 JSON（NDJSON），音频 base64 藏在 data/audio 字段里，可能分多块
    let body = resp.text().await.map_err(|e| format!("读取豆包响应失败: {e}"))?;
    println!(
        "[豆包TTS] 响应长度 {}，前 300 字符: {}",
        body.len(),
        body.chars().take(300).collect::<String>()
    );
    let mut b64 = String::new();
    let mut err_msg = String::new();
    let mut chunks = 0usize;
    for line in body.lines() {
        let line = line.trim();
        let line = line.strip_prefix("data:").map(str::trim).unwrap_or(line);
        if line.is_empty() || line == "[DONE]" {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(code) = v.get("code").and_then(|c| c.as_i64()) {
            if code != 0 {
                err_msg = v
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("未知错误")
                    .to_string();
            }
        }
        let before = b64.len();
        collect_audio_b64(&v, &mut b64);
        if b64.len() > before {
            chunks += 1;
        }
    }
    println!("[豆包TTS] 音频块 {chunks} 个，base64 共 {} 字符", b64.len());
    if b64.is_empty() {
        return Err(if err_msg.is_empty() {
            "豆包语音未返回音频数据".into()
        } else {
            format!("豆包语音错误: {err_msg}")
        });
    }
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.as_bytes())
        .map_err(|e| format!("音频 base64 解码失败: {e}"))?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("baize-tts-{ts}.mp3"));
    let _ = std::fs::write(&path, &bytes); // 落盘仅供排查，播放走 data URL
    println!("[豆包TTS] 合成成功 {} 字节 → data URL 播放", bytes.len());
    Ok(format!("data:audio/mpeg;base64,{b64}"))
}

/// 递归收集 JSON 中 audio/data 字段里的 base64 音频块
fn collect_audio_b64(v: &serde_json::Value, out: &mut String) {
    match v {
        serde_json::Value::Object(m) => {
            for (k, val) in m {
                if (k == "audio" || k == "data") && val.is_string() {
                    if let Some(s) = val.as_str() {
                        if s.len() > 16 {
                            out.push_str(s);
                        }
                    }
                }
                collect_audio_b64(val, out);
            }
        }
        serde_json::Value::Array(a) => {
            for x in a {
                collect_audio_b64(x, out);
            }
        }
        _ => {}
    }
}

/// OpenAI 兼容 /audio/speech 合成
async fn openai_synthesize(
    state: &tauri::State<'_, crate::AppState>,
    cfg: &TtsConfig,
    text: &str,
) -> Result<String, String> {
    let _ = state;
    if cfg.base_url.trim().is_empty() || cfg.api_key.trim().is_empty() {
        return Err("云端语音模型未配置完整（需 base_url 与 api_key）".into());
    }
    let base = cfg.base_url.trim().trim_end_matches('/');
    let url = format!("{base}/audio/speech");
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .bearer_auth(cfg.api_key.trim())
        .timeout(std::time::Duration::from_secs(30))
        .json(&serde_json::json!({
            "model": cfg.model,
            "input": text,
            "voice": cfg.voice,
            "response_format": "mp3",
        }))
        .send()
        .await
        .map_err(|e| format!("请求语音模型失败: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let brief: String = body.chars().take(200).collect();
        return Err(format!("语音模型返回 {status}: {brief}"));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("读取音频数据失败: {e}"))?;
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(format!("data:audio/mpeg;base64,{b64}"))
}
