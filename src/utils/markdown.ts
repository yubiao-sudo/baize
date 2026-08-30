import { Marked } from "marked";
import { previewHtml } from "../api";

/**
 * 轻量 Markdown 渲染（零新增依赖，复用已装的 marked）。
 *  - 代码块自动语法高亮（自写正则 token 高亮器）
 *  - 支持白泽的 ==重点== 高亮约定，渲染为 <mark class="hl">
 */

// ---------- 代码块复制 / HTML 预览（供 dangerouslySetInnerHTML 内联 onclick 调用） ----------

function copyCodeBlock(btn: HTMLElement) {
  const codeEl = btn.closest(".codeblock")?.querySelector("code");
  const text = codeEl?.textContent ?? "";
  const done = (msg: string) => {
    btn.textContent = msg;
    window.setTimeout(() => {
      btn.textContent = "复制";
    }, 1500);
  };
  // WebView 里 navigator.clipboard 可能因权限失败，降级到 execCommand
  const fallback = () => {
    try {
      const ta = document.createElement("textarea");
      ta.value = text;
      ta.setAttribute("readonly", "");
      ta.style.position = "fixed";
      ta.style.opacity = "0";
      document.body.appendChild(ta);
      ta.select();
      const ok = document.execCommand("copy");
      document.body.removeChild(ta);
      done(ok ? "已复制" : "复制失败");
    } catch {
      done("复制失败");
    }
  };
  if (navigator.clipboard?.writeText) {
    navigator.clipboard.writeText(text).then(() => done("已复制")).catch(fallback);
  } else {
    fallback();
  }
}

function previewCodeBlock(btn: HTMLElement) {
  const codeEl = btn.closest(".codeblock")?.querySelector("code");
  const html = codeEl?.textContent ?? "";
  void previewHtml(html);
}

if (typeof window !== "undefined") {
  (window as unknown as Record<string, unknown>).__baizeCodeCopy = copyCodeBlock;
  (window as unknown as Record<string, unknown>).__baizePreviewHtml = previewCodeBlock;
}

// ---------- HTML 转义 ----------
export function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function escapeRegex(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

// ---------- 关键字符号表（按语言族） ----------
const KEYWORDS: Record<string, string> = {
  // C 系（JS/TS/C/C++/Java/C#/Go/PHP/Swift/JSON...）
  default:
    "abstract as async await break case catch class const continue debugger default delete do else enum export extends false finally for from function get if implements import in instanceof interface let new null of package private protected public return set static super switch this throw true try type typeof undefined var void while with yield",
  python:
    "and as assert async await break class continue def del elif else except False finally for from global if import in is lambda None nonlocal not or pass raise return True try while with yield",
  rust:
    "as async await break const continue crate dyn else enum extern false fn for if impl in let loop match mod move mut pub ref return self Self static struct super trait true type unsafe use where while None Some Ok Err Box Vec String Option Result",
  sql: "add all alter and as asc by case count create delete desc distinct drop else end exists from group having in index inner insert into is join left like limit not null on or order outer primary right select set sum table then union update values when where",
  shell:
    "if then else elif fi for while do done case esac function export local readonly return exit echo source alias set unset shift in",
};

function isLang(lang: string, ...names: string[]): boolean {
  return names.includes(lang.toLowerCase());
}

function keywordSet(lang: string): string {
  if (isLang(lang, "python", "py", "py3")) return KEYWORDS.python;
  if (isLang(lang, "rust", "rs")) return KEYWORDS.rust;
  if (isLang(lang, "sql", "mysql", "postgresql", "pgsql", "sqlite")) return KEYWORDS.sql;
  if (isLang(lang, "bash", "sh", "shell", "zsh", "powershell", "ps1", "bat", "cmd")) return KEYWORDS.shell;
  return KEYWORDS.default;
}

// 用 # 做行注释的语言
function hashComment(lang: string): boolean {
  return isLang(
    lang,
    "python", "py", "py3", "bash", "sh", "shell", "zsh", "powershell", "ps1",
    "ruby", "yaml", "yml", "toml", "makefile", "r", "perl",
  );
}

// 用 -- 做行注释的语言（SQL / Lua / Haskell）
function dashComment(lang: string): boolean {
  return isLang(lang, "sql", "mysql", "postgresql", "pgsql", "sqlite", "lua", "haskell");
}

/**
 * 轻量语法高亮：HTML 转义 → 依次保护字符串/注释/数字/关键字 → 还原占位。
 * 占位符形如 ZHQ<序号>ZHQ，数字两侧均为字母，确保后续 `\b` 正则不会误伤。
 */
export function highlightCode(code: string, lang: string): string {
  const esc = escapeHtml(code);
  const tokens: string[] = [];
  const push = (html: string) => {
    const i = tokens.length;
    tokens.push(html);
    return `ZHQ${i}ZHQ`;
  };

  let w = esc;

  // 1) 字符串（双引号 / 单引号 / 反引号，含转义）
  w = w.replace(
    /("(?:\\.|[^"\\\n])*"|'(?:\\.|[^'\\\n])*'|`(?:\\.|[^`\\\n])*`)/g,
    (m) => push(`<span class="tok-str">${m}</span>`),
  );

  // 2) 注释
  if (hashComment(lang)) {
    w = w.replace(/(#[^\n]*)/g, (m) => push(`<span class="tok-cmt">${m}</span>`));
  } else if (dashComment(lang)) {
    w = w.replace(/(--[^\n]*)/g, (m) => push(`<span class="tok-cmt">${m}</span>`));
  } else {
    w = w.replace(/(\/\/[^\n]*|\/\*[\s\S]*?\*\/)/g, (m) => push(`<span class="tok-cmt">${m}</span>`));
  }

  // 3) 数字（含十六进制 / 小数 / 科学计数法）
  w = w.replace(
    /\b(0[xXbBoO][0-9a-fA-F]+|\d+(?:\.\d+)?(?:[eE][+-]?\d+)?)\b/g,
    (m) => push(`<span class="tok-num">${m}</span>`),
  );

  // 4) 关键字
  const kw = keywordSet(lang);
  const re = new RegExp(`\\b(?:${kw.split(/\s+/).map(escapeRegex).join("|")})\\b`, "g");
  w = w.replace(re, (m) => push(`<span class="tok-kw">${m}</span>`));

  // 5) 还原占位
  return w.replace(/ZHQ(\d+)ZHQ/g, (_m, i) => tokens[Number(i)]);
}

// ---------- Markdown 实例（含代码高亮） ----------
const md = new Marked({ gfm: true, breaks: true });
md.use({
  renderer: {
    code({ text, lang }) {
      const l = (lang ?? "").trim().split(/\s+/)[0] ?? "";
      const body = highlightCode(text, l);
      const cls = l ? ` class="language-${escapeHtml(l)}"` : "";
      // 识别完整 HTML 页面代码（lang=html，或未标注语言但以 <!DOCTYPE html>/<html 开头）
      const isHtml =
        /^html$/i.test(l) ||
        (l === "" && (/^\s*<!doctype\s+html/i.test(text) || /^\s*<html[\s>]/i.test(text)));
      const langLabel = escapeHtml(l || "文本");
      const copyBtn =
        '<button type="button" class="codeblock-btn" title="复制代码" onclick="__baizeCodeCopy(this)">复制</button>';
      const previewBtn = isHtml
        ? '<button type="button" class="codeblock-btn codeblock-preview" title="在内置白泽浏览器中预览" onclick="__baizePreviewHtml(this)">预览</button>'
        : "";
      return (
        '<div class="codeblock">' +
        `<div class="codeblock-head"><span class="codeblock-lang">${langLabel}</span>` +
        `<span class="codeblock-actions">${copyBtn}${previewBtn}</span></div>` +
        `<pre><code${cls}>${body}</code></pre>` +
        "</div>"
      );
    },
  },
});

/**
 * 渲染一段消息文本为 HTML。
 * 先保护 ==重点== 标记（避免被 markdown 打断），解析后再还原为 <mark class="hl">。
 */
export function renderMarkdown(text: string): string {
  if (!text) return "";
  const marks: string[] = [];
  const src = text.replace(/==([^=\n]+)==/g, (_m, inner: string) => {
    const i = marks.length;
    marks.push(escapeHtml(inner));
    return `ZMH${i}ZMH`;
  });

  let html: string;
  try {
    html = md.parse(src) as string;
  } catch {
    html = escapeHtml(text);
  }
  return html.replace(/ZMH(\d+)ZMH/g, (_m, i) => `<mark class="hl">${marks[Number(i)]}</mark>`);
}