# -*- coding: utf-8 -*-
"""白泽 Office 文档生成与转换 sidecar。

stdin 接收 JSON 请求，stdout 输出 JSON 响应。

支持操作：
  op=docx   markdown → .docx（完整排版设计系统）
  op=pptx   大纲/JSON → .pptx（16:9 主题设计）
  op=docx_to_md   .docx → markdown
  op=pdf_merge / op=pdf_split
  op=xlsx_to_csv / op=csv_to_xlsx
"""

import json
import os
import re
import sys
import datetime


def respond(obj):
    sys.stdout.write(json.dumps(obj, ensure_ascii=False))
    sys.stdout.flush()


# ── 进度上报协议（Rust 侧逐行读 stderr，转成执行流进度条） ──
# 行格式：@@BAIZE_PROGRESS {"pct": 42.0, "msg": "渲染第 3/10 页幻灯片"}
PROGRESS_TAG = "@@BAIZE_PROGRESS"


def _progress(pct, msg):
    try:
        pct = max(0.0, min(100.0, float(pct)))
        sys.stderr.write(
            PROGRESS_TAG + " " + json.dumps({"pct": round(pct, 1), "msg": str(msg)},
                                            ensure_ascii=False) + "\n")
        sys.stderr.flush()
    except Exception:
        pass


# ══════════════════════════ 主题常量 ══════════════════════════

ACCENT = "1F4E79"      # 深蓝：标题 / 表头 / 强调
ACCENT2 = "2E74B5"     # 亮蓝：次级强调 / 边框
GRAY = "595959"        # 正文灰
LIGHT = "D9E2F3"       # 浅蓝：斑马纹 / 分隔
CODE_BG = "F2F2F2"     # 代码底
QUOTE_BG = "F7F9FC"    # 引用底

THEME_FONTS = {
    # 经典商务：正文宋体，标题微软雅黑
    "classic": {"body": "宋体", "head": "微软雅黑", "code": "Consolas"},
    # 现代极简：正文与标题均微软雅黑
    "modern": {"body": "微软雅黑", "head": "微软雅黑", "code": "Consolas"},
}


# ══════════════════════════ op=docx ══════════════════════════

def render_docx(req):
    import docx
    from docx import Document
    from docx.shared import Pt, Cm, Inches, RGBColor
    from docx.enum.text import WD_ALIGN_PARAGRAPH, WD_LINE_SPACING
    from docx.enum.table import WD_TABLE_ALIGNMENT
    from docx.oxml.ns import qn
    from docx.oxml import OxmlElement

    fonts = THEME_FONTS.get(req.get("theme", "classic"), THEME_FONTS["classic"])
    body_indent = req.get("indent_body", True)

    def set_cn_font(run, cn, ascii_font=None):
        run.font.name = ascii_font or cn
        rPr = run._element.get_or_add_rPr()
        rFonts = rPr.find(qn("w:rFonts"))
        if rFonts is None:
            rFonts = OxmlElement("w:rFonts")
            rPr.append(rFonts)
        rFonts.set(qn("w:eastAsia"), cn)
        rFonts.set(qn("w:ascii"), ascii_font or cn)
        rFonts.set(qn("w:hAnsi"), ascii_font or cn)

    def shade_para(p, fill):
        pPr = p._p.get_or_add_pPr()
        shd = OxmlElement("w:shd")
        shd.set(qn("w:val"), "clear")
        shd.set(qn("w:fill"), fill)
        pPr.append(shd)

    def para_border(p, edges, color=ACCENT2, size=8, space=4):
        pPr = p._p.get_or_add_pPr()
        pBdr = OxmlElement("w:pBdr")
        for e in edges:
            b = OxmlElement(f"w:{e}")
            b.set(qn("w:val"), "single")
            b.set(qn("w:sz"), str(size))
            b.set(qn("w:space"), str(space))
            b.set(qn("w:color"), color)
            pBdr.append(b)
        pPr.append(pBdr)

    def add_hyperlink(p, text, url):
        from docx.opc.constants import RELATIONSHIP_TYPE
        r_id = p.part.relate_to(url, RELATIONSHIP_TYPE.HYPERLINK, is_external=True)
        hl = OxmlElement("w:hyperlink")
        hl.set(qn("r:id"), r_id)
        r = OxmlElement("w:r")
        rPr = OxmlElement("w:rPr")
        c = OxmlElement("w:color")
        c.set(qn("w:val"), ACCENT2)
        rPr.append(c)
        u = OxmlElement("w:u")
        u.set(qn("w:val"), "single")
        rPr.append(u)
        r.append(rPr)
        t = OxmlElement("w:t")
        t.set(qn("xml:space"), "preserve")
        t.text = text
        r.append(t)
        hl.append(r)
        p._p.append(hl)

    def add_page_number(section):
        footer = section.footer
        p = footer.paragraphs[0] if footer.paragraphs else footer.add_paragraph()
        p.alignment = WD_ALIGN_PARAGRAPH.CENTER
        run = p.add_run()
        run.font.size = Pt(9)
        run.font.color.rgb = RGBColor(0x8C, 0x8C, 0x8C)
        set_cn_font(run, fonts["body"], "Times New Roman")
        fld1 = OxmlElement("w:fldChar")
        fld1.set(qn("w:fldCharType"), "begin")
        instr = OxmlElement("w:instrText")
        instr.set(qn("xml:space"), "preserve")
        instr.text = " PAGE "
        fld2 = OxmlElement("w:fldChar")
        fld2.set(qn("w:fldCharType"), "end")
        run._r.append(fld1)
        run._r.append(instr)
        run._r.append(fld2)

    doc = Document()

    # ── 页面：A4 + 中文文档标准页边距 ──
    sec = doc.sections[0]
    sec.page_width, sec.page_height = Cm(21.0), Cm(29.7)
    sec.top_margin = sec.bottom_margin = Cm(2.54)
    sec.left_margin = sec.right_margin = Cm(3.0)
    add_page_number(sec)

    # ── Normal 样式：正文字体 / 1.5 倍行距 ──
    normal = doc.styles["Normal"]
    normal.font.name = fonts["body"]
    normal.font.size = Pt(12)
    normal.font.color.rgb = RGBColor(0x33, 0x33, 0x33)
    normal.element.rPr.rFonts.set(qn("w:eastAsia"), fonts["body"])
    normal.paragraph_format.line_spacing = 1.5
    normal.paragraph_format.space_after = Pt(6)

    # ── 标题色阶（H1 深蓝大字+底线装饰，逐级收敛）──
    HEAD_SPEC = [
        (1, 22, ACCENT, 18, 10, True),
        (2, 16, ACCENT, 14, 8, True),
        (3, 13.5, "2F5496", 10, 6, True),
        (4, 12, "404040", 8, 4, False),
    ]
    for lvl, size, color, before, after, bold in HEAD_SPEC:
        st = doc.styles[f"Heading {lvl}"]
        st.font.name = fonts["head"]
        st.font.size = Pt(size)
        st.font.bold = bold
        st.font.color.rgb = RGBColor.from_string(color)
        rpr = st.element.get_or_add_rPr()
        rf = rpr.find(qn("w:rFonts"))
        if rf is None:
            rf = OxmlElement("w:rFonts")
            rpr.append(rf)
        rf.set(qn("w:eastAsia"), fonts["head"])
        st.paragraph_format.space_before = Pt(before)
        st.paragraph_format.space_after = Pt(after)
        st.paragraph_format.line_spacing = 1.3

    # ── 行内标记：**粗** *斜* `码` [文](链) ──
    INLINE_RE = re.compile(r"(\*\*(.+?)\*\*|\*(.+?)\*|`([^`]+)`|\[([^\]]+)\]\(([^)]+)\))")

    def write_inline(p, text, base_size=None):
        pos = 0
        for m in INLINE_RE.finditer(text):
            if m.start() > pos:
                _plain_run(p, text[pos:m.start()], base_size)
            if m.group(2) is not None:
                r = p.add_run(m.group(2))
                r.bold = True
                if base_size:
                    r.font.size = Pt(base_size)
            elif m.group(3) is not None:
                r = p.add_run(m.group(3))
                r.italic = True
                if base_size:
                    r.font.size = Pt(base_size)
            elif m.group(4) is not None:
                r = p.add_run(m.group(4))
                r.font.name = fonts["code"]
                r._element.get_or_add_rPr().rFonts.set(qn("w:eastAsia"), fonts["code"])
                r.font.size = Pt((base_size or 12) - 1.5)
                r.font.color.rgb = RGBColor.from_string("C7254E")
                rPr = r._element.get_or_add_rPr()
                shd = OxmlElement("w:shd")
                shd.set(qn("w:val"), "clear")
                shd.set(qn("w:fill"), "F5F5F5")
                rPr.append(shd)
            else:
                add_hyperlink(p, m.group(5), m.group(6))
            pos = m.end()
        if pos < len(text):
            _plain_run(p, text[pos:], base_size)

    def _plain_run(p, text, base_size=None):
        r = p.add_run(text)
        set_cn_font(r, fonts["body"], "Times New Roman")
        if base_size:
            r.font.size = Pt(base_size)
        return r

    def body_para(text, first_indent=True):
        p = doc.add_paragraph()
        if first_indent and body_indent:
            p.paragraph_format.first_line_indent = Pt(24)  # 首行缩进两字符
        write_inline(p, text)
        return p

    # ── 封面页 ──
    title = (req.get("title") or "").strip()
    if title:
        for _ in range(5):
            doc.add_paragraph()
        p = doc.add_paragraph()
        p.alignment = WD_ALIGN_PARAGRAPH.CENTER
        r = p.add_run(title)
        r.font.size = Pt(28)
        r.font.bold = True
        r.font.color.rgb = RGBColor.from_string(ACCENT)
        set_cn_font(r, fonts["head"])
        # 标题下装饰短线（用下划线边框实现）
        bar = doc.add_paragraph()
        bar.alignment = WD_ALIGN_PARAGRAPH.CENTER
        br = bar.add_run("　" * 8)
        br.font.size = Pt(6)
        para_border(bar, ("bottom",), color=ACCENT2, size=18, space=1)
        bar.paragraph_format.space_after = Pt(20)
        sub = (req.get("subtitle") or "").strip()
        if sub:
            sp = doc.add_paragraph()
            sp.alignment = WD_ALIGN_PARAGRAPH.CENTER
            sr = sp.add_run(sub)
            sr.font.size = Pt(15)
            sr.font.color.rgb = RGBColor.from_string(GRAY)
            set_cn_font(sr, fonts["head"])
        meta = doc.add_paragraph()
        meta.alignment = WD_ALIGN_PARAGRAPH.CENTER
        meta.paragraph_format.space_before = Pt(30)
        author = (req.get("author") or "").strip()
        today = datetime.date.today().strftime("%Y 年 %m 月 %d 日")
        meta_text = " · ".join(x for x in [author, today] if x)
        mr = meta.add_run(meta_text)
        mr.font.size = Pt(11)
        mr.font.color.rgb = RGBColor.from_string("8C8C8C")
        set_cn_font(mr, fonts["body"], "Times New Roman")
        if req.get("toc"):
            doc.add_page_break()
            h = doc.add_paragraph()
            hr = h.add_run("目　录")
            hr.font.size = Pt(18)
            hr.font.bold = True
            hr.font.color.rgb = RGBColor.from_string(ACCENT)
            set_cn_font(hr, fonts["head"])
            h.alignment = WD_ALIGN_PARAGRAPH.CENTER
            fld_p = doc.add_paragraph()
            fld = OxmlElement("w:fldSimple")
            fld.set(qn("w:instr"), r'TOC \o "1-3" \h \z \u')
            inner_r = OxmlElement("w:r")
            inner_t = OxmlElement("w:t")
            inner_t.text = "（目录域：打开文档后按 F9 更新生成）"
            inner_r.append(inner_t)
            fld.append(inner_r)
            fld_p._p.append(fld)
        doc.add_page_break()

    # ── Markdown 块级解析 ──
    content = req.get("content") or ""
    lines = content.replace("\r\n", "\n").split("\n")
    i, n = 0, len(lines)
    first_h1_skipped = False
    code_re = re.compile(r"^```(\w*)\s*$")
    _progress(12.0, "封面与目录已排版，正在写入正文…")
    while i < n:
        line = lines[i]
        stripped = line.strip()
        # 正文逐行进度（Rust 侧按 120ms 节流，不会刷屏）
        if n > 0:
            _progress(12.0 + 78.0 * i / n, f"排版正文 {i}/{n} 行 · 约 {int(12 + 78 * i / n)}%")

        # 围栏代码块
        m = code_re.match(stripped)
        if m:
            i += 1
            code_lines = []
            while i < n and not lines[i].strip().startswith("```"):
                code_lines.append(lines[i])
                i += 1
            i += 1  # 跳过收尾 ```
            for j, cl in enumerate(code_lines):
                cp = doc.add_paragraph()
                cp.paragraph_format.space_before = Pt(8 if j == 0 else 0)
                cp.paragraph_format.space_after = Pt(8 if j == len(code_lines) - 1 else 0)
                cp.paragraph_format.line_spacing = 1.15
                cp.paragraph_format.left_indent = Inches(0.25)
                shade_para(cp, CODE_BG)
                if j == 0:
                    para_border(cp, ("top",), color=LIGHT, size=4)
                if j == len(code_lines) - 1:
                    para_border(cp, ("bottom",), color=LIGHT, size=4)
                cr = cp.add_run(cl if cl else " ")
                cr.font.name = fonts["code"]
                cr._element.get_or_add_rPr().rFonts.set(qn("w:eastAsia"), fonts["code"])
                cr.font.size = Pt(9.5)
                cr.font.color.rgb = RGBColor.from_string("24292F")
            continue

        # 标题（首个与封面同名的 H1 跳过，避免重复）
        mh = re.match(r"^(#{1,6})\s+(.*)$", stripped)
        if mh:
            lvl = len(mh.group(1))
            text = mh.group(2).strip()
            if lvl == 1 and not first_h1_skipped and title and text == title:
                first_h1_skipped = True
                i += 1
                continue
            lvl = min(lvl, 4)
            p = doc.add_heading("", level=lvl)
            write_inline(p, text)
            if lvl == 1:
                para_border(p, ("bottom",), color=ACCENT2, size=12, space=2)
            i += 1
            continue

        # 分隔线 → 居中细线
        if re.match(r"^(-{3,}|\*{3,})$", stripped):
            p = doc.add_paragraph()
            p.paragraph_format.space_before = Pt(6)
            p.paragraph_format.space_after = Pt(6)
            r = p.add_run("　")
            r.font.size = Pt(4)
            para_border(p, ("bottom",), color=LIGHT, size=6)
            i += 1
            continue

        # 引用块（连续 > 行合并）
        if stripped.startswith(">"):
            quote_lines = []
            while i < n and lines[i].strip().startswith(">"):
                quote_lines.append(lines[i].strip().lstrip(">").strip())
                i += 1
            qp = doc.add_paragraph()
            qp.paragraph_format.left_indent = Inches(0.3)
            qp.paragraph_format.space_before = Pt(6)
            qp.paragraph_format.space_after = Pt(6)
            shade_para(qp, QUOTE_BG)
            para_border(qp, ("left",), color=ACCENT2, size=16)
            write_inline(qp, " ".join(x for x in quote_lines if x))
            for r in qp.runs:
                r.font.color.rgb = RGBColor.from_string(GRAY)
            continue

        # 表格（连续 | 行）
        if stripped.startswith("|") and i + 1 < n and re.match(r"^\|?[\s:|-]+\|?\s*$", lines[i + 1].strip()) and "-" in lines[i + 1]:
            rows = []
            header = [c.strip() for c in stripped.strip("|").split("|")]
            i += 2
            while i < n and lines[i].strip().startswith("|"):
                rows.append([c.strip() for c in lines[i].strip().strip("|").split("|")])
                i += 1
            ncol = len(header)
            tbl = doc.add_table(rows=len(rows) + 1, cols=ncol)
            tbl.alignment = WD_TABLE_ALIGNMENT.CENTER
            # 边框：外框亮蓝、内线浅灰
            tblPr = tbl._tbl.tblPr
            borders = OxmlElement("w:tblBorders")
            for edge, color, sz in [
                ("top", ACCENT, 8), ("bottom", ACCENT, 8),
                ("left", LIGHT, 4), ("right", LIGHT, 4),
                ("insideH", LIGHT, 4), ("insideV", LIGHT, 4),
            ]:
                e = OxmlElement(f"w:{edge}")
                e.set(qn("w:val"), "single")
                e.set(qn("w:sz"), str(sz))
                e.set(qn("w:color"), color)
                borders.append(e)
            tblPr.append(borders)
            for c, txt in enumerate(header):
                cell = tbl.rows[0].cells[c]
                cell.text = ""
                cp = cell.paragraphs[0]
                cr = cp.add_run(txt)
                cr.font.bold = True
                cr.font.size = Pt(10.5)
                cr.font.color.rgb = RGBColor(0xFF, 0xFF, 0xFF)
                set_cn_font(cr, fonts["head"])
                tcPr = cell._tc.get_or_add_tcPr()
                shd = OxmlElement("w:shd")
                shd.set(qn("w:val"), "clear")
                shd.set(qn("w:fill"), ACCENT)
                tcPr.append(shd)
            for ri, row in enumerate(rows):
                for c in range(ncol):
                    txt = row[c] if c < len(row) else ""
                    cell = tbl.rows[ri + 1].cells[c]
                    cell.text = ""
                    cp = cell.paragraphs[0]
                    write_inline(cp, txt, base_size=10.5)
                    cp.paragraph_format.line_spacing = 1.2
                    if ri % 2 == 1:
                        tcPr = cell._tc.get_or_add_tcPr()
                        shd = OxmlElement("w:shd")
                        shd.set(qn("w:val"), "clear")
                        shd.set(qn("w:fill"), "F4F7FC")
                        tcPr.append(shd)
            sp = doc.add_paragraph()
            sp.paragraph_format.space_after = Pt(4)
            sp.add_run("").font.size = Pt(2)
            continue

        # 图片 ![alt](path)
        mi = re.match(r"^!\[([^\]]*)\]\(([^)]+)\)\s*$", stripped)
        if mi:
            img_path = mi.group(2)
            if os.path.exists(img_path):
                doc.add_picture(img_path, width=Inches(5.8))
                doc.paragraphs[-1].alignment = WD_ALIGN_PARAGRAPH.CENTER
            else:
                p = doc.add_paragraph()
                r = p.add_run(f"[图片缺失: {img_path}]")
                r.italic = True
                r.font.size = Pt(10)
                r.font.color.rgb = RGBColor.from_string("B0B0B0")
            i += 1
            continue

        # 无序列表（两级缩进）
        mul = re.match(r"^(\s*)([-*+])\s+(.*)$", line)
        if mul:
            level = 1 if len(mul.group(1)) >= 2 else 0
            style = "List Bullet 2" if level else "List Bullet"
            p = doc.add_paragraph(style=style)
            p.paragraph_format.line_spacing = 1.5
            write_inline(p, mul.group(3))
            i += 1
            continue

        # 有序列表
        mol = re.match(r"^(\s*)(\d+)[.、]\s+(.*)$", line)
        if mol:
            level = 1 if len(mol.group(1)) >= 2 else 0
            style = "List Number 2" if level else "List Number"
            p = doc.add_paragraph(style=style)
            p.paragraph_format.line_spacing = 1.5
            write_inline(p, mol.group(3))
            i += 1
            continue

        # 空行
        if not stripped:
            i += 1
            continue

        # 普通段落（连续非空行合并）
        para = [stripped]
        i += 1
        while i < n and lines[i].strip() and not re.match(r"^(#|>|\||```|!\[|[-*+]\s|\d+[.、]\s|-{3,})", lines[i].strip()):
            para.append(lines[i].strip())
            i += 1
        body_para(" ".join(para))

    out = req["path"]
    os.makedirs(os.path.dirname(os.path.abspath(out)), exist_ok=True)
    _progress(94.0, "正在保存 .docx 文件…")
    doc.save(out)
    respond({
        "ok": True,
        "path": out,
        "bytes": os.path.getsize(out),
        "theme": req.get("theme", "classic"),
        "has_cover": bool(title),
        "has_toc": bool(req.get("toc")),
    })


# ══════════════════════════ op=pptx ══════════════════════════

def render_pptx(req):
    from pptx import Presentation
    from pptx.util import Inches, Pt, Emu
    from pptx.dml.color import RGBColor
    from pptx.enum.text import PP_ALIGN, MSO_ANCHOR
    from pptx.enum.shapes import MSO_SHAPE

    NAVY = RGBColor(0x1F, 0x38, 0x64)
    BLUE = RGBColor(0x2E, 0x74, 0xB5)
    DARK = RGBColor(0x33, 0x33, 0x33)
    GRAYC = RGBColor(0x8C, 0x8C, 0x8C)
    WHITE = RGBColor(0xFF, 0xFF, 0xFF)

    prs = Presentation()
    prs.slide_width = Inches(13.333)
    prs.slide_height = Inches(7.5)
    blank = prs.slide_layouts[6]

    def add_rect(slide, x, y, w, h, color):
        shp = slide.shapes.add_shape(MSO_SHAPE.RECTANGLE, x, y, w, h)
        shp.fill.solid()
        shp.fill.fore_color.rgb = color
        shp.line.fill.background()
        shp.shadow.inherit = False
        return shp

    def add_text(slide, x, y, w, h, text, size, color, bold=False, align=PP_ALIGN.LEFT, font="微软雅黑"):
        tb = slide.shapes.add_textbox(x, y, w, h)
        tf = tb.text_frame
        tf.word_wrap = True
        p = tf.paragraphs[0]
        p.alignment = align
        r = p.add_run()
        r.text = text
        r.font.size = Pt(size)
        r.font.bold = bold
        r.font.color.rgb = color
        r.font.name = font
        return tb

    deck_title = (req.get("title") or "").strip()
    subtitle = (req.get("subtitle") or "").strip()
    author = (req.get("author") or "").strip()
    total_base = 1

    # ── 封面页：深蓝全屏 + 白色大标题 + 强调条 ──
    s = prs.slides.add_slide(blank)
    add_rect(s, 0, 0, prs.slide_width, prs.slide_height, NAVY)
    add_rect(s, Inches(0.9), Inches(3.55), Inches(1.6), Inches(0.07), BLUE)
    tb = add_text(s, Inches(0.9), Inches(2.3), Inches(11.5), Inches(1.2),
                  deck_title or "演示文稿", 40, WHITE, bold=True)
    if subtitle:
        add_text(s, Inches(0.9), Inches(3.85), Inches(11.5), Inches(0.8), subtitle, 18, RGBColor(0xBD, 0xD7, 0xEE))
    meta_parts = [x for x in [author, datetime.date.today().strftime("%Y-%m-%d")] if x]
    if meta_parts:
        add_text(s, Inches(0.9), Inches(6.6), Inches(8), Inches(0.5), " · ".join(meta_parts), 13, GRAYC)
    # 右上角装饰方块组
    add_rect(s, Inches(12.35), Inches(0.55), Inches(0.45), Inches(0.45), BLUE)
    add_rect(s, Inches(11.7), Inches(0.55), Inches(0.45), Inches(0.45), RGBColor(0x2A, 0x4D, 0x7F))

    # ── 内容解析：slides JSON 优先，否则解析 markdown ──
    slides_spec = req.get("slides") or []
    if not slides_spec:
        slides_spec = []
        cur = None
        for raw in (req.get("content") or "").replace("\r\n", "\n").split("\n"):
            line = raw.rstrip()
            if line.startswith("# "):
                if cur:
                    slides_spec.append(cur)
                cur = {"title": line[2:].strip(), "bullets": [], "notes": []}
            elif cur is None:
                continue
            elif line.startswith("## "):
                cur["bullets"].append({"text": line[3:].strip(), "level": 0, "strong": True})
            elif line.startswith(">"):
                note = line.lstrip("> ").strip()
                if note:
                    cur["notes"].append(note)
            elif re.match(r"^\s*[-*+]\s+", line):
                ind = len(line) - len(line.lstrip())
                cur["bullets"].append({"text": re.sub(r"^\s*[-*+]\s+", "", line).strip(), "level": 1 if ind >= 2 else 0})
            elif line.strip():
                cur["bullets"].append({"text": line.strip(), "level": 0})
        if cur:
            slides_spec.append(cur)

    _progress(10.0, f"封面已生成，准备渲染 {len(slides_spec)} 页内容…")
    for idx, sp in enumerate(slides_spec):
        stitle = (sp.get("title") or "").strip() if isinstance(sp, dict) else str(sp)
        bullets = sp.get("bullets") or [] if isinstance(sp, dict) else []
        notes = sp.get("notes") or [] if isinstance(sp, dict) else []
        if isinstance(notes, str):
            notes = [notes]
        _progress(12.0 + 80.0 * idx / max(1, len(slides_spec)),
                  f"渲染第 {idx + 1}/{len(slides_spec)} 页幻灯片 · {stitle or '（分区页）'}")
        s = prs.slides.add_slide(blank)

        if not bullets:
            # 分区页：浅蓝背景 + 居中标题
            add_rect(s, 0, 0, prs.slide_width, prs.slide_height, RGBColor(0xF2, 0xF6, 0xFB))
            add_rect(s, Inches(6.17), Inches(3.15), Inches(1.0), Inches(0.06), BLUE)
            add_text(s, Inches(1.5), Inches(3.4), Inches(10.3), Inches(1.0), stitle, 30, NAVY, bold=True, align=PP_ALIGN.CENTER)
        else:
            # 标题 + 强调色条
            add_text(s, Inches(0.65), Inches(0.42), Inches(11.6), Inches(0.75), stitle, 24, NAVY, bold=True)
            add_rect(s, Inches(0.68), Inches(1.18), Inches(0.85), Inches(0.055), BLUE)
            body = s.shapes.add_textbox(Inches(0.9), Inches(1.65), Inches(11.5), Inches(5.1))
            tf = body.text_frame
            tf.word_wrap = True
            first = True
            for b in bullets:
                text = b if isinstance(b, str) else b.get("text", "")
                level = (b.get("level", 0) if isinstance(b, dict) else 0) or 0
                strong = b.get("strong", False) if isinstance(b, dict) else False
                p = tf.paragraphs[0] if first else tf.add_paragraph()
                first = False
                p.level = min(level, 4)
                p.space_after = Pt(10)
                r = p.add_run()
                r.text = ("▪ " if level == 0 else "– ") if not strong else ""
                r.text += text
                r.font.size = Pt(16 if level == 0 else 14)
                r.font.color.rgb = NAVY if strong else (DARK if level == 0 else RGBColor(0x59, 0x59, 0x59))
                r.font.bold = strong
                r.font.name = "微软雅黑"
        if notes:
            s.notes_slide.notes_text_frame.text = "\n".join(str(x) for x in notes)

        # 页脚：左侧文档名 / 右侧页码
        if deck_title:
            add_text(s, Inches(0.65), Inches(7.05), Inches(6), Inches(0.35), deck_title, 9, GRAYC)
        add_text(s, Inches(11.9), Inches(7.05), Inches(1.0), Inches(0.35),
                 f"{idx + 2}", 10, GRAYC, align=PP_ALIGN.RIGHT)

    out = req["path"]
    os.makedirs(os.path.dirname(os.path.abspath(out)), exist_ok=True)
    _progress(94.0, "正在保存 .pptx 文件…")
    prs.save(out)
    respond({
        "ok": True,
        "path": out,
        "bytes": os.path.getsize(out),
        "slides": len(prs.slides.__iter__.__self__._sldIdLst),
    })


# ══════════════════════════ op=docx_to_md ══════════════════════════

def docx_to_md(req):
    import docx
    from docx.document import Document as DocObj
    from docx.oxml.table import CT_Tbl
    from docx.oxml.text.paragraph import CT_P
    from docx.table import Table
    from docx.text.paragraph import Paragraph

    d = docx.Document(req["src"])
    _progress(15.0, "已打开 Word 文档，正在抽取段落与表格…")
    out = []
    _children = list(d.element.body.iterchildren())
    for _ci, child in enumerate(_children):
        if _children:
            _progress(15.0 + 75.0 * _ci / len(_children),
                      f"转换中 {_ci}/{len(_children)} 个块…")
        if isinstance(child, CT_P):
            p = Paragraph(child, d)
            text = p.text.strip()
            if not text:
                continue
            style = (p.style.name or "").lower()
            if style.startswith("heading"):
                try:
                    lvl = int(style.split()[-1])
                except ValueError:
                    lvl = 2
                out.append("#" * min(lvl, 6) + " " + text)
            elif "list bullet" in style:
                out.append("- " + text)
            elif "list number" in style:
                out.append("1. " + text)
            else:
                out.append(text)
            out.append("")
        elif isinstance(child, CT_Tbl):
            t = Table(child, d)
            if t.rows:
                head = [c.text.strip().replace("|", "\\|") for c in t.rows[0].cells]
                out.append("| " + " | ".join(head) + " |")
                out.append("|" + "---|" * len(head))
                for row in t.rows[1:]:
                    cells = [c.text.strip().replace("|", "\\|") for c in row.cells]
                    out.append("| " + " | ".join(cells) + " |")
                out.append("")
    md = "\n".join(out).strip() + "\n"
    dst = req.get("out") or re.sub(r"\.docx$", ".md", req["src"], flags=re.I)
    os.makedirs(os.path.dirname(os.path.abspath(dst)), exist_ok=True)
    with open(dst, "w", encoding="utf-8") as f:
        f.write(md)
    respond({"ok": True, "path": dst, "chars": len(md)})


# ══════════════════════════ pdf 操作 ══════════════════════════

def pdf_merge(req):
    from pypdf import PdfReader, PdfWriter
    w = PdfWriter()
    total = 0
    srcs = req["srcs"]
    _progress(10.0, f"准备合并 {len(srcs)} 个 PDF…")
    for si, src in enumerate(srcs):
        r = PdfReader(src)
        total += len(r.pages)
        for pg in r.pages:
            w.add_page(pg)
        _progress(10.0 + 80.0 * (si + 1) / len(srcs),
                  f"已合并 {si + 1}/{len(srcs)} 个文件 · 累计 {total} 页")
    out = req["out"]
    os.makedirs(os.path.dirname(os.path.abspath(out)), exist_ok=True)
    _progress(93.0, "正在写出合并后的 PDF…")
    with open(out, "wb") as f:
        w.write(f)
    respond({"ok": True, "path": out, "pages": total, "sources": len(req["srcs"])})


def pdf_split(req):
    from pypdf import PdfReader, PdfWriter
    r = PdfReader(req["src"])
    out_dir = req.get("out_dir") or os.path.dirname(os.path.abspath(req["src"]))
    os.makedirs(out_dir, exist_ok=True)
    stem = os.path.splitext(os.path.basename(req["src"]))[0]
    pages = []
    total_pages = len(r.pages)
    for i, pg in enumerate(r.pages):
        w = PdfWriter()
        w.add_page(pg)
        p = os.path.join(out_dir, f"{stem}_p{i + 1:03d}.pdf")
        with open(p, "wb") as f:
            w.write(f)
        pages.append(p)
        _progress(8.0 + 88.0 * (i + 1) / max(1, total_pages),
                  f"已拆分 {i + 1}/{total_pages} 页")
    respond({"ok": True, "count": len(pages), "pages": pages})


# ══════════════════════════ xlsx / csv 互转 ══════════════════════════

def xlsx_to_csv(req):
    import openpyxl
    import csv
    wb = openpyxl.load_workbook(req["src"], data_only=True, read_only=True)
    ws = wb[req["sheet"]] if req.get("sheet") else wb.active
    dst = req.get("out") or re.sub(r"\.xlsx$", ".csv", req["src"], flags=re.I)
    os.makedirs(os.path.dirname(os.path.abspath(dst)), exist_ok=True)
    rows = 0
    with open(dst, "w", encoding="utf-8-sig", newline="") as f:
        wr = csv.writer(f)
        for row in ws.iter_rows(values_only=True):
            wr.writerow(["" if v is None else v for v in row])
            rows += 1
            if rows % 200 == 0:
                _progress(15.0 + min(75.0, rows / 200.0), f"已导出 {rows} 行…")
    respond({"ok": True, "path": dst, "rows": rows, "sheet": ws.title})


def csv_to_xlsx(req):
    import openpyxl
    import csv
    dst = req.get("out") or re.sub(r"\.csv$", ".xlsx", req["src"], flags=re.I)
    os.makedirs(os.path.dirname(os.path.abspath(dst)), exist_ok=True)
    wb = openpyxl.Workbook()
    ws = wb.active
    ws.title = req.get("sheet") or "Sheet1"
    rows = 0
    with open(req["src"], "r", encoding="utf-8-sig", newline="") as f:
        for row in csv.reader(f):
            ws.append(row)
            rows += 1
            if rows % 200 == 0:
                _progress(15.0 + min(70.0, rows / 200.0), f"已写入 {rows} 行…")
    # 表头加粗
    if rows:
        from openpyxl.styles import Font, PatternFill
        for c in ws[1]:
            c.font = Font(bold=True, color="FFFFFF")
            c.fill = PatternFill("solid", fgColor=ACCENT)
    _progress(92.0, "正在保存 Excel 文件…")
    wb.save(dst)
    respond({"ok": True, "path": dst, "rows": rows})


# ══════════════════════════ 入口 ══════════════════════════

def main():
    try:
        req = json.loads(sys.stdin.read())
        op = req.get("op", "")
        if op == "docx":
            render_docx(req)
        elif op == "pptx":
            render_pptx(req)
        elif op == "docx_to_md":
            docx_to_md(req)
        elif op == "pdf_merge":
            pdf_merge(req)
        elif op == "pdf_split":
            pdf_split(req)
        elif op == "xlsx_to_csv":
            xlsx_to_csv(req)
        elif op == "csv_to_xlsx":
            csv_to_xlsx(req)
        else:
            respond({"ok": False, "error": f"未知操作: {op}"})
            sys.exit(1)
    except ImportError as e:
        respond({"ok": False, "error": f"缺少 Python 依赖库: {e}（pip install python-docx python-pptx pypdf openpyxl）"})
        sys.exit(1)
    except Exception as e:
        respond({"ok": False, "error": f"{type(e).__name__}: {e}"})
        sys.exit(1)


if __name__ == "__main__":
    main()
