#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
read_document.py —— 白泽办公文档解析 sidecar

支持格式：pdf / docx / xlsx / pptx / csv / txt / md
能力：
  - 提取正文文本
  - 提取结构化表格（可导出为 CSV）
  - 提取内嵌图片（保存为本地文件）
  - 批量解析多个文件

协议：
  从 stdin 读取一行/一段 JSON 请求，向 stdout 输出 JSON 响应（ensure_ascii=False）。

请求结构：
{
  "paths":          ["F:/a.pdf", "F:/b.docx", ...],   # 必填，文件绝对路径列表
  "extract_text":   true,      # 是否提取文本（默认 true）
  "extract_tables": true,      # 是否提取表格（默认 true）
  "extract_images": true,      # 是否提取图片（默认 true）
  "export_csv":     false,     # 是否把表格导出为 CSV（默认 false）
  "csv_dir":        null,      # CSV 导出目录；缺省用源文件同目录
  "max_chars":      20000,     # 单文件文本截断上限
  "out_dir":        "..."      # 图片等产物的输出目录（由 Rust 侧创建）
}

响应结构：
{
  "ok": true,
  "count": 2,
  "files": [ { path, format, text, chars, truncated, stats, tables, tables_count,
               images, images_count, csv_files } ],
  "warnings": [...]
}
"""
import sys
import os
import json
import csv as csv_mod
import tempfile
import zipfile


def _try_import(name):
    try:
        return __import__(name)
    except Exception:
        return None


pdfplumber = _try_import("pdfplumber")
docx = _try_import("docx")
openpyxl = _try_import("openpyxl")
pptx = _try_import("pptx")
pypdf = _try_import("pypdf")

SUPPORTED = {".pdf", ".docx", ".xlsx", ".pptx", ".csv", ".txt", ".md", ".xls"}
IMG_EXTS = (".png", ".jpg", ".jpeg", ".gif", ".bmp", ".tiff", ".tif", ".svg", ".emf", ".wmf")


def ext_of(path):
    return os.path.splitext(path)[1].lower()


def warn(warnings, msg):
    warnings.append(msg)


# --------------------------------------------------------------------------- #
# 文本 / 表格 / 图片 提取
# --------------------------------------------------------------------------- #

def read_text_file(path, warnings):
    for enc in ("utf-8", "utf-8-sig", "gbk", "latin-1"):
        try:
            with open(path, "r", encoding=enc) as f:
                return f.read()
        except (UnicodeDecodeError, UnicodeError):
            continue
        except Exception as e:
            warn(warnings, f"读取文本文件失败: {e}")
            return ""
    with open(path, "rb") as f:
        return f.read().decode("utf-8", errors="replace")


def _table_from_rows(rows):
    # rows: list[list[str|None]] -> { columns, rows }
    if not rows:
        return {"columns": [], "rows": []}
    columns = ["" if c is None else str(c) for c in rows[0]]
    body = [[("" if c is None else str(c)) for c in r] for r in rows[1:]]
    return {"columns": columns, "rows": body}


def _save_zip_images(path, media_prefix, out_dir, stem, warnings):
    imgs = []
    try:
        with zipfile.ZipFile(path) as z:
            for name in z.namelist():
                low = name.lower()
                if not name.startswith(media_prefix) or name.endswith("/"):
                    continue
                if not low.endswith(IMG_EXTS):
                    continue
                try:
                    data = z.read(name)
                except Exception:
                    continue
                suffix = os.path.splitext(name)[1].lower() or ".bin"
                out = os.path.join(out_dir, f"{stem}_{len(imgs):03d}{suffix}")
                with open(out, "wb") as f:
                    f.write(data)
                imgs.append({"index": len(imgs), "path": out, "ext": suffix, "size": len(data)})
    except Exception as e:
        warn(warnings, f"提取图片失败: {e}")
    return imgs


def _save_pdf_images(path, out_dir, stem, warnings):
    imgs = []
    if pypdf is None:
        warn(warnings, "提取 PDF 内嵌图片需要 pypdf，未安装，已跳过")
        return imgs
    try:
        reader = pypdf.PdfReader(path)
        for page in reader.pages:
            try:
                for img in page.images:
                    data = img.data
                    suffix = os.path.splitext(img.name or "")[1].lower() or ".png"
                    out = os.path.join(out_dir, f"{stem}_{len(imgs):03d}{suffix}")
                    with open(out, "wb") as f:
                        f.write(data)
                    imgs.append({"index": len(imgs), "path": out, "ext": suffix, "size": len(data)})
            except Exception:
                continue
    except Exception as e:
        warn(warnings, f"提取 PDF 图片失败: {e}")
    return imgs


def extract_pdf(path, out_dir, stem, opts, warnings):
    ok_text = opts.get("extract_text", True)
    ok_tables = opts.get("extract_tables", True)
    ok_images = opts.get("extract_images", True)
    text, tables, imgs, stats = "", [], [], {}
    if pdfplumber is None:
        warn(warnings, "缺少依赖 pdfplumber（pip install pdfplumber），无法解析 PDF")
        if ok_images and pypdf is not None:
            imgs = _save_pdf_images(path, out_dir, stem, warnings)
        return text, tables, imgs, stats
    with pdfplumber.open(path) as pdf:
        pages = pdf.pages or []
        stats["pages"] = len(pages)
        for pi, page in enumerate(pages):
            if ok_text:
                t = page.extract_text() or ""
                if t:
                    text += (t + "\n\n")
            if ok_tables:
                extracted = page.extract_tables() or []
                for tbl in extracted:
                    rows = _table_from_rows(tbl)
                    # 跳过全空表
                    if rows["columns"] or any(any(c for c in r) for r in rows["rows"]):
                        tables.append(rows)
    if ok_images:
        imgs = _save_pdf_images(path, out_dir, stem, warnings)
    return text, tables, imgs, stats


def extract_docx(path, out_dir, stem, opts, warnings):
    ok_text = opts.get("extract_text", True)
    ok_tables = opts.get("extract_tables", True)
    ok_images = opts.get("extract_images", True)
    text, tables, imgs, stats = "", [], [], {}
    if docx is None:
        warn(warnings, "缺少依赖 python-docx（pip install python-docx），无法解析 Word")
        if ok_images:
            imgs = _save_zip_images(path, "word/media/", out_dir, stem, warnings)
        return text, tables, imgs, stats
    d = docx.Document(path)
    if ok_text:
        parts = [p.text for p in d.paragraphs]
        # 表格文字也并入正文（便于上下文阅读）
        for tbl in d.tables:
            for row in tbl.rows:
                parts.append(" | ".join(cell.text for cell in row.cells))
        text = "\n".join(parts)
    stats["paragraphs"] = len(d.paragraphs)
    if ok_tables:
        for tbl in d.tables:
            rows = [[cell.text for cell in row.cells] for row in tbl.rows]
            tables.append(_table_from_rows(rows))
    if ok_images:
        imgs = _save_zip_images(path, "word/media/", out_dir, stem, warnings)
    return text, tables, imgs, stats


def extract_xlsx(path, out_dir, stem, opts, warnings):
    ok_text = opts.get("extract_text", True)
    ok_tables = opts.get("extract_tables", True)
    ok_images = opts.get("extract_images", True)
    text, tables, imgs, stats = "", [], [], {}
    if openpyxl is None:
        warn(warnings, "缺少依赖 openpyxl（pip install openpyxl），无法解析 Excel")
        if ok_images:
            imgs = _save_zip_images(path, "xl/media/", out_dir, stem, warnings)
        return text, tables, imgs, stats
    wb = openpyxl.load_workbook(path, data_only=True, read_only=True)
    stats["sheets"] = wb.sheetnames
    for ws in wb.worksheets:
        rows = [list(r) for r in ws.iter_rows(values_only=True)]
        if not rows:
            continue
        if ok_tables:
            tables.append(_table_from_rows(rows))
        if ok_text:
            for r in rows:
                cells = ["" if c is None else str(c) for c in r]
                text += " | ".join(cells) + "\n"
            text += "\n"
    wb.close()
    if ok_images:
        imgs = _save_zip_images(path, "xl/media/", out_dir, stem, warnings)
    return text, tables, imgs, stats


def _iter_shape_text(shape, buf):
    if shape.has_text_frame:
        for para in shape.text_frame.paragraphs:
            line = "".join(run.text for run in para.runs)
            buf.append(line)
    if shape.has_table:
        tbl = shape.table
        for row in tbl.rows:
            buf.append(" | ".join(cell.text for cell in row.cells))
    # 组合形状递归
    if shape.shape_type is not None and str(getattr(shape.shape_type, "name", "")) == "GROUP":
        try:
            for sub in shape.shapes:
                _iter_shape_text(sub, buf)
        except Exception:
            pass


def extract_pptx(path, out_dir, stem, opts, warnings):
    ok_text = opts.get("extract_text", True)
    ok_tables = opts.get("extract_tables", True)
    ok_images = opts.get("extract_images", True)
    text, tables, imgs, stats = "", [], [], {}
    if pptx is None:
        warn(warnings, "缺少依赖 python-pptx（pip install python-pptx），无法解析 PPT")
        if ok_images:
            imgs = _save_zip_images(path, "ppt/media/", out_dir, stem, warnings)
        return text, tables, imgs, stats
    prs = pptx.Presentation(path)
    stats["slides"] = len(prs.slides)
    if ok_text or ok_tables:
        for si, slide in enumerate(prs.slides):
            buf = []
            for shape in slide.shapes:
                if ok_text:
                    _iter_shape_text(shape, buf)
                if ok_tables and shape.has_table:
                    tbl = shape.table
                    rows = [[cell.text for cell in row.cells] for row in tbl.rows]
                    tables.append(_table_from_rows(rows))
            if buf:
                text += "\n".join(buf) + "\n\n"
    if ok_images:
        imgs = _save_zip_images(path, "ppt/media/", out_dir, stem, warnings)
    return text, tables, imgs, stats


# --------------------------------------------------------------------------- #
# CSV 导出
# --------------------------------------------------------------------------- #

def _export_csv(path, stem, tables, csv_dir, out_dir, warnings):
    files = []
    if not tables:
        return files
    target_dir = csv_dir or os.path.dirname(path) or out_dir
    try:
        os.makedirs(target_dir, exist_ok=True)
    except Exception:
        target_dir = out_dir
    for i, tbl in enumerate(tables):
        fname = f"{stem}_table{i}.csv"
        outp = os.path.join(target_dir, fname)
        try:
            with open(outp, "w", newline="", encoding="utf-8-sig") as f:
                w = csv_mod.writer(f)
                if tbl.get("columns"):
                    w.writerow(tbl["columns"])
                for row in tbl.get("rows", []):
                    w.writerow(row)
            files.append(outp)
        except Exception as e:
            warn(warnings, f"导出 CSV 失败 {fname}: {e}")
    return files


def process_file(path, opts, out_dir, warnings):
    fmt = ext_of(path).lstrip(".")
    stem = os.path.splitext(os.path.basename(path))[0]
    res = {
        "path": path,
        "format": fmt,
        "text": "",
        "chars": 0,
        "truncated": False,
        "stats": {},
        "tables": [],
        "tables_count": 0,
        "images": [],
        "images_count": 0,
        "csv_files": [],
    }
    # 图片输出子目录（避免污染 out_dir 根）
    img_dir = os.path.join(out_dir, "images")
    try:
        os.makedirs(img_dir, exist_ok=True)
    except Exception:
        img_dir = out_dir

    if fmt == "pdf":
        text, tables, imgs, stats = extract_pdf(path, img_dir, stem, opts, warnings)
    elif fmt == "docx":
        text, tables, imgs, stats = extract_docx(path, img_dir, stem, opts, warnings)
    elif fmt == "xlsx":
        text, tables, imgs, stats = extract_xlsx(path, img_dir, stem, opts, warnings)
    elif fmt == "pptx":
        text, tables, imgs, stats = extract_pptx(path, img_dir, stem, opts, warnings)
    elif fmt in ("csv", "txt", "md"):
        text = read_text_file(path, warnings)
        tables, imgs, stats = [], [], {}
        if fmt == "csv":
            # 简单把 CSV 也转成结构化单表（与文本并轨）
            try:
                with open(path, "r", encoding="utf-8-sig", newline="") as f:
                    reader = csv_mod.reader(f)
                    rows = [r for r in reader]
                if rows and opts.get("extract_tables", True):
                    tables = [_table_from_rows(rows)]
            except Exception as e:
                warn(warnings, f"读取 CSV 表格失败: {e}")
    else:
        text, tables, imgs, stats = "", [], [], {}

    max_chars = int(opts.get("max_chars", 20000) or 0)
    if max_chars > 0 and len(text) > max_chars:
        text = text[:max_chars]
        res["truncated"] = True

    res.update({
        "text": text,
        "chars": len(text),
        "stats": stats,
        "tables": tables,
        "tables_count": len(tables),
        "images": imgs,
        "images_count": len(imgs),
    })

    if opts.get("export_csv"):
        res["csv_files"] = _export_csv(path, stem, tables, opts.get("csv_dir"), out_dir, warnings)

    return res


def main():
    raw = sys.stdin.read()
    if not raw.strip():
        json.dump({"ok": False, "error": "empty request"}, sys.stdout, ensure_ascii=False)
        return
    try:
        req = json.loads(raw)
    except Exception as e:
        json.dump({"ok": False, "error": f"请求 JSON 解析失败: {e}"}, sys.stdout, ensure_ascii=False)
        return

    paths = req.get("paths") or ([req["path"]] if req.get("path") else [])
    out_dir = req.get("out_dir") or os.path.join(os.path.dirname(__file__) or tempfile.gettempdir(), "out")
    opts = {
        "extract_text": req.get("extract_text", True),
        "extract_tables": req.get("extract_tables", True),
        "extract_images": req.get("extract_images", True),
        "export_csv": req.get("export_csv", False),
        "csv_dir": req.get("csv_dir"),
        "max_chars": req.get("max_chars", 20000),
    }
    try:
        os.makedirs(out_dir, exist_ok=True)
    except Exception:
        pass

    warnings = []
    files = []
    for p in paths:
        try:
            files.append(process_file(p, opts, out_dir, warnings))
        except Exception as e:
            warnings.append(f"{p} 解析失败: {e}")

    json.dump({"ok": True, "count": len(files), "files": files, "warnings": warnings},
              sys.stdout, ensure_ascii=False)


if __name__ == "__main__":
    main()