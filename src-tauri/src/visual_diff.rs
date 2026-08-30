//! 视觉回归对比 —— 基线截图 vs 当前截图 的像素级 diff
//!
//! 用途：UI 改版回归检查。把「改版前截图（基线）」与「改版后截图」逐像素比对，产出：
//! 1. 差异像素数与占比（pass/fail 判定，默认阈值 2%）
//! 2. 差异区域包围盒列表（8px 网格热区 + 4-连通聚类，过滤抗锯齿噪点）
//! 3. 红色高亮 diff 图（差异像素盖半透明红 + 包围盒描红框），一眼定位改版处
//!
//! 两图尺寸不同时自动把当前图缩放到基线尺寸（三角形滤波）后比对，输出 resized 标记。
//! 同时实现为 agent 工具（VisualDiffTool）：current 省略时现场截屏，白泽可自主执行
//! 「截基线 → 改版 → 截当前 → 对比」的完整回归流程。

use std::path::Path;
use std::sync::Arc;

use image::{GenericImageView, RgbaImage};
use serde_json::{json, Value};

use crate::capability::Capability;
use crate::tools::{PermissionClass, Tool};

/// 逐像素比对：任一通道（含 alpha）差值 > tolerance 视为差异像素。
/// 返回（差异像素数，按行优先的差异掩码）
fn diff_mask(baseline: &RgbaImage, current: &RgbaImage, tolerance: u8) -> (usize, Vec<bool>) {
    let mut mask = vec![false; (baseline.width() * baseline.height()) as usize];
    let mut count = 0usize;
    for (i, (b, c)) in baseline.pixels().zip(current.pixels()).enumerate() {
        if b.0
            .iter()
            .zip(c.0.iter())
            .any(|(x, y)| x.abs_diff(*y) > tolerance)
        {
            mask[i] = true;
            count += 1;
        }
    }
    (count, mask)
}

/// 差异区域聚类：cell×cell 网格，热格 = 格内差异像素占比 > cell_ratio；
/// 热格 4-连通 BFS 合并成包围盒；过滤 min_size 以下的小区域（抗锯齿噪点）。
/// 返回 (x1, y1, x2, y2) 像素坐标（含边界），按面积降序，最多 50 个。
fn diff_regions(
    mask: &[bool],
    w: u32,
    h: u32,
    cell: u32,
    cell_ratio: f64,
    min_size: u32,
) -> Vec<(u32, u32, u32, u32)> {
    let gw = (w + cell - 1) / cell;
    let gh = (h + cell - 1) / cell;
    let hot_threshold = ((cell * cell) as f64 * cell_ratio).ceil() as usize;

    let mut hot = vec![false; (gw * gh) as usize];
    for gy in 0..gh {
        for gx in 0..gw {
            let mut n = 0usize;
            for y in (gy * cell)..((gy + 1) * cell).min(h) {
                for x in (gx * cell)..((gx + 1) * cell).min(w) {
                    if mask[(y * w + x) as usize] {
                        n += 1;
                    }
                }
            }
            if n >= hot_threshold {
                hot[(gy * gw + gx) as usize] = true;
            }
        }
    }

    let mut visited = vec![false; hot.len()];
    let mut regions = Vec::new();
    for start in 0..hot.len() {
        if !hot[start] || visited[start] {
            continue;
        }
        // BFS 合并连通热格
        let mut queue = vec![start];
        visited[start] = true;
        let (mut min_gx, mut min_gy, mut max_gx, mut max_gy) = {
            let gx = (start % gw as usize) as u32;
            let gy = (start / gw as usize) as u32;
            (gx, gy, gx, gy)
        };
        while let Some(idx) = queue.pop() {
            let gx = (idx % gw as usize) as u32;
            let gy = (idx / gw as usize) as u32;
            min_gx = min_gx.min(gx);
            min_gy = min_gy.min(gy);
            max_gx = max_gx.max(gx);
            max_gy = max_gy.max(gy);
            let mut neighbors: Vec<(u32, u32)> = Vec::with_capacity(4);
            if let Some(x) = gx.checked_sub(1) {
                neighbors.push((x, gy));
            }
            if gx + 1 < gw {
                neighbors.push((gx + 1, gy));
            }
            if let Some(y) = gy.checked_sub(1) {
                neighbors.push((gx, y));
            }
            if gy + 1 < gh {
                neighbors.push((gx, gy + 1));
            }
            for nb in neighbors {
                let nidx = (nb.1 * gw + nb.0) as usize;
                if hot[nidx] && !visited[nidx] {
                    visited[nidx] = true;
                    queue.push(nidx);
                }
            }
        }
        let (x1, y1) = (min_gx * cell, min_gy * cell);
        let (x2, y2) = ((max_gx + 1) * cell - 1, (max_gy + 1) * cell - 1);
        // 裁剪到图像边界
        let (x2, y2) = (x2.min(w - 1), y2.min(h - 1));
        if x2 - x1 + 1 >= min_size && y2 - y1 + 1 >= min_size {
            regions.push((x1, y1, x2, y2));
        }
    }
    regions.sort_by(|a, b| {
        let area = |r: &(u32, u32, u32, u32)| (r.2 - r.0 + 1) * (r.3 - r.1 + 1);
        area(b).cmp(&area(a))
    });
    regions.truncate(50);
    regions
}

