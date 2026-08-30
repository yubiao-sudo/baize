//! 麦克风听写与会议纪要：实时录音 + 本地转写。
//!
//! 复用本地调用栈（全部离线、无需联网）：
//!   - ffmpeg（dshow 音频采集）把系统默认麦克风录成 WAV；
//!   - 本地 Whisper CLI（whisper / whisper-cpp / whisper-cli）把 WAV 转成文字。
//!
//! 工具：
//!   - [`MicRecordTool`]（`mic_record`）：录音 N 秒 → 可选立即本地转写，返回 WAV 路径与文字。
//!   转写的文字可继续交给 Agent 用 markdown_set 整理成会议纪要。

use std::process::Command;

use serde_json::{json, Value};

use crate::tools::{PermissionClass, Tool, resolve_path};

/// 探测系统默认麦克风名称（ffmpeg dshow 设备列表），失败返回 None
fn discover_mic_name() -> Option<String> {
    let out = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-list_devices",
            "true",
            "-f",
            "dshow",
            "-i",
            "dummy",
        ])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stderr).to_string();
    let mut in_audio = false;
    for line in text.lines() {
        if line.contains("DirectShow audio devices") {
            in_audio = true;
            continue;
        }
        if !in_audio {
            continue;
        }
        if line.contains("DirectShow video devices") || line.contains("Alternative name") {
            break;
        }
        if let (Some(a), Some(b)) = (line.find('"'), line.rfind('"')) {
            if b > a {
                return Some(line[a + 1..b].to_string());
            }
        }
    }
    None
}

/// 用 ffmpeg dshow 把默认麦克风录成单声道 16k WAV
fn record(duration_secs: u64, mic: &str, out: &str) -> Result<(), String> {
    let status = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "dshow",
            "-i",
            &format!("audio={mic}"),
            "-t",
            &duration_secs.to_string(),
            "-ac",
            "1",
            "-ar",
            "16000",
            "-y",
            out,
        ])
        .status()
        .map_err(|e| format!("未检测到 ffmpeg，请先安装并加入 PATH（用于麦克风录音）: {e}"))?;
    if !status.success() {
        return Err("ffmpeg 录音失败（可能麦克风被占用或无权限访问）".to_string());
    }
    Ok(())
}

/// 本地转写：探测 whisper/whisper-cpp/whisper-cli 并转写，返回文字
fn transcribe(path: &str, lang: &str, model: &str) -> Result<String, String> {
    let mut tool = "whisper";
    let mut ok = Command::new("whisper")
        .args(["--help"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        tool = "whisper-cpp";
        ok = Command::new("whisper-cpp")
            .args(["--help"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !ok {
            tool = "whisper-cli";
            ok = Command::new("whisper-cli")
                .args(["--help"])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
        }
    }
    if !ok {
        return Err(
            "未检测到本地 Whisper。请安装其一：\n\
             1) faster-whisper：pip install faster-whisper（推荐）；\n\
             2) openai-whisper：pip install openai-whisper；\n\
             3) whisper.cpp：编译后把 whisper-cli 加入 PATH。\n\
             安装后即可本地语音转写。"
                .to_string(),
        );
    }

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let out_dir = std::env::temp_dir().join(format!("baize_mic_stt_{ts}"));
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("创建转写目录失败: {e}"))?;
    let out_dir_str = out_dir.to_string_lossy().to_string();

    let status = Command::new(tool)
        .args([
            path,
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

    let stem = std::path::Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let txt_path = out_dir.join(format!("{stem}.txt"));
    std::fs::read_to_string(&txt_path)
        .map_err(|e| format!("读取转写结果失败（{}）: {e}", txt_path.display()))
}

/// 麦克风录音工具（实时本地采集 + 可选本地转写）
pub struct MicRecordTool;

impl Tool for MicRecordTool {
    fn name(&self) -> &str {
        "mic_record"
    }
    fn description(&self) -> &str {
        "从系统默认麦克风实时录音并保存为 WAV；默认同时用本地 Whisper 转写为文字。\
         用于「帮我记录这段发言」「录个会议」「语音速记」等。duration 为录音秒数（默认 30，会议纪要建议 120~180）。\
         转写的文字可继续用 markdown_set 整理成会议纪要。需 ffmpeg（录音）与本地 Whisper（转写）。"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "duration": { "type": "integer", "description": "录音秒数，默认 30" },
                "transcribe": { "type": "boolean", "description": "录音后是否立即本地转写，默认 true" },
                "lang": { "type": "string", "description": "转写语言，默认 zh" },
                "model": { "type": "string", "description": "Whisper 模型（tiny/base/small/medium），默认 base" },
                "output": { "type": "string", "description": "可选：WAV 文件输出路径（默认写入系统临时目录）" }
            }
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let duration = args["duration"].as_u64().unwrap_or(30).clamp(1, 3600);
        let transcribe_flag = args["transcribe"].as_bool().unwrap_or(true);
        let lang = args["lang"].as_str().unwrap_or("zh");
        let model = args["model"].as_str().unwrap_or("base");

        let out_path = match args["output"].as_str() {
            Some(p) => resolve_path(p),
            None => {
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                std::env::temp_dir()
                    .join(format!("baize_mic_{ts}.wav"))
                    .to_string_lossy()
                    .to_string()
            }
        };

        let mic = discover_mic_name().ok_or_else(|| {
            "未找到可用麦克风设备。请确认已连接麦克风，且 ffmpeg 已安装并加入 PATH（用于 dshow 录音）"
                .to_string()
        })?;

        record(duration, &mic, &out_path)?;

        let mut out = json!({ "ok": true, "path": out_path, "duration_secs": duration, "mic": mic });

        if transcribe_flag {
            match transcribe(&out_path, lang, model) {
                Ok(text) => {
                    out["text"] = Value::String(text);
                    out["engine"] = Value::String(model.to_string());
                }
                Err(e) => {
                    out["transcribe_error"] = Value::String(e);
                }
            }
        }

        Ok(out)
    }
}