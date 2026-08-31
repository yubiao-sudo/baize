//! 多模态交互（Multi-modal Interaction）
//!
//! 对应《白泽自主进化》功能四：从「键盘鼠标+文本」扩展为「语音 + 视觉 + 视频」全模态。
//!
//! 能力：
//!   - image_describe  : 本地图片 → 视觉模型描述（复用视觉 grounding 通道）
//!   - screen_understand: 截屏 → 本地 OCR 文字 + 视觉模型场景描述
//!   - video_analyze   : 视频抽帧（ffmpeg）→ 关键帧逐帧描述 → 内容概要
//!   - stt_transcribe : 语音转写（本地 Whisper CLI，未安装时给出清晰指引）
//!   - 语音输出 TTS     : 复用 notify::SpeakTool（speak）
//!
//! 全部「本地优先」：视觉模型走 Ollama，OCR 走 Tesseract，抽帧走 ffmpeg，转写走 Whisper。

use std::sync::Arc;

use serde_json::{json, Value};

use crate::capability::Capability;
use crate::tools::{PermissionClass, Tool, resolve_path};

// ───────────────── image_describe ─────────────────

/// 描述本地图片内容（视觉模型）
pub struct ImageDescribeTool;

impl Tool for ImageDescribeTool {
    fn name(&self) -> &str {
        "image_describe"
    }
    fn description(&self) -> &str {
        "用本地视觉模型描述一张图片的内容（主体、场景、图中文字）。path 为图片路径，hint 可选背景提示（如「这是一张产品宣传图」），帮助模型更准确地理解"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "图片绝对路径" },
                "hint": { "type": "string", "description": "可选：背景提示" }
            },
            "required": ["path"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let path = resolve_path(args["path"].as_str().ok_or("缺少参数 path")?);
        let hint = args["hint"].as_str().unwrap_or("");
        let desc = crate::visual_grounding::describe_image(&path, hint)?;
        Ok(json!({ "path": path, "description": desc }))
    }
}

// ───────────────── screen_understand ─────────────────

/// 屏幕理解：截屏 + OCR + 视觉描述
pub struct ScreenUnderstandTool {
    capability: Arc<dyn Capability>,
}

impl ScreenUnderstandTool {
    pub fn new(capability: Arc<dyn Capability>) -> Self {
        Self { capability }
    }
}

impl Tool for ScreenUnderstandTool {
    fn name(&self) -> &str {
        "screen_understand"
    }
    fn description(&self) -> &str {
        "理解当前屏幕：截取当前屏幕，同时做本地 OCR 文字提取（Tesseract）与视觉模型场景描述（Ollama），返回「画面里有什么字 + 什么内容」。用于「帮我看下这个界面」「屏幕现在是什么状态」等"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "lang": { "type": "string", "description": "OCR 语言，默认 chi_sim+eng" },
                "question": { "type": "string", "description": "可选：针对屏幕内容的具体问题（如「这个窗口里有没有登录按钮」）" }
            }
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let info = self.capability.capture_screen().map_err(|e| e.to_string())?;
        let path = info.path.clone();
        let lang = args["lang"].as_str().unwrap_or("chi_sim+eng");

        let mut out = json!({
            "path": path,
            "width": info.width,
            "height": info.height,
        });

        // 1) OCR 文字
        match crate::ocr::ocr_detect(&path, lang) {
            Ok((text, _words)) => {
                out["ocr_text"] = Value::String(text);
            }
            Err(e) => {
                out["ocr_error"] = Value::String(e);
            }
        }

        // 2) 视觉描述
        let hint = args["question"].as_str().unwrap_or("");
        match crate::visual_grounding::describe_image(&path, hint) {
            Ok(desc) => {
                out["vision"] = Value::String(desc);
            }
            Err(e) => {
                out["vision_error"] = Value::String(e);
            }
        }

        Ok(out)
    }
}

// ───────────────── video_analyze ─────────────────

/// 视频内容分析：抽帧 → 视觉描述 → 概要
pub struct VideoAnalyzeTool;

impl Tool for VideoAnalyzeTool {
    fn name(&self) -> &str {
        "video_analyze"
    }
    fn description(&self) -> &str {
        "分析视频内容：用 ffmpeg 抽取 N 帧关键画面，逐帧用本地视觉模型描述，返回「关键画面描述 + 粗略内容概要」。用于「看下这个视频讲了啥」。frames 为抽取帧数（默认 8，建议 5~20）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "视频文件绝对路径" },
                "frames": { "type": "integer", "description": "抽取帧数，默认 8（建议 5~20）" },
                "question": { "type": "string", "description": "可选：针对视频内容的特定问题（如「视频里出现了哪些人物」）" }
            },
            "required": ["path"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let path = resolve_path(args["path"].as_str().ok_or("缺少参数 path")?);
        let count = args["frames"].as_u64().unwrap_or(8).clamp(1, 60) as usize;
        let hint = args["question"].as_str().unwrap_or("");

        // 1) 抽帧
        let frames = extract_frames(&path, count)?;
        if frames.is_empty() {
            return Err("未能从视频中抽取出任何帧画面".to_string());
        }

        // 2) 逐帧描述
        let frame_hint = if hint.trim().is_empty() {
            "这是从视频里抽取的一帧关键画面，请描述画面里的人、场景、动作和出现的文字。".to_string()
        } else {
            format!("这是从视频里抽取的一帧关键画面，用户的问题：{hint}。请结合此画面回答。")
        };
        let mut descriptions = Vec::new();
        for f in &frames {
            match crate::visual_grounding::describe_image(f, &frame_hint) {
                Ok(d) => descriptions.push(json!({ "frame": f, "description": d })),
                Err(e) => descriptions.push(json!({ "frame": f, "error": e })),
            }
        }

        Ok(json!({
            "path": path,
            "extracted_frames": frames.len(),
            "frames": descriptions,
        }))
    }
}