/// 生成差异高亮图：当前截图为底，差异像素盖半透明红，包围盒描 2px 红框
fn render_diff(current: &RgbaImage, mask: &[bool], regions: &[(u32, u32, u32, u32)]) -> RgbaImage {
    let mut out = current.clone();
    let (w, h) = out.dimensions();
    // 半透明红覆盖差异像素
    for (i, p) in out.pixels_mut().enumerate() {
        if mask[i] {
            const RED: [u8; 3] = [255, 48, 48];
            const A: f32 = 0.45;
            p.0[0] = (p.0[0] as f32 * (1.0 - A) + RED[0] as f32 * A) as u8;
            p.0[1] = (p.0[1] as f32 * (1.0 - A) + RED[1] as f32 * A) as u8;
            p.0[2] = (p.0[2] as f32 * (1.0 - A) + RED[2] as f32 * A) as u8;
        }
    }
    // 包围盒红框（2px）
    for (x1, y1, x2, y2) in regions {
        for (x, y) in box_outline(*x1, *y1, *x2, *y2) {
            if x < w && y < h {
                out.put_pixel(x, y, image::Rgba([255, 0, 0, 255]));
            }
        }
    }
    out
}

/// 矩形描边像素坐标（2px 粗）
fn box_outline(x1: u32, y1: u32, x2: u32, y2: u32) -> Vec<(u32, u32)> {
    let mut pts = Vec::new();
    for x in x1..=x2 {
        for d in 0..2 {
            if y1 + d <= y2 {
                pts.push((x, y1 + d));
            }
            if y2 >= d {
                pts.push((x, y2 - d));
            }
        }
    }
    for y in y1..=y2 {
        for d in 0..2 {
            if x1 + d <= x2 {
                pts.push((x1 + d, y));
            }
            if x2 >= d {
                pts.push((x2 - d, y));
            }
        }
    }
    pts
}

fn load_rgba(path: &str) -> Result<RgbaImage, String> {
    if !Path::new(path).exists() {
        return Err(format!("截图不存在：{path}"));
    }
    image::open(path)
        .map(|img| img.to_rgba8())
        .map_err(|e| format!("读取截图失败 {path}: {e}"))
}

/// 执行视觉回归对比：返回 JSON 结果（差异统计 + 区域列表 + diff 图路径）
pub fn run_diff(
    baseline_path: &str,
    current_path: &str,
    tolerance: u8,
    threshold: f64,
    save_dir: Option<&str>,
) -> Result<Value, String> {
    let baseline = load_rgba(baseline_path)?;
    let mut current = load_rgba(current_path)?;

    // 尺寸不一致：把当前图缩放到基线尺寸再比（记录 resized）
    let resized = current.dimensions() != baseline.dimensions();
    if resized {
        current = image::imageops::resize(
            &current,
            baseline.width(),
            baseline.height(),
            image::imageops::FilterType::Triangle,
        );
    }

    let (w, h) = baseline.dimensions();
    let total = (w * h) as usize;
    let (changed, mask) = diff_mask(&baseline, &current, tolerance);
    let ratio = changed as f64 / total as f64;
    let regions = diff_regions(&mask, w, h, 8, 0.1, 8);

    // diff 图落盘：save_dir 优先，否则与当前截图同目录
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let dir = match save_dir {
        Some(d) if !d.is_empty() => std::path::PathBuf::from(d),
        _ => Path::new(current_path)
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_default(),
    };
    let diff_name = format!("diff-{ts}.png");
    let diff_path = dir.join(&diff_name);
    render_diff(&current, &mask, &regions)
        .save(&diff_path)
        .map_err(|e| format!("保存 diff 图失败: {e}"))?;

    Ok(json!({
        "ok": true,
        "pass": ratio <= threshold,
        "changed_pixels": changed,
        "total_pixels": total,
        "changed_ratio": (ratio * 10000.0).round() / 10000.0,
        "threshold": threshold,
        "tolerance": tolerance,
        "size": { "width": w, "height": h },
        "resized": resized,
        "regions": regions.iter().map(|(x1, y1, x2, y2)| json!({
            "x": x1, "y": y1, "w": x2 - x1 + 1, "h": y2 - y1 + 1
        })).collect::<Vec<_>>(),
        "diff_image": diff_path.to_string_lossy(),
    }))
}

/// agent 工具：视觉回归对比（current 省略时现场截屏）
pub struct VisualDiffTool {
    capability: Arc<dyn Capability>,
}

impl VisualDiffTool {
    pub fn new(capability: Arc<dyn Capability>) -> Self {
        Self { capability }
    }
}

impl Tool for VisualDiffTool {
    fn name(&self) -> &str {
        "visual_diff"
    }

    fn description(&self) -> &str {
        "视觉回归对比：把基线截图与当前画面做像素级 diff，找出 UI 改版/异常变化。\
         返回差异像素占比、差异区域包围盒（改了哪里）与红色高亮 diff 图路径。\
         使用时机：1) UI 测试中验证页面外观是否回归；2) 用户问「界面哪里变了」「对比这两张截图」；\
         3) 改版前后留档比对。current 省略时自动截取当前屏幕；两图尺寸不同会自动对齐。\
         判定：changed_ratio ≤ threshold（默认 2%）即 pass。"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "baseline": {
                    "type": "string",
                    "description": "基线截图的本地 PNG 路径（改版前/期望样子）"
                },
                "current": {
                    "type": "string",
                    "description": "当前截图路径；省略则现场截取屏幕"
                },
                "tolerance": {
                    "type": "number",
                    "description": "像素容差 0-100（每通道最大允许差值），默认 24；调大可忽略轻微压缩/抗锯齿噪声"
                },
                "threshold": {
                    "type": "number",
                    "description": "pass 判定的差异占比上限（0-1），默认 0.02 即 2%"
                },
                "save_dir": {
                    "type": "string",
                    "description": "diff 高亮图保存目录；省略则存到当前截图同目录"
                }
            },
            "required": ["baseline"]
        })
    }

    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }

    fn run(&self, args: Value) -> Result<Value, String> {
        let baseline = args["baseline"]
            .as_str()
            .ok_or("visual_diff 缺少 baseline（基线截图路径）")?
            .to_string();
        let current = match args["current"].as_str() {
            Some(p) if !p.is_empty() => p.to_string(),
            _ => self
                .capability
                .capture_screen()
                .map_err(|e| format!("截取当前屏幕失败: {e}"))?
                .path,
        };
        let tolerance = args["tolerance"].as_u64().unwrap_or(24).min(100) as u8;
        let threshold = match args["threshold"].as_f64() {
            Some(t) if (0.0..=1.0).contains(&t) => t,
            _ => 0.02,
        };
        let save_dir = args["save_dir"].as_str().map(|s| s.to_string());
        run_diff(
            &baseline,
            &current,
            tolerance,
            threshold,
            save_dir.as_deref(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn solid(w: u32, h: u32, c: [u8; 4]) -> RgbaImage {
        RgbaImage::from_pixel(w, h, Rgba(c))
    }

    #[test]
    fn identical_images_pass() {
        let img = solid(64, 64, [10, 20, 30, 255]);
        let (changed, mask) = diff_mask(&img, &img, 24);
        assert_eq!(changed, 0);
        assert!(!mask.iter().any(|m| *m));
    }

    #[test]
    fn tolerance_ignores_small_drift() {
        let a = solid(32, 32, [100, 100, 100, 255]);
        let b = solid(32, 32, [110, 110, 110, 255]);
        let (changed, _) = diff_mask(&a, &b, 24);
        assert_eq!(changed, 0, "通道差 10 < 容差 24，应视为相同");
        let (changed2, _) = diff_mask(&a, &b, 5);
        assert_eq!(changed2, 32 * 32, "通道差 10 > 容差 5，应全部判定差异");
    }

    #[test]
    fn changed_square_yields_one_region() {
        let baseline = solid(128, 128, [255, 255, 255, 255]);
        let mut current = baseline.clone();
        // 在 (40,40)-(63,63) 画一个 24x24 黑块
        for y in 40..64 {
            for x in 40..64 {
                current.put_pixel(x, y, Rgba([0, 0, 0, 255]));
            }
        }
        let (changed, mask) = diff_mask(&baseline, &current, 24);
        assert_eq!(changed, 24 * 24);
        let regions = diff_regions(&mask, 128, 128, 8, 0.1, 8);
        assert_eq!(regions.len(), 1, "24x24 连续变化应聚成一个区域");
        let (x1, y1, x2, y2) = regions[0];
        assert_eq!((x1, y1, x2, y2), (40, 40, 63, 63));
    }

    #[test]
    fn tiny_noise_filtered_by_min_size() {
        let baseline = solid(128, 128, [255, 255, 255, 255]);
        let mut current = baseline.clone();
        // 2x2 孤立噪点
        current.put_pixel(10, 10, Rgba([0, 0, 0, 255]));
        current.put_pixel(11, 10, Rgba([0, 0, 0, 255]));
        current.put_pixel(10, 11, Rgba([0, 0, 0, 255]));
        current.put_pixel(11, 11, Rgba([0, 0, 0, 255]));
        let (_, mask) = diff_mask(&baseline, &current, 24);
        let regions = diff_regions(&mask, 128, 128, 8, 0.1, 8);
        assert!(regions.is_empty(), "2x2 噪点应被 min_size=8 过滤");
    }

    #[test]
    fn diff_image_overlays_red_on_changed() {
        let baseline = solid(64, 64, [255, 255, 255, 255]);
        let mut current = baseline.clone();
        for y in 0..16 {
            for x in 0..16 {
                current.put_pixel(x, y, Rgba([0, 0, 0, 255]));
            }
        }
        let (_, mask) = diff_mask(&baseline, &current, 24);
        let regions = diff_regions(&mask, 64, 64, 8, 0.1, 8);
        let out = render_diff(&current, &mask, &regions);
        // 差异像素被红覆盖：R 分量显著高于原黑色
        let p = out.get_pixel(5, 5);
        assert!(p.0[0] > 100, "差异区应被红覆盖，实际 {:?}", p.0);
        // 未变化区域保持白色
        let q = out.get_pixel(40, 40);
        assert_eq!(q.0, [255, 255, 255, 255]);
    }

    #[test]
    fn run_diff_end_to_end_with_resize() {
        let dir = std::env::temp_dir().join(format!("baize-vdiff-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let base_path = dir.join("base.png");
        let cur_path = dir.join("cur.png");
        solid(80, 60, [200, 200, 200, 255]).save(&base_path).unwrap();
        // 尺寸不同（应自动缩放）+ 中央黑块
        let mut cur = solid(160, 120, [200, 200, 200, 255]);
        for y in 40..80 {
            for x in 40..120 {
                cur.put_pixel(x, y, Rgba([0, 0, 0, 255]));
            }
        }
        cur.save(&cur_path).unwrap();

        let out = run_diff(
            base_path.to_str().unwrap(),
            cur_path.to_str().unwrap(),
            24,
            0.02,
            None,
        )
        .unwrap();

        assert_eq!(out["resized"], json!(true));
        assert_eq!(out["pass"], json!(false), "大面积差异应 fail");
        let regions = out["regions"].as_array().unwrap();
        assert!(!regions.is_empty(), "应检出差异区域");
        let diff_img = out["diff_image"].as_str().unwrap();
        assert!(Path::new(diff_img).exists(), "diff 图应已落盘");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
