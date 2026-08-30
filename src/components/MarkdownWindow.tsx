import { useEffect, useMemo, useRef, useState } from "react";
import { marked } from "marked";
import DOMPurify from "dompurify";
import {
  closeMarkdownTab,
  getMarkdownState,
  onMarkdownUpdate,
  saveMarkdown,
  switchMarkdownTab,
} from "../api";
import type { MarkdownDoc } from "../types";

// 与会话区 Typewriter 一致的逐字节奏
const CHUNK = 3;
const INTERVAL = 20;
// 渲染节流：打字期间每 150ms 才重新 parse 一次（打字速度不变，长文档不再每 20ms 全量 parse）
const PARSE_EVERY = 150;

type TocItem = { level: number; text: string; id: string };

/**
 * 内置 Markdown 文档窗口（独立窗口，位于主窗口右侧）
 *  - 多标签页：每次新生成的文档写入独立标签页，不覆盖之前的文档
 *  - 活跃文档逐字显示（与会话区回复效果一致）；追加续写时继续打字，覆盖/新开时重新打字
 *  - 渲染节流：打字期间 markdown 每 150ms 增量刷新一次，结束后全量渲染
 *  - 智能跟随滚动：仅当用户位于底部时才自动滚动，上翻阅读不被拽回
 *  - 输出经 DOMPurify 消毒，杜绝 AI 生成内容里的脚本注入
 *  - 实用功能：复制全文 / 另存为 / 打印(导出 PDF) / 目录跳转
 */
export default function MarkdownWindow() {
  const [docs, setDocs] = useState<MarkdownDoc[]>([]);
  const [activeId, setActiveId] = useState("");
  const [shown, setShown] = useState(0); // 已揭示字符数（计数器用）
  const [renderedChars, setRenderedChars] = useState(0); // 已渲染进 markdown 的字符数（≤ shown）
  const [savedMsg, setSavedMsg] = useState("");
  const [toc, setToc] = useState<TocItem[]>([]);
  const [showToc, setShowToc] = useState(false);
  const followRef = useRef(true);
  const bodyRef = useRef<HTMLDivElement>(null);
  const prevRef = useRef<Record<string, string>>({});

  const activeDoc = docs.find((d) => d.id === activeId) ?? null;
  const target = activeDoc?.content ?? "";
  const done = shown >= target.length;

  const resolveActiveId = (ds: MarkdownDoc[]) => {
    const active = ds.find((d) => d.active);
    return active ? active.id : ds.length ? ds[ds.length - 1].id : "";
  };

  useEffect(() => {
    getMarkdownState().then((s) => {
      const ds = s.docs ?? [];
      ds.forEach((d) => (prevRef.current[d.id] = d.content));
      setDocs(ds);
      setActiveId(resolveActiveId(ds));
      setShown(0); // 从头逐字显示（按需创建的窗口，内容即刚写入的）
      setRenderedChars(0);
    });
    const un = onMarkdownUpdate((s) => {
      const ds = s.docs ?? [];
      const id = resolveActiveId(ds);
      const content = ds.find((d) => d.id === id)?.content ?? "";
      const prev = prevRef.current[id];
      const isAppend = prev !== undefined && content.startsWith(prev);
      ds.forEach((d) => (prevRef.current[d.id] = d.content));
      setDocs(ds);
      setActiveId(id);
      if (!isAppend) {
        setShown(0); // 覆盖/新开 → 重新逐字
        setRenderedChars(0);
      }
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  // 保存文档：弹出另存为对话框（保存当前活跃标签页）
  const handleSave = async () => {
    if (!activeDoc) return;
    try {
      const path = await saveMarkdown(activeDoc.title || "白泽文档", activeDoc.content);
      if (path) {
        setSavedMsg(`已保存：${path}`);
        window.setTimeout(() => setSavedMsg(""), 4000);
      }
    } catch (e) {
      setSavedMsg(`保存失败：${String(e)}`);
      window.setTimeout(() => setSavedMsg(""), 4000);
    }
  };

  // 复制全文（Markdown 原文进剪贴板）
  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(activeDoc?.content ?? "");
      setSavedMsg("已复制全文");
    } catch {
      setSavedMsg("复制失败");
    }
    window.setTimeout(() => setSavedMsg(""), 2500);
  };

  // 打印 / 导出 PDF：文档窗口独立打印（打印视图隐藏工具栏，见 index.css @media print）
  const handlePrint = () => window.print();

  // 切换标签页：立即全量显示，不再重播打字
  const onSwitch = (id: string) => {
    if (id === activeId) return;
    setActiveId(id);
    const len = docs.find((d) => d.id === id)?.content.length ?? 0;
    setShown(len);
    setRenderedChars(len);
    void switchMarkdownTab(id);
  };

  // 关闭标签页
  const onClose = (id: string) => {
    const next = docs.filter((d) => d.id !== id);
    if (activeId === id) {
      const last = next[next.length - 1];
      setActiveId(last ? last.id : "");
      setShown(last ? last.content.length : 0);
      setRenderedChars(last ? last.content.length : 0);
    }
    setDocs(next);
    void closeMarkdownTab(id);
  };

  // 逐字推进 + 渲染节流（requestAnimationFrame 对齐刷新率；推进节奏不变，
  // 但 setShown/markdown 重渲染最多每 60/150ms 一次，长文档不再每 20ms 全量 parse）
  useEffect(() => {
    if (shown >= target.length && renderedChars >= target.length) return;
    let raf = 0;
    let n = shown;
    let lastAdv = performance.now();
    let lastRender = 0;
    const step = (now: number) => {
      if (now - lastAdv >= INTERVAL) {
        lastAdv = now;
        n = Math.min(target.length, n + CHUNK);
      }
      const allRevealed = n >= target.length;
      const shouldRender =
        (now - lastRender >= PARSE_EVERY && n > renderedChars) || allRevealed;
      if (shouldRender) {
        lastRender = now;
        setShown(n);
        setRenderedChars(n);
      }
      if (!allRevealed || renderedChars < n) {
        raf = requestAnimationFrame(step);
      }
    };
    raf = requestAnimationFrame(step);
    return () => cancelAnimationFrame(raf);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [target]);

  // 智能跟随滚动：只有用户本来就在底部附近时才跟随，上翻阅读不被打断
  const onScroll = () => {
    const el = bodyRef.current;
    if (!el) return;
    followRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 90;
  };
  useEffect(() => {
    const el = bodyRef.current;
    if (el && followRef.current) el.scrollTop = el.scrollHeight;
  }, [renderedChars]);

  // markdown 渲染（节流后的切片 + DOMPurify 消毒）
  const html = useMemo(() => {
    const md = target.slice(0, renderedChars);
    if (!md) return "";
    try {
      return DOMPurify.sanitize(marked.parse(md) as string);
    } catch {
      return md;
    }
  }, [target, renderedChars]);

  // 目录：渲染完成后从 DOM 提取 h1-h3（打字期间不折腾）
  useEffect(() => {
    if (!done || !bodyRef.current) return;
    const hs = Array.from(bodyRef.current.querySelectorAll("h1,h2,h3"));
    const items: TocItem[] = hs.map((h, i) => {
      h.id = `doc-h-${i}`;
      return { level: Number(h.tagName[1]), text: (h.textContent || "").slice(0, 28), id: h.id };
    });
    setToc(items);
  }, [done, html]);

  const jumpTo = (id: string) => {
    document.getElementById(id)?.scrollIntoView({ behavior: "smooth", block: "start" });
    setShowToc(false);
  };

  return (
    <div className="side-window">
      <div className="browser-tabbar">
        {docs.map((d) => (
          <div
            key={d.id}
            className={`browser-tab ${d.id === activeId ? "active" : ""}`}
            onClick={() => onSwitch(d.id)}
          >
            <span className="browser-tab-icon">📄</span>
            <span className="browser-tab-title">{d.title || "白泽文档"}</span>
            <button
              className="browser-tab-close"
              title="关闭标签页"
              onClick={(e) => {
                e.stopPropagation();
                onClose(d.id);
              }}
            >
              ×
            </button>
          </div>
        ))}
      </div>

      <div className="side-toolbar">
        <span className="side-title">📄 {activeDoc?.title || "白泽文档"}</span>
        <span className="side-tag">
          {done ? `${target.length} 字` : `${shown}/${target.length}`}
        </span>
        <span className="side-spacer" />
        {savedMsg && <span className="side-saved">{savedMsg}</span>}
        {toc.length >= 2 && (
          <button
            className="side-btn"
            onClick={() => setShowToc((v) => !v)}
            title="目录导航"
          >
            ☰ 目录
          </button>
        )}
        <button className="side-btn" onClick={() => void handleCopy()} title="复制 Markdown 全文">
          复制
        </button>
        <button className="side-btn" onClick={() => void handleSave()} title="另存为文件">
          💾
        </button>
        <button className="side-btn" onClick={handlePrint} title="打印 / 导出 PDF">
          🖨
        </button>
      </div>

      {showToc && toc.length >= 2 && (
        <div className="doc-toc">
          {toc.map((t) => (
            <div
              key={t.id}
              className={`doc-toc-item doc-toc-l${t.level}`}
              onClick={() => jumpTo(t.id)}
            >
              {t.text}
            </div>
          ))}
        </div>
      )}

      <div className="side-content markdown-body" ref={bodyRef} onScroll={onScroll}>
        {docs.length === 0 ? (
          <div className="browser-empty">
            让白泽在这里写文档吧
            <br />
            例如：「写一份测试报告」·「总结这周的工作」
          </div>
        ) : (
          <>
            <div dangerouslySetInnerHTML={{ __html: html }} />
            {!done && <span className="caret">▍</span>}
          </>
        )}
      </div>
    </div>
  );
}