/// 用 ffmpeg 从视频里均匀抽取约 `count` 帧，返回帧图片路径列表
fn extract_frames(video_path: &str, count: usize) -> Result<Vec<String>, String> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let out_dir = std::env::temp_dir().join(format!("baize_video_{ts}"));
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("创建抽帧目录失败: {e}"))?;

    let dir_str = out_dir.to_string_lossy().to_string();

    // 先用 ffprobe 拿时长，从而算出合适的 fps（约 count 帧）；拿不到则回退到固定 1 帧/5 秒
    let fps = match video_duration(video_path) {
        Some(secs) if secs > 0.0 => (count as f64 / secs).max(0.05),
        _ => 0.2,
    };

    let pattern = format!("{dir}/frame_%04d.jpg", dir = dir_str);
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            video_path,
            "-vf",
            &format!("fps={fps}"),
            "-frames:v",
            &count.to_string(),
            &pattern,
        ])
        .status()
        .map_err(|e| {
            format!("未检测到 ffmpeg，请先安装并加入 PATH（用于视频抽帧）: {e}")
        })?;

    if !status.success() {
        return Err("ffmpeg 抽帧执行失败".to_string());
    }

    let mut frames: Vec<String> = std::fs::read_dir(&out_dir)
        .map_err(|e| format!("读取抽帧结果失败: {e}"))?
        .filter_map(|e| e.ok())
        .map(|e| e.path().to_string_lossy().to_string())
        .collect();
    frames.sort();
    Ok(frames)
}

/// 用 ffprobe 获取视频时长（秒）；失败返回 None
fn video_duration(path: &str) -> Option<f64> {
    let out = crate::tools::silent_command("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
            path,
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse::<f64>().ok()
}

// ───────────────── stt_transcribe ─────────────────

/// 语音转写（本地 Whisper）
pub struct SttTranscribeTool;

impl Tool for SttTranscribeTool {
    fn name(&self) -> &str {
        "stt_transcribe"
    }
    fn description(&self) -> &str {
        "把本地音频文件（mp3/wav/m4a/flac 等）转写为文字。使用本地 Whisper CLI（whisper / whisper-cpp）。lang 为语言代码（默认 zh），未安装 Whisper 时会返回清晰的安装指引。配合语音输入场景使用"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "音频文件绝对路径" },
                "lang": { "type": "string", "description": "语言代码，默认 zh" },
                "model": { "type": "string", "description": "Whisper 模型名（tiny/base/small/medium），默认 base" }
            },
            "required": ["path"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let path = resolve_path(args["path"].as_str().ok_or("缺少参数 path")?);
        let lang = args["lang"].as_str().unwrap_or("zh");
        let model = args["model"].as_str().unwrap_or("base");

        // Whisper 可执行文件探测：优先 whisper，其次 whisper-cpp，再 whisper-cli
        let mut tool = "whisper";
        let mut ok = std::process::Command::new("whisper")
            .args(["--help"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !ok {
            tool = "whisper-cpp";
            ok = std::process::Command::new("whisper-cpp")
                .args(["--help"])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if !ok {
                tool = "whisper-cli";
                ok = std::process::Command::new("whisper-cli")
                    .args(["--help"])
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false);
            }
        }
        if !ok {
            return Err(
                "未检测到本地 Whisper。请安装其一：\n\
                 1) faster-whisper：pip install faster-whisper（推荐，快且省内存）；\n\
                 2) openai-whisper：pip install openai-whisper；\n\
                 3) whisper.cpp：编译后把 whisper-cli 加入 PATH。\n\
                 安装后即可本地语音转写，无需联网。"
                    .to_string(),
            );
        }

        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let out_dir = std::env::temp_dir().join(format!("baize_stt_{ts}"));
        std::fs::create_dir_all(&out_dir).map_err(|e| format!("创建转写目录失败: {e}"))?;
        let out_dir_str = out_dir.to_string_lossy().to_string();

        let status = crate::tools::silent_command(tool)
            .args([
                path.as_str(),
                "--language",
                lang,
                "--model",
                model,
                "--output_format",
                "txt",
                "--output_dir",
                &out_dir_str,
                "--fp16",
                "False",
            ])
            .status()
            .map_err(|e| format!("启动 {tool} 失败: {e}"))?;

        if !status.success() {
            return Err(format!("{tool} 转写执行失败"));
        }

        // 读取生成的 .txt（文件名与源音频同名）
        let stem = std::path::Path::new(&path)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let txt_path = out_dir.join(format!("{stem}.txt"));
        let text = std::fs::read_to_string(&txt_path)
            .map_err(|e| format!("读取转写结果失败（{}）: {e}", txt_path.display()))?;

        Ok(json!({ "path": path, "text": text, "engine": tool, "model": model }))
    }
}