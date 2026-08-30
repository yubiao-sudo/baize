import { marked } from "marked";

marked.setOptions({
  gfm: true,
  breaks: true,
});

/** 渲染 Markdown 为 HTML 字符串（后端内容可信：本地 Agent 自产） */
export function renderMd(src: string): string {
  try {
    return marked.parse(src, { async: false }) as string;
  } catch {
    return src
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/\n/g, "<br/>");
  }
}
