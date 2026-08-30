# -*- coding: utf-8 -*-
"""白泽桌面图标生成：深空圆角底座 + 大水球（玻璃质感青蓝渐变）+ 高光 + 气泡点缀。
输出 src-tauri/icons/ 下的 icon.ico / 32x32.png / 128x128.png / 128x128@2x.png / icon.png
"""
import math
import os

from PIL import Image, ImageDraw, ImageFilter

S = 1024  # 母版尺寸
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "src-tauri", "icons")
os.makedirs(OUT, exist_ok=True)


def lerp(a, b, t):
    return tuple(int(a[i] + (b[i] - a[i]) * t) for i in range(3))


def rounded_gradient_base(size):
    """深空圆角方形底座：左上深蓝黑 → 右下靛蓝的柔和渐变"""
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    grad = Image.new("RGBA", (size, size))
    top, bot = (10, 14, 30), (22, 34, 66)
    px = grad.load()
    for y in range(size):
        t = y / (size - 1)
        c = lerp(top, bot, t)
        for x in range(size):
            px[x, y] = (*c, 255)
    mask = Image.new("L", (size, size), 0)
    d = ImageDraw.Draw(mask)
    r = int(size * 0.225)  # 圆角
    d.rounded_rectangle([0, 0, size - 1, size - 1], radius=r, fill=255)
    img.paste(grad, (0, 0), mask)
    # 底座内缘细描边（微光）
    edge = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    de = ImageDraw.Draw(edge)
    de.rounded_rectangle(
        [1, 1, size - 2, size - 2], radius=r, outline=(103, 232, 249, 60), width=max(2, size // 512)
    )
    img = Image.alpha_composite(img, edge)
    return img


def draw_water_ball(size):
    """大水球：径向渐变球体 + 顶部玻璃高光 + 青色边缘光 + 底部投影"""
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    cx, cy = size * 0.5, size * 0.46
    R = size * 0.30  # 球半径

    # 球体：逐环径向渐变（中心浅青 → 边缘深蓝），加一点左上偏移的光源感
    core = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    d = ImageDraw.Draw(core)
    steps = 160
    c_in, c_mid, c_out = (125, 232, 250), (34, 160, 238), (18, 60, 150)
    lx, ly = cx - R * 0.35, cy - R * 0.4  # 光源偏移
    for i in range(steps, 0, -1):
        t = 1 - i / steps  # 0=外缘 1=中心
        r = R * i / steps
        # 距光源偏移中心的距离决定明暗（模拟球面光照）
        dx, dy = (cx - lx) / R, (cy - ly) / R
        light = 1 - min(1.0, math.hypot(dx * (1 - t), dy * (1 - t)) * 0.9)
        col = lerp(c_out, c_mid, t) if t < 0.55 else lerp(c_mid, c_in, (t - 0.55) / 0.45)
        col = lerp(col, (210, 250, 255), light * 0.35)
        alpha = int(255 * min(1.0, 0.35 + t * 0.75))
        d.ellipse([cx - r, cy - r, cx + r, cy + r], fill=(*col, alpha))
    img = Image.alpha_composite(img, core)

    # 青色边缘辉光
    glow = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    dg = ImageDraw.Draw(glow)
    dg.ellipse(
        [cx - R * 1.01, cy - R * 1.01, cx + R * 1.01, cy + R * 1.01],
        outline=(103, 232, 249, 200),
        width=max(3, size // 200),
    )
    glow = glow.filter(ImageFilter.GaussianBlur(size * 0.012))
    img = Image.alpha_composite(img, glow)

    # 顶部玻璃高光：斜置椭圆，强模糊
    hl = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    dh = ImageDraw.Draw(hl)
    dh.ellipse(
        [cx - R * 0.62, cy - R * 0.78, cx + R * 0.05, cy - R * 0.18],
        fill=(255, 255, 255, 175),
    )
    hl = hl.filter(ImageFilter.GaussianBlur(size * 0.028))
    img = Image.alpha_composite(img, hl)
    # 小亮点
    dot = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    dd = ImageDraw.Draw(dot)
    dd.ellipse(
        [cx - R * 0.48, cy - R * 0.58, cx - R * 0.30, cy - R * 0.40],
        fill=(255, 255, 255, 230),
    )
    dot = dot.filter(ImageFilter.GaussianBlur(size * 0.006))
    img = Image.alpha_composite(img, dot)

    # 底部反光弧（水面感）
    rl = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    dr = ImageDraw.Draw(rl)
    dr.arc(
        [cx - R * 0.72, cy - R * 0.72, cx + R * 0.72, cy + R * 0.72],
        start=35, end=145,
        fill=(103, 232, 249, 150),
        width=max(3, size // 240),
    )
    rl = rl.filter(ImageFilter.GaussianBlur(size * 0.008))
    img = Image.alpha_composite(img, rl)

    # 环绕小气泡（卫星水珠）
    bubbles = [
        (cx + R * 1.18, cy - R * 0.78, R * 0.10),
        (cx - R * 1.22, cy + R * 0.42, R * 0.075),
        (cx + R * 1.05, cy + R * 0.92, R * 0.06),
    ]
    for bx, by, br in bubbles:
        b = Image.new("RGBA", (size, size), (0, 0, 0, 0))
        db = ImageDraw.Draw(b)
        for i in range(24, 0, -1):
            t = i / 24
            col = lerp((14, 90, 170), (160, 240, 255), 1 - t)
            db.ellipse(
                [bx - br * t, by - br * t, bx + br * t, by + br * t],
                fill=(*col, int(230 * (1 - t * 0.4))),
            )
        db.ellipse(
            [bx - br * 0.45, by - br * 0.5, bx - br * 0.05, by - br * 0.1],
            fill=(255, 255, 255, 190),
        )
        img = Image.alpha_composite(img, b.filter(ImageFilter.GaussianBlur(size * 0.002)))

    return img


def compose(size):
    """底座 + 居中缩放的水球 + 底座下方柔和投影"""
    base = rounded_gradient_base(size)
    ball = draw_water_ball(size)

    # 投影：椭圆黑晕在球底
    sh = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    ds = ImageDraw.Draw(sh)
    ds.ellipse(
        [size * 0.28, size * 0.70, size * 0.72, size * 0.82],
        fill=(0, 0, 0, 120),
    )
    sh = sh.filter(ImageFilter.GaussianBlur(size * 0.035))
    base = Image.alpha_composite(base, sh)

    ball_small = ball.resize((size, size), Image.LANCZOS)
    base = Image.alpha_composite(base, ball_small)
    return base


def main():
    master = compose(S)
    master.save(os.path.join(OUT, "icon.png"))
    master.resize((256, 256), Image.LANCZOS).save(os.path.join(OUT, "128x128@2x.png"))
    master.resize((128, 128), Image.LANCZOS).save(os.path.join(OUT, "128x128.png"))
    master.resize((32, 32), Image.LANCZOS).save(os.path.join(OUT, "32x32.png"))
    master.resize(
        (256, 256), Image.LANCZOS
    ).save(
        os.path.join(OUT, "icon.ico"),
        sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
    )
    print("icons written to", OUT)


if __name__ == "__main__":
    main()
