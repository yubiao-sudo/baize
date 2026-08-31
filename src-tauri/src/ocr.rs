//! 本地 OCR：调用 Tesseract CLI 提取图片文字，返回全文与词级坐标。

use serde_json::{json, Value};

use crate::tools::{resolve_path, PermissionClass, Tool};

pub struct OcrImageTool;

impl Tool for OcrImageTool {
    fn name(&self) -> &str {
        "ocr_image"
    }
    fn description(&self) -> &str {
        "对本地图片做 OCR 文字提取（Tesseract），返回全文与词级坐标（x/y/w/h/conf），可与 capture_screen 截图链式使用"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "图片路径（绝对路径或相对工作空间的相对路径）" },
                "lang": { "type": "string", "description": "识别语言，默认 chi_sim+eng（中英）" }
            },
            "required": ["path"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let path = resolve_path(args["path"].as_str().ok_or("缺少参数 path")?);
        let lang = args["lang"].as_str().unwrap_or("chi_sim+eng");
        let (stdout, stderr) = run_tesseract(&path, lang)?;
        let (text, words) = parse_tsv(&stdout);

        if text.is_empty() {
            if !stderr.is_empty() {
                return Err(format!("OCR 失败或未识别到文字：{stderr}"));
            }
            return Ok(json!({ "path": path, "lang": lang, "text": "", "words": [] }));
        }

        Ok(json!({
            "path": path,
            "lang": lang,
            "text": text,
            "words": words,
        }))
    }
}

/// 对图片做 OCR，仅返回提取出的全文（供附件图片解析等内部复用）
pub fn ocr_text(path: &str, lang: &str) -> Result<String, String> {
    let (stdout, stderr) = run_tesseract(path, lang)?;
    let (text, _words) = parse_tsv(&stdout);
    if text.is_empty() && !stderr.is_empty() {
        return Err(format!("OCR 失败或未识别到文字：{stderr}"));
    }
    Ok(text)
}

/// 对图片做 OCR，返回 (全文, 词级坐标框)。词框含 text/x/y/w/h/conf，供 GUI 自动化文字定位复用。
pub fn ocr_detect(path: &str, lang: &str) -> Result<(String, Vec<Value>), String> {
    let (stdout, stderr) = run_tesseract(path, lang)?;
    let (text, words) = parse_tsv(&stdout);
    if text.is_empty() && !stderr.is_empty() {
        return Err(format!("OCR 失败或未识别到文字：{stderr}"));
    }
    Ok((text, words))
}

/// GUI 自动化专用 OCR：速度与准确率双优的双引擎策略。
///
/// 一级：Windows.Media.Ocr 系统引擎（Win10/11 自带）——全屏约 0.2~0.5s，
///       中文质量比 Tesseract 好一档（GUI 任务耗时的最大单点优化）；
/// 二级：Tesseract best 包（chi_sim+eng），喂入前做预处理（灰度 + 暗色主题自动反色），
///       解决小字/暗底识别率问题。系统引擎不可用（精简版系统/语言包缺失）时才走此路。
pub fn ocr_detect_gui(path: &str) -> Result<(String, Vec<Value>), String> {
    let t0 = std::time::Instant::now();
    match windows_ocr(path) {
        Ok((text, words)) => {
            println!(
                "[OCR] Windows 引擎 {} 词，耗时 {}ms",
                words.len(),
                t0.elapsed().as_millis()
            );
            Ok((text, words))
        }
        Err(e) => {
            println!("[OCR] Windows 引擎不可用（{e}），回退预处理版 Tesseract");
            // 预处理：灰度 + 暗色自动反色（Tesseract 偏好深字浅底）；失败则用原图
            let pre = preprocess_for_tesseract(path).unwrap_or_else(|_| path.to_string());
            let r = ocr_detect(&pre, "chi_sim+eng");
            if pre != path {
                let _ = std::fs::remove_file(&pre);
            }
            println!("[OCR] Tesseract 总耗时 {}ms", t0.elapsed().as_millis());
            r
        }
    }
}

/// Windows.Media.Ocr 系统引擎：PNG 路径 → (全文, 词级坐标框)。
/// 词框坐标与 Tesseract 同为图片物理像素空间，调用方的显示器偏移逻辑不变。
fn windows_ocr(path: &str) -> Result<(String, Vec<Value>), String> {
    use windows::Globalization::Language;
    use windows::Graphics::Imaging::BitmapDecoder;
    use windows::Media::Ocr::OcrEngine;
    use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};

    // 1) 引擎：优先中文语言包，退回用户语言列表
    let zh = Language::CreateLanguage(&windows::core::HSTRING::from("zh-CN"))
        .map_err(|e| format!("创建语言对象失败: {e}"))?;
    let engine = match OcrEngine::TryCreateFromLanguage(&zh) {
        Ok(e) => Some(e),
        Err(_) => OcrEngine::TryCreateFromUserProfileLanguages().ok(),
    };
    let engine =
        engine.ok_or_else(|| "系统无可用 OCR 语言包（需在系统设置中安装中文语言的可选功能）".to_string())?;

    // 2) PNG 字节 → 内存流
    let bytes = std::fs::read(path).map_err(|e| format!("读取截图失败: {e}"))?;
    let stream = InMemoryRandomAccessStream::new().map_err(|e| e.to_string())?;
    let writer =
        DataWriter::CreateDataWriter(&stream).map_err(|e| e.to_string())?;
    writer.WriteBytes(&bytes).map_err(|e| e.to_string())?;
    writer.StoreAsync().map_err(|e| e.to_string())?.get().map_err(|e| e.to_string())?;
    writer.FlushAsync().map_err(|e| e.to_string())?.get().map_err(|e| e.to_string())?;
    // 关键：Detach 解绑底层流。直接 drop DataWriter 会连带关闭流，
    // 后续 BitmapDecoder 读取时报「该对象已关闭 (0x80000013)」
    writer.DetachStream().map_err(|e| e.to_string())?;
    stream.Seek(0).map_err(|e| e.to_string())?;

    // 3) 解码为 SoftwareBitmap（系统引擎只吃这个）
    let decoder =
        BitmapDecoder::CreateAsync(&stream).map_err(|e| e.to_string())?.get().map_err(|e| e.to_string())?;
    let bitmap = decoder
        .GetSoftwareBitmapAsync()
        .map_err(|e| e.to_string())?
        .get()
        .map_err(|e| e.to_string())?;
    // 系统引擎有最大边长限制（典型 2600px），超限回退 Tesseract（避免缩放引入坐标换算误差）
    let max_dim = OcrEngine::MaxImageDimension().map_err(|e| e.to_string())? as i64;
    if bitmap.PixelWidth().map_err(|e| e.to_string())? as i64 > max_dim
        || bitmap.PixelHeight().map_err(|e| e.to_string())? as i64 > max_dim
    {
        return Err("图片超出系统引擎最大边长限制".to_string());
    }

    // 4) 识别 → 词级框（坐标即图片像素空间）
    let result = engine
        .RecognizeAsync(&bitmap)
        .map_err(|e| e.to_string())?
        .get()
        .map_err(|e| e.to_string())?;
    let lines = result.Lines().map_err(|e| e.to_string())?;
    let mut words = Vec::new();
    let mut parts: Vec<String> = Vec::new();
    for line in lines {
        let line_text = line.Text().map_err(|e| e.to_string())?.to_string();
        parts.push(line_text);
        for w in line.Words().map_err(|e| e.to_string())? {
            let rect = w.BoundingRect().map_err(|e| e.to_string())?;
            words.push(json!({
                "text": w.Text().map_err(|e| e.to_string())?.to_string(),
                "x": rect.X as f64,
                "y": rect.Y as f64,
                "w": rect.Width as f64,
                "h": rect.Height as f64,
                "conf": 100,
            }));
        }
    }
    Ok((parts.join("\n"), words))
}

/// Tesseract 预处理：灰度 + 暗色主题自动反色（均值亮度 < 110 时），
/// 解决「浅字深底」场景下 Tesseract 识别率骤降的问题。返回临时文件路径。
fn preprocess_for_tesseract(src: &str) -> Result<String, String> {
    let img = image::open(src).map_err(|e| format!("打开截图失败: {e}"))?;
    let gray = img.to_luma8();
    let (w, h) = gray.dimensions();
    let total: u64 = gray.pixels().map(|p| p[0] as u64).sum();
    let mean = (total / (w as u64 * h as u64).max(1)) as u8;
    let mut out = gray;
    if mean < 110 {
        image::imageops::invert(&mut out);
    }
    let tmp = std::env::temp_dir().join(format!(
        "baize-ocr-pre-{}.png",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));
    image::DynamicImage::ImageLuma8(out)
        .save_with_format(&tmp, image::ImageFormat::Png)
        .map_err(|e| format!("写入预处理图失败: {e}"))?;
    Ok(tmp.to_string_lossy().to_string())
}

#[cfg(test)]
mod ocr_tests {
    use super::*;

    /// 用工作目录里最近的真实截屏验证双引擎：Windows 引擎应成功且显著快于 Tesseract。
    /// 无历史截屏时跳过（CI 环境）。
    #[test]
    fn gui_ocr_real_screenshot() {
        let dir = std::env::current_dir().unwrap();
        let shot = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                n.starts_with("baize-screenshot-") && n.ends_with(".png")
            })
            .max_by_key(|e| e.metadata().unwrap().modified().unwrap());
        let Some(shot) = shot else {
            println!("无历史截屏，跳过");
            return;
        };
        let path = shot.path().to_string_lossy().to_string();

        // Windows 引擎
        let t0 = std::time::Instant::now();
        let mut win_ok = false;
        match windows_ocr(&path) {
            Ok((text, words)) => {
                win_ok = true;
                println!(
                    "[Windows 引擎] {}ms，{} 词，全文前 120 字：{}",
                    t0.elapsed().as_millis(),
                    words.len(),
                    text.chars().take(120).collect::<String>()
                );
                assert!(!words.is_empty(), "Windows 引擎返回空词表");
                assert!(t0.elapsed().as_millis() < 5000, "Windows 引擎耗时异常");
            }
            Err(e) => println!("[Windows 引擎] 不可用（{e}），将走 Tesseract 预处理路径"),
        }

        // Tesseract 原图 vs 预处理后（对照组）
        let t1 = std::time::Instant::now();
        let r2 = ocr_detect(&path, "chi_sim+eng");
        println!(
            "[Tesseract 原图] {}ms，词数 {}",
            t1.elapsed().as_millis(),
            r2.as_ref().map(|(_, w)| w.len()).unwrap_or(0)
        );
        let t2 = std::time::Instant::now();
        let pre = preprocess_for_tesseract(&path).unwrap();
        let r3 = ocr_detect(&pre, "chi_sim+eng");
        println!(
            "[Tesseract 预处理后] {}ms，词数 {}",
            t2.elapsed().as_millis(),
            r3.as_ref().map(|(_, w)| w.len()).unwrap_or(0)
        );
        let _ = std::fs::remove_file(&pre);
        assert!(win_ok || r2.is_ok() || r3.is_ok(), "双引擎全部失败");
    }
}

fn run_tesseract(path: &str, lang: &str) -> Result<(String, String), String> {
    let output = crate::tools::silent_command("tesseract")
        .arg(path)
        .arg("stdout")
        .args(["-l", lang])
        .arg("tsv")
        .output()
        .map_err(|e| {
            format!(
                "未检测到 Tesseract，请先安装 tesseract 并加入 PATH（同时安装语言包 chi_sim、eng）: {e}"
            )
        })?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    Ok((stdout, stderr))
}

/// 解析 tesseract TSV 输出（首行表头，列：level page block par line word left top width height conf text）
fn parse_tsv(stdout: &str) -> (String, Vec<Value>) {
    let mut words = Vec::new();
    let mut parts: Vec<String> = Vec::new();
    let mut word_rows = 0usize;

    for line in stdout.lines().skip(1) {
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 12 {
            continue;
        }
        let level = cols[0].trim();
        let text = cols[11].trim();
        if text.is_empty() {
            continue;
        }
        let entry = json!({
            "text": text,
            "x": parse_int(cols[6]),
            "y": parse_int(cols[7]),
            "w": parse_int(cols[8]),
            "h": parse_int(cols[9]),
            "conf": cols[10].trim(),
        });
        if level == "5" {
            word_rows += 1;
            words.push(entry);
            parts.push(text.to_string());
        }
    }

    // 无词级结果时退化：采集任意非空文本行
    if word_rows == 0 {
        words.clear();
        parts.clear();
        for line in stdout.lines().skip(1) {
            let cols: Vec<&str> = line.split('\t').collect();
            if cols.len() < 12 {
                continue;
            }
            let text = cols[11].trim();
            if text.is_empty() {
                continue;
            }
            words.push(json!({
                "text": text,
                "x": parse_int(cols[6]),
                "y": parse_int(cols[7]),
                "w": parse_int(cols[8]),
                "h": parse_int(cols[9]),
                "conf": cols[10].trim(),
            }));
            parts.push(text.to_string());
        }
    }

    (parts.join(" "), words)
}

fn parse_int(s: &str) -> i32 {
    s.trim().parse::<i32>().unwrap_or(0)
}