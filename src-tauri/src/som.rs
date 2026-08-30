//! Set-of-Marks 视觉标注：把连续像素坐标回归问题降维为「选编号」分类问题。
//!
//! 核心思路（对齐 OmniParser / UI-TARS / GPT-4o Set-of-Marks）：
//! 1. 用本地 OCR 拿到候选文字块；
//! 2. 在截图上给每个候选画彩色矩形框 + 左上角序号；
//! 3. 把「带编号的截图 + 候选清单」喂给视觉模型，模型只需回一个编号；
//! 4. 代码查表得到精确坐标，不再让模型盲猜像素 (x,y)。
//!
//! 点阵数字用 5×7 经典字体，纯 put_pixel 绘制，不引入字体文件 / 图像绘图依赖。

/// 使用视觉模型，从标注截图的 N 个候选框中选出目标对应的编号。
/// 返回 0-based 编号；失败返回 None。
pub fn som_select(annotated_path: &str, target: &str, n: usize) -> Option<usize> {
    if n == 0 {
        return None;
    }
    let (base64, _scale) = crate::visual_grounding::load_base64_downscaled(annotated_path, 896);
    if base64.is_empty() {
        return None;
    }
    let prompt = format!(
        "图中标记了 {} 个彩色矩形框，每个框左上角有一个编号（从 1 到 {}）。\
         请找出与「{}」最匹配的那个框，只回复它的编号（一个整数），不要其它任何文字。找不到就回复 0。",
        n, n, target
    );
    // 统一视觉调用：内部处理熔断 / 网络失败记录 / 成功清熔断。
    let text = crate::visual_grounding::vision_generate_raw(
        &base64,
        &prompt,
        std::time::Duration::from_secs(15),
    )
    .ok()?;
    let num: usize = text
        .split(|c: char| !c.is_ascii_digit())
        .find_map(|s| s.parse::<usize>().ok())?;
    if num == 0 || num > n {
        None
    } else {
        Some(num - 1)
    }
}

/// 计算两张截图的界面变化百分比（降采样逐像素 RGB 差异）。
/// 返回 0.0~100.0，用于操作后判断界面是否发生变化（<1% 视为几乎未变化）。
pub fn image_diff_pct(before: &str, after: &str) -> f64 {
    let (Ok(a), Ok(b)) = (image::open(before), image::open(after)) else {
        return 0.0;
    };
    let a = a.to_rgb8();
    let b = b.to_rgb8();
    if a.dimensions() != b.dimensions() {
        return 100.0;
    }
    let (w, h) = a.dimensions();
    if w == 0 || h == 0 {
        return 0.0;
    }
    let mut changed = 0u64;
    let mut sampled = 0u64;
    for y in (0..h).step_by(2) {
        for x in (0..w).step_by(2) {
            let pa = a.get_pixel(x, y);
            let pb = b.get_pixel(x, y);
            let dr = (pa[0] as i32 - pb[0] as i32).abs();
            let dg = (pa[1] as i32 - pb[1] as i32).abs();
            let db = (pa[2] as i32 - pb[2] as i32).abs();
            if dr + dg + db > 60 {
                changed += 1;
            }
            sampled += 1;
        }
    }
    if sampled == 0 {
        return 0.0;
    }
    changed as f64 * 100.0 / sampled as f64
}

/// 在截图上给候选框画矩形 + 左上角序号，写出标注图，返回 (新图路径, 候选中心坐标列表)。
/// `candidates` 为 (x, y, w, h)，坐标为截图内像素。
pub fn annotate(
    image_path: &str,
    candidates: &[(i32, i32, i32, i32)],
) -> Result<(String, Vec<(f64, f64)>), String> {
    if candidates.is_empty() {
        return Err("无可标注候选框".to_string());
    }

    let mut img = image::open(image_path)
        .map_err(|e| format!("读取截图失败: {e}"))?
        .to_rgba8();
    let (img_w, img_h) = img.dimensions();

    for (i, &(x, y, w, h)) in candidates.iter().enumerate() {
        draw_rect(&mut img, x, y, w, h, (231, 70, 58)); // 红色框
        // 左上角 14×22 黑底序号块
        let bx = x.max(0);
        let by = y.max(0);
        let bw = 14.min(img_w as i32 - bx);
        let bh = 22.min(img_h as i32 - by);
        for py in by..(by + bh).min(img_h as i32) {
            for px in bx..(bx + bw).min(img_w as i32) {
                img.put_pixel(px as u32, py as u32, image::Rgba([20, 20, 20, 255]));
            }
        }
        draw_digit(&mut img, bx + 1, by + 2, (i + 1) as u32, 2, (255, 255, 255));
    }

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let name = format!("baize-som-{ts}.png");
    img.save(&name).map_err(|e| format!("保存标注图失败: {e}"))?;
    let path = std::env::current_dir()
        .map(|d| d.join(&name).to_string_lossy().to_string())
        .unwrap_or(name);

    let centers = candidates
        .iter()
        .map(|&(x, y, w, h)| (x as f64 + w as f64 / 2.0, y as f64 + h as f64 / 2.0))
        .collect();
    Ok((path, centers))
}

/// 画空心矩形（2px 边框），超出图边界自动裁剪。
fn draw_rect(img: &mut image::RgbaImage, x: i32, y: i32, w: i32, h: i32, c: (u8, u8, u8)) {
    let (iw, ih) = img.dimensions();
    let x1 = x.max(0);
    let y1 = y.max(0);
    let x2 = (x + w).min(iw as i32 - 1);
    let y2 = (y + h).min(ih as i32 - 1);
    if x2 <= x1 || y2 <= y1 {
        return;
    }
    let mut set = |px: i32, py: i32| {
        if px >= 0 && py >= 0 && (px as u32) < iw && (py as u32) < ih {
            img.put_pixel(px as u32, py as u32, image::Rgba([c.0, c.1, c.2, 255]));
        }
    };
    for px in x1..=x2 {
        set(px, y1);
        set(px, y1 + 1);
        set(px, y2);
        set(px, y2 - 1);
    }
    for py in y1..=y2 {
        set(x1, py);
        set(x1 + 1, py);
        set(x2, py);
        set(x2 - 1, py);
    }
}

/// 用 5×7 点阵字体画数字（scale 为像素缩放倍数）。
fn draw_digit(img: &mut image::RgbaImage, x0: i32, y0: i32, digit: u32, scale: u32, c: (u8, u8, u8)) {
    type Row = [u8; 5];
    let glyph: [Row; 7] = match digit {
        0 => [[0,1,1,1,0],[1,0,0,0,1],[1,0,0,1,1],[1,0,1,0,1],[1,1,0,0,1],[1,0,0,0,1],[0,1,1,1,0]],
        1 => [[0,0,1,0,0],[0,1,1,0,0],[0,0,1,0,0],[0,0,1,0,0],[0,0,1,0,0],[0,0,1,0,0],[0,1,1,1,0]],
        2 => [[0,1,1,1,0],[1,0,0,0,1],[0,0,0,0,1],[0,0,0,1,0],[0,0,1,0,0],[0,1,0,0,0],[1,1,1,1,1]],
        3 => [[1,1,1,1,1],[0,0,0,1,0],[0,0,1,0,0],[0,0,0,1,0],[0,0,0,0,1],[1,0,0,0,1],[0,1,1,1,0]],
        4 => [[0,0,0,1,0],[0,0,1,1,0],[0,1,0,1,0],[1,0,0,1,0],[1,1,1,1,1],[0,0,0,1,0],[0,0,0,1,0]],
        5 => [[1,1,1,1,1],[1,0,0,0,0],[1,1,1,1,0],[0,0,0,0,1],[0,0,0,0,1],[1,0,0,0,1],[0,1,1,1,0]],
        6 => [[0,0,1,1,0],[0,1,0,0,0],[1,0,0,0,0],[1,1,1,1,0],[1,0,0,0,1],[1,0,0,0,1],[0,1,1,1,0]],
        7 => [[1,1,1,1,1],[0,0,0,0,1],[0,0,0,1,0],[0,0,1,0,0],[0,1,0,0,0],[0,1,0,0,0],[0,1,0,0,0]],
        8 => [[0,1,1,1,0],[1,0,0,0,1],[1,0,0,0,1],[0,1,1,1,0],[1,0,0,0,1],[1,0,0,0,1],[0,1,1,1,0]],
        9 => [[0,1,1,1,0],[1,0,0,0,1],[1,0,0,0,1],[0,1,1,1,1],[0,0,0,0,1],[0,0,0,1,0],[0,0,1,0,0]],
        _ => [[1,1,1,1,1],[1,0,0,0,1],[1,0,0,0,1],[1,0,0,0,1],[1,0,0,0,1],[1,0,0,0,1],[1,1,1,1,1]],
    };

    let (iw, ih) = img.dimensions();
    for (row, bits) in glyph.iter().enumerate() {
        for (col, &on) in bits.iter().enumerate() {
            if on == 1 {
                for dy in 0..scale {
                    for dx in 0..scale {
                        let px = x0 + col as i32 * scale as i32 + dx as i32;
                        let py = y0 + row as i32 * scale as i32 + dy as i32;
                        if px >= 0 && py >= 0 && (px as u32) < iw && (py as u32) < ih {
                            img.put_pixel(px as u32, py as u32, image::Rgba([c.0, c.1, c.2, 255]));
                        }
                    }
                }
            }
        }
    }
}