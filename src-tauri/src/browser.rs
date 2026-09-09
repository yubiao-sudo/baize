use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use regex::Regex;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::tools::{PermissionClass, Tool};

/// 搜索结果项（结构化，供白泽感知）
#[derive(Clone, serde::Serialize)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub summary: String,
    /// 网站图标：优先来自搜索引擎结果页原生解析（Bing wr_fav），空则前端直接加载站点 /favicon.ico
    #[serde(default)]
    pub icon: Option<String>,
}

/// 浏览器标签页
#[derive(Clone, serde::Serialize)]
pub struct Tab {
    pub id: String,
    /// html | markdown | text | video | url
    pub kind: String,
    pub title: String,
    pub content: String,
    /// 是否为当前激活的标签页
    pub active: bool,
}

/// 内置浏览器窗口的共享状态（白泽可控制/感知）
#[derive(Default, Clone, serde::Serialize)]
pub struct BrowserState {
    /// 当前 iframe 地址（html 模式时为空）
    pub url: String,
    /// HTML 模式内容（render_html / 搜索结果页），优先于 url 渲染
    pub html: Option<String>,
    pub title: String,
    /// 结构化搜索结果（供白泽感知，browser_get 时返回）
    pub results: Option<Vec<SearchResult>>,
    /// 标签页列表（每个内容一个标签页）
    pub tabs: Vec<Tab>,
}

impl BrowserState {
    pub fn snapshot(&self) -> Value {
        json!({
            "url": self.url,
            "html": self.html,
            "title": self.title,
            "results": self.results,
            "tabs": self.tabs,
        })
    }

    /// 打开一个新标签页，返回标签页 id
    pub fn open_tab(&mut self, kind: &str, title: &str, content: &str) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        // 新标签页激活，其余置非激活
        for t in &mut self.tabs {
            t.active = false;
        }
        self.tabs.push(Tab {
            id: id.clone(),
            kind: kind.to_string(),
            title: title.to_string(),
            content: content.to_string(),
            active: true,
        });
        id
    }

    /// 激活指定标签页
    pub fn set_active_tab(&mut self, id: &str) {
        for t in &mut self.tabs {
            t.active = t.id == id;
        }
    }

    /// 关闭指定标签页
    pub fn close_tab(&mut self, id: &str) {
        let was_active = self.tabs.iter().any(|t| t.id == id && t.active);
        self.tabs.retain(|t| t.id != id);
        if was_active {
            if let Some(last) = self.tabs.last_mut() {
                last.active = true;
            }
        }
    }
}

/// 把最新状态推送给浏览器窗口（按需调出窗口）
fn emit_update(app: &AppHandle, state: &Arc<Mutex<BrowserState>>) {
    crate::windows::ensure_browser_window(app);
    let snap = state.lock().unwrap().snapshot();
    let _ = app.emit_to("browser", "browser-update", &snap);
}

/// 简易 URL 编码（UTF-8 字节 → percent）
fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// percent 解码（%XX → 字节，+ → 空格）
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// 去除 HTML 标签并解码常见实体
fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&nbsp;", " ")
        .trim()
        .to_string()
}

/// HTML 转义
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// DuckDuckGo 跳转链接 → 真实目标 URL
fn decode_uddg(href: &str) -> String {
    if let Some(idx) = href.find("uddg=") {
        let val = &href[idx + 5..];
        let end = val.find('&').unwrap_or(val.len());
        let decoded = percent_decode(&val[..end]);
        if !decoded.is_empty() {
            return decoded;
        }
    }
    href.to_string()
}

/// 解析 DuckDuckGo html 版结果（标题 + 链接 + 摘要）
fn parse_duckduckgo(html: &str) -> Vec<SearchResult> {
    let re_link =
        Regex::new(r#"<a[^>]*class="result__a"[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#).unwrap();
    let re_snip = Regex::new(r#"<a[^>]*class="result__snippet"[^>]*>(.*?)</a>"#).unwrap();

    let mut links: Vec<(String, String)> = Vec::new();
    for cap in re_link.captures_iter(html) {
        let href = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let title = cap.get(2).map(|m| m.as_str()).unwrap_or("");
        links.push((decode_uddg(href), strip_tags(title)));
    }
    let summaries: Vec<String> = re_snip
        .captures_iter(html)
        .map(|c| strip_tags(c.get(1).map(|m| m.as_str()).unwrap_or("")))
        .collect();

    links
        .into_iter()
        .zip(summaries.into_iter().chain(std::iter::repeat(String::new())))
        .take(10)
        .map(|((url, title), summary)| SearchResult { title, url, summary, icon: None })
        .collect()
}

/// 域名首字母头像的渐变色板（按域名哈希取色，作为 favicon 加载失败时的最终兜底）
const AVATAR_GRADIENTS: &[(&str, &str)] = &[
    ("#6366f1", "#8b5cf6"),
    ("#0ea5e9", "#22d3ee"),
    ("#10b981", "#34d399"),
    ("#f59e0b", "#fbbf24"),
    ("#ef4444", "#f97316"),
    ("#ec4899", "#f472b6"),
    ("#8b5cf6", "#a78bfa"),
    ("#14b8a6", "#2dd4bf"),
    ("#f97316", "#fb923c"),
    ("#22c55e", "#84cc16"),
];

/// 从 URL 提取展示域名（去协议/路径/www.）
fn display_domain(url: &str) -> String {
    let host = url
        .split("://")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or(url);
    host.strip_prefix("www.").unwrap_or(host).to_string()
}

/// 生成精美的深色主题搜索结果页：
/// 渐变头部 + 引擎/条数/时间徽章 + 卡片式结果列表（域名头像、交错入场动画、悬停辉光）
fn build_results_html(query: &str, engine: &str, results: &[SearchResult]) -> String {
    let mut items = String::new();
    if results.is_empty() {
        items.push_str(
            "<div class='empty'><div class='ico'>🕸️</div><div class='msg'>未获取到搜索结果</div>\
             <div class='hint'>可能被搜索服务限流，请稍后重试或换个关键词</div></div>",
        );
    }
    for (i, r) in results.iter().enumerate() {
        let domain = display_domain(&r.url);
        // 图标加载失败时的文字兜底：显示站点主域名（如 baidu.com → baidu），最多 7 字符
        let site_name: String = domain
            .split('.')
            .next()
            .unwrap_or("?")
            .chars()
            .take(7)
            .collect();
        let hash: usize = domain.bytes().map(|b| b as usize).sum();
        let (c1, c2) = AVATAR_GRADIENTS[hash % AVATAR_GRADIENTS.len()];
        let summary_html = if r.summary.is_empty() {
            String::new()
        } else {
            format!("<div class='s'>{}</div>", escape_html(&r.summary))
        };
        // 图标来源：搜索引擎结果页原生解析 → 站点自身 /favicon.ico（WebView 加载，走系统网络与缓存）
        let img_src = r
            .icon
            .clone()
            .unwrap_or_else(|| format!("https://{domain}/favicon.ico"));
        items.push_str(&format!(
            "<a class='item' style='animation-delay:{}ms' href='{}' target='_blank' rel='noopener'>\
             <span class='num'>{}</span>\
             <span class='ava' style='background:linear-gradient(135deg,{c1},{c2})'>\
             <img alt='' loading='lazy' src='{}' \
             onload=\"this.style.opacity='1'\" onerror=\"this.style.display='none'\">\
             <span class='an'>{site_name}</span></span>\
             <span class='body'><span class='t'>{}</span>\
             <span class='meta'><span class='dot' style='background:{c1}'></span>{}</span>\
             {}</span></a>",
            i * 45,
            escape_html(&r.url),
            i + 1,
            escape_html(&img_src),
            escape_html(&r.title),
            escape_html(&domain),
            summary_html,
        ));
    }

    let engine_badge = if engine.is_empty() { "多引擎" } else { engine };
    let now = chrono::Local::now().format("%H:%M").to_string();
    let count_label = if results.is_empty() {
        "0 条结果".to_string()
    } else {
        format!("{} 条结果", results.len())
    };
    format!(
        "<!DOCTYPE html><html><head><meta charset='utf-8'>\
         <meta name='viewport' content='width=device-width,initial-scale=1'>\
         <style>\
         *{{box-sizing:border-box}}\
         body{{font-family:system-ui,'Microsoft YaHei',sans-serif;background:#0b0e14;color:#e5e7eb;\
         padding:0 0 30px;margin:0;min-height:100vh}}\
         .hero{{padding:26px 24px 18px;background:linear-gradient(135deg,#101827 0%,#0b0e14 60%);\
         border-bottom:1px solid #1c2434;position:relative;overflow:hidden}}\
         .hero::before{{content:'';position:absolute;top:-70px;right:-40px;width:220px;height:220px;\
         border-radius:50%;background:radial-gradient(circle,rgba(56,189,248,.14),transparent 70%);pointer-events:none}}\
         .q{{font-size:19px;font-weight:700;color:#f8fafc;display:flex;align-items:center;gap:10px;margin-bottom:12px}}\
         .q .icon{{width:34px;height:34px;border-radius:10px;display:flex;align-items:center;justify-content:center;\
         font-size:16px;background:linear-gradient(135deg,#0ea5e9,#6366f1);box-shadow:0 4px 14px rgba(14,165,233,.35)}}\
         .chips{{display:flex;gap:8px;flex-wrap:wrap}}\
         .chip{{font-size:11px;color:#94a3b8;background:#141a26;border:1px solid #232c3b;\
         border-radius:999px;padding:4px 11px}}\
         .chip.acc{{color:#7dd3fc;border-color:rgba(56,189,248,.35);background:rgba(56,189,248,.08)}}\
         .list{{max-width:780px;margin:18px auto 0;padding:0 20px}}\
         @keyframes rise{{from{{opacity:0;transform:translateY(10px)}}to{{opacity:1;transform:none}}}}\
         .item{{display:flex;gap:13px;align-items:flex-start;text-decoration:none;color:inherit;\
         background:linear-gradient(180deg,#141926,#111622);border:1px solid #1e2636;border-radius:14px;\
         padding:15px 17px;margin-bottom:11px;animation:rise .4s ease both;transition:border-color .18s,\
         transform .18s,box-shadow .18s}}\
         .item:hover{{border-color:rgba(56,189,248,.55);transform:translateY(-2px);\
         box-shadow:0 8px 24px rgba(2,8,20,.5),0 0 0 1px rgba(56,189,248,.12)}}\
         .num{{flex-shrink:0;width:20px;font-size:11px;color:#475569;font-weight:600;padding-top:5px;\
         font-variant-numeric:tabular-nums}}\
         .ava{{flex-shrink:0;width:36px;height:36px;border-radius:10px;display:flex;align-items:center;\
         justify-content:center;font-size:16px;font-weight:700;color:#fff;text-shadow:0 1px 2px rgba(0,0,0,.3);\
         box-shadow:inset 0 0 0 1px rgba(255,255,255,.07)}}\
         .ava img{{width:22px;height:22px;border-radius:5px;opacity:0;transition:opacity .25s;\
         filter:drop-shadow(0 1px 2px rgba(0,0,0,.4))}}\
         .ava .an{{font-size:9px;font-weight:700;letter-spacing:.2px;color:#fff;max-width:30px;\
         overflow:hidden;text-overflow:ellipsis;white-space:nowrap;text-shadow:0 1px 2px rgba(0,0,0,.35);\
         .body{{flex:1;min-width:0;display:flex;flex-direction:column;gap:5px}}\
         .t{{font-size:14.5px;font-weight:600;color:#93c5fd;line-height:1.45;\
         transition:color .15s;overflow:hidden;display:-webkit-box;-webkit-line-clamp:2;\
         -webkit-box-orient:vertical}}\
         .item:hover .t{{color:#bfdbfe}}\
         .meta{{display:flex;align-items:center;gap:6px;font-size:11px;color:#64748b}}\
         .dot{{width:7px;height:7px;border-radius:50%;flex-shrink:0}}\
         .s{{font-size:12.5px;color:#9ca3af;line-height:1.65;overflow:hidden;display:-webkit-box;\
         -webkit-line-clamp:3;-webkit-box-orient:vertical}}\
         .empty{{max-width:780px;margin:30px auto;padding:46px 20px;text-align:center;\
         background:#10141f;border:1px dashed #26304a;border-radius:16px}}\
         .empty .ico{{font-size:38px;margin-bottom:12px}}\
         .empty .msg{{font-size:15px;color:#cbd5e1;font-weight:600;margin-bottom:6px}}\
         .empty .hint{{font-size:12.5px;color:#64748b}}\
         .foot{{max-width:780px;margin:22px auto 0;padding:0 20px;text-align:center;\
         font-size:11px;color:#475569}}\
         </style></head><body>\
         <div class='hero'><div class='q'><span class='icon'>🔍</span>{}</div>\
         <div class='chips'><span class='chip acc'>🌐 {}</span><span class='chip'>{}</span>\
         <span class='chip'>🕒 {}</span></div></div>\
         <div class='list'>{}</div>\
         <div class='foot'>白泽多引擎聚合搜索 · 点击卡片在新窗口打开来源</div>\
         </body></html>",
        escape_html(query),
        escape_html(engine_badge),
        escape_html(&count_label),
        escape_html(&now),
        items,
    )
}

/// 搜索引擎
#[derive(Clone, Copy)]
enum Engine {
    DuckDuckGo,
    Bing,
    Baidu,
}

impl Engine {
    fn name(&self) -> &str {
        match self {
            Engine::DuckDuckGo => "DuckDuckGo",
            Engine::Bing => "Bing",
            Engine::Baidu => "百度",
        }
    }
    fn url(&self, query: &str) -> String {
        match self {
            Engine::DuckDuckGo => format!("https://html.duckduckgo.com/html/?q={}", percent_encode(query)),
            Engine::Bing => format!("https://www.bing.com/search?q={}", percent_encode(query)),
            Engine::Baidu => format!("https://www.baidu.com/s?wd={}", percent_encode(query)),
        }
    }
    fn parse(&self, html: &str) -> Vec<SearchResult> {
        match self {
            Engine::DuckDuckGo => parse_duckduckgo(html),
            Engine::Bing => parse_bing(html),
            Engine::Baidu => parse_baidu(html),
        }
    }
}

/// 常见浏览器 User-Agent（轮换，降低被固定指纹识别的概率）
const USER_AGENTS: &[&str] = &[
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/122.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36 Edg/124.0.0.0",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/121.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:123.0) Gecko/20100101 Firefox/123.0",
];

/// 随机选一个 UA（用当前纳秒做伪随机种子）
fn random_ua() -> &'static str {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0);
    USER_AGENTS[n % USER_AGENTS.len()]
}

/// 构造带完整浏览器请求头的 HTTP 客户端（模拟真实浏览器，降低反爬拦截）
///
/// `use_env_proxy=false` 时显式禁用系统/环境代理（HTTP_PROXY 等）——
/// 环境代理进程经常漂移或离线（如本地代理端口变更后未更新），reqwest 默认
/// 会读取环境代理变量，死代理会让所有引擎全部请求失败，表现为「搜索不到」。
fn build_client_opts(timeout: Duration, use_env_proxy: bool) -> Result<reqwest::blocking::Client, String> {
    use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, ACCEPT_LANGUAGE, CACHE_CONTROL, PRAGMA};

    let mut headers = HeaderMap::new();
    headers.insert(
        ACCEPT,
        HeaderValue::from_static(
            "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7",
        ),
    );
    headers.insert(
        ACCEPT_LANGUAGE,
        HeaderValue::from_static("zh-CN,zh;q=0.9,en;q=0.8,en-US;q=0.7"),
    );
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(PRAGMA, HeaderValue::from_static("no-cache"));
    headers.insert(
        "upgrade-insecure-requests",
        HeaderValue::from_static("1"),
    );
    headers.insert("sec-fetch-dest", HeaderValue::from_static("document"));
    headers.insert("sec-fetch-mode", HeaderValue::from_static("navigate"));
    headers.insert("sec-fetch-site", HeaderValue::from_static("none"));
    headers.insert("sec-fetch-user", HeaderValue::from_static("?1"));
    headers.insert(
        "sec-ch-ua",
        HeaderValue::from_static(
            "\"Not_A Brand\";v=\"8\", \"Chromium\";v=\"120\", \"Google Chrome\";v=\"120\"",
        ),
    );
    headers.insert("sec-ch-ua-mobile", HeaderValue::from_static("?0"));
    headers.insert("sec-ch-ua-platform", HeaderValue::from_static("\"Windows\""));
    headers.insert("dnt", HeaderValue::from_static("1"));

    let mut builder = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .user_agent(random_ua())
        .default_headers(headers)
        .redirect(reqwest::redirect::Policy::limited(10));
    if !use_env_proxy {
        builder = builder.no_proxy();
    }
    builder.build().map_err(|e| format!("创建 HTTP 客户端失败: {e}"))
}

/// 抓取网页（完整浏览器头 + UA 轮换 + 最多 3 次重试退避；gzip/brotli 自动解压）
fn fetch_html(url: &str) -> Result<String, String> {
    fetch_html_opts(url, Duration::from_secs(15), 3, true)
}

/// 可调抓取：超时 / 尝试次数 / 是否走环境代理
fn fetch_html_opts(
    url: &str,
    timeout: Duration,
    attempts: usize,
    use_env_proxy: bool,
) -> Result<String, String> {
    let url = url.to_string();
    std::thread::spawn(move || -> Result<String, String> {
        let mut last_err = String::new();
        for attempt in 0..attempts {
            let client = match build_client_opts(timeout, use_env_proxy) {
                Ok(c) => c,
                Err(e) => {
                    last_err = e;
                    continue;
                }
            };
            match client.get(&url).send() {
                Ok(resp) if resp.status().is_success() => {
                    return resp.text().map_err(|e| format!("读取响应失败: {e}"));
                }
                Ok(resp) => last_err = format!("HTTP {}", resp.status()),
                Err(e) => last_err = format!("请求失败: {e}"),
            }
            if attempt + 1 < attempts {
                std::thread::sleep(Duration::from_millis(300 * (attempt as u64 + 1)));
            }
        }
        Err(last_err)
    })
    .join()
    .map_err(|_| "抓取线程异常退出".to_string())?
}

/// 是否含中日韩字符（用于选择引擎顺序：CJK 查询对 Bing/百度友好，DDG 中文召回差且国内直连不通）
fn has_cjk(s: &str) -> bool {
    s.chars().any(|c| {
        matches!(c as u32,
            0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0x3040..=0x30FF | 0xAC00..=0xD7AF | 0x3000..=0x303F)
    })
}

/// 单引擎尝试：抓取 + 解析，返回 (结果, 失败原因)
fn try_engine(engine: Engine, query: &str, use_env_proxy: bool) -> (Vec<SearchResult>, String) {
    // 搜索页是轻量 HTML，8s 超时 + 2 次尝试足够；避免单引擎拖满 45s+ 才降级
    match fetch_html_opts(&engine.url(query), Duration::from_secs(8), 2, use_env_proxy) {
        Err(e) => (Vec::new(), e),
        Ok(html) => {
            let results = engine.parse(&html);
            if results.is_empty() {
                // HTTP 200 但解析为空：多为反爬验证页（DDG anomaly / 百度安全验证 / Bing 同意页）
                let hint = if html.contains("anomaly") || html.contains("challenge") {
                    "反爬验证页"
                } else if html.contains("wappass") || html.contains("安全验证") {
                    "百度安全验证"
                } else if html.len() < 2000 {
                    "响应内容过短（疑似拦截页）"
                } else {
                    "页面布局变化或无匹配结果"
                };
                (Vec::new(), format!("解析 0 条（{hint}）"))
            } else {
                (results, String::new())
            }
        }
    }
}

/// 多引擎搜索：失败/空结果自动降级。
/// 引擎顺序按查询语言自适应——CJK 查询走 Bing → 百度 → DuckDuckGo（DDG 国内直连不通
/// 且中文召回差），非 CJK 查询走 DuckDuckGo → Bing → 百度。
/// 全部失败且系统配置了环境代理时，用「禁用环境代理」的直连客户端再兜底一轮——
/// 环境代理进程离线（端口漂移/未启动）时 reqwest 默认撞死代理，是搜索全军覆没的常见根因。
/// 返回 (成功引擎名, 结果列表, 已尝试引擎及原因)
fn search_multi(query: &str) -> (String, Vec<SearchResult>, Vec<String>) {
    let order: &[Engine] = if has_cjk(query) {
        &[Engine::Bing, Engine::Baidu, Engine::DuckDuckGo]
    } else {
        &[Engine::DuckDuckGo, Engine::Bing, Engine::Baidu]
    };

    let mut tried: Vec<String> = Vec::new();
    // 第一轮走系统默认（含环境代理）；全败后第二轮禁用环境代理直连重试
    for (round, use_env_proxy) in [(0usize, true), (1, false)] {
        let tag = if round == 1 { "[直连]" } else { "" };
        for engine in order {
            let (results, err) = try_engine(*engine, query, use_env_proxy);
            if results.is_empty() {
                tried.push(format!("{}{}: {}", tag, engine.name(), err));
            } else {
                tried.push(format!("{}{}: 成功", tag, engine.name()));
                return (engine.name().to_string(), results, tried);
            }
        }
    }
    (String::new(), Vec::new(), tried)
}

/// 解析 Bing 结果：按 b_algo 结果块切分，块内标题与摘要天然对齐；
/// 布局无 b_algo 时回退到全页扫描
fn parse_bing(html: &str) -> Vec<SearchResult> {
    let mut results = Vec::new();

    // 主路径：每个 <li class="b_algo"> 是一条完整结果
    let re_block = Regex::new(r#"(?s)<li[^>]*class="[^"]*b_algo[^"]*"[^>]*>(.*?)</li>"#).unwrap();
    for cap in re_block.captures_iter(html) {
        let block = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        if let Some(r) = parse_bing_block(block) {
            results.push(r);
            if results.len() >= 10 {
                break;
            }
        }
    }
    if !results.is_empty() {
        return results;
    }

    // 回退：无 b_algo（布局变体）时全页扫描 h2 链接
    let re_link = Regex::new(r#"<h2[^>]*><a[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#).unwrap();
    for cap in re_link.captures_iter(html) {
        let url = cap.get(1).map(|m| m.as_str()).unwrap_or("").to_string();
        let title = cap.get(2).map(|m| m.as_str()).unwrap_or("");
        if url.starts_with("http") {
            results.push(SearchResult {
                title: strip_tags(title),
                url,
                summary: String::new(),
                icon: None,
            });
            if results.len() >= 10 {
                break;
            }
        }
    }
    results
}

/// 从单个 b_algo 块中提取标题/链接/摘要/原生网站图标
fn parse_bing_block(block: &str) -> Option<SearchResult> {
    let re_link = Regex::new(r#"(?s)<h2[^>]*><a[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#).unwrap();
    let cap = re_link.captures(block)?;
    let url = cap.get(1)?.as_str().to_string();
    if !url.starts_with("http") {
        return None;
    }
    let title = strip_tags(cap.get(2).map(|m| m.as_str()).unwrap_or(""));
    // 摘要取块内第一段 <p>（b_algo 块内的 <p> 就是本条结果的摘要，不再全页错位）
    let re_p = Regex::new(r#"(?s)<p[^>]*>(.*?)</p>"#).unwrap();
    let summary = re_p
        .captures(block)
        .map(|c| strip_tags(c.get(1).map(|m| m.as_str()).unwrap_or("")))
        .unwrap_or_default();
    // 网站图标：Bing 在每条结果的 wr_fav 区域内嵌了目标站点 favicon 的缩略图，
    // 直接取用（与搜索结果同源，无需第三方 favicon 服务）
    let re_fav = Regex::new(r#"(?s)wr_fav.*?<img[^>]*src="([^"]+)""#).unwrap();
    let icon = re_fav
        .captures(block)
        .map(|c| c.get(1).map(|m| m.as_str()).unwrap_or("").to_string());
    Some(SearchResult { title, url, summary, icon })
}

/// 解析百度结果：h3 不再强制要求 class 属性；摘要多选择器兜底 + (?s) 跨行匹配
/// （百度页面摘要常含换行，旧正则 `.` 不匹配换行导致摘要全部丢失）
fn parse_baidu(html: &str) -> Vec<SearchResult> {
    let re_link = Regex::new(r#"(?s)<h3[^>]*>\s*<a[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#).unwrap();
    let re_snip_a = Regex::new(r#"(?s)class="c-abstract[^"]*"[^>]*>(.*?)</div>"#).unwrap();
    let re_snip_b = Regex::new(r#"(?s)<span[^>]*class="content-right_[^"]*"[^>]*>(.*?)</span>"#).unwrap();
    let mut links = Vec::new();
    for cap in re_link.captures_iter(html) {
        let url = cap.get(1).map(|m| m.as_str()).unwrap_or("").to_string();
        let title = cap.get(2).map(|m| m.as_str()).unwrap_or("");
        if url.starts_with("http") {
            links.push((url, strip_tags(title)));
        }
    }
    let mut snips: Vec<String> = re_snip_a
        .captures_iter(html)
        .map(|c| strip_tags(c.get(1).map(|m| m.as_str()).unwrap_or("")))
        .filter(|s| !s.is_empty())
        .collect();
    if snips.is_empty() {
        snips = re_snip_b
            .captures_iter(html)
            .map(|c| strip_tags(c.get(1).map(|m| m.as_str()).unwrap_or("")))
            .filter(|s| !s.is_empty())
            .collect();
    }
    links
        .into_iter()
        .zip(snips.into_iter().chain(std::iter::repeat(String::new())))
        .take(10)
        .map(|((url, title), summary)| SearchResult { title, url, summary, icon: None })
        .collect()
}

// ───────────────────────── browser_navigate ─────────────────────────

pub struct BrowserNavigateTool {
    app: AppHandle,
    state: Arc<Mutex<BrowserState>>,
}

impl BrowserNavigateTool {
    pub fn new(app: AppHandle, state: Arc<Mutex<BrowserState>>) -> Self {
        Self { app, state }
    }
}

impl Tool for BrowserNavigateTool {
    fn name(&self) -> &str {
        "browser_navigate"
    }
    fn description(&self) -> &str {
        "在独立的原生浏览器窗口中打开一个网址（可加载任意站点、绕过 iframe 嵌入限制）。当内置浏览器（白泽·浏览器）里的 url 标签页因网站安全策略（X-Frame-Options/CSP）拒绝内嵌、或需要完整登录/脚本环境而打不开时，用这个工具在独立浏览器窗口打开网页。"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "要打开的完整网址，如 https://example.com" }
            },
            "required": ["url"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let url = args["url"].as_str().ok_or("缺少参数 url")?.to_string();
        {
            let mut s = self.state.lock().unwrap();
            s.url = url.clone();
        }
        crate::windows::open_external_browser(&self.app, &url);
        Ok(json!({ "ok": true, "url": url }))
    }
}

// ───────────────────────── browser_search ─────────────────────────

pub struct BrowserSearchTool {
    app: AppHandle,
    state: Arc<Mutex<BrowserState>>,
}

impl BrowserSearchTool {
    pub fn new(app: AppHandle, state: Arc<Mutex<BrowserState>>) -> Self {
        Self { app, state }
    }
}

impl Tool for BrowserSearchTool {
    fn name(&self) -> &str {
        "browser_search"
    }
    fn description(&self) -> &str {
        "在内置浏览器窗口中进行内容搜索（抓取 DuckDuckGo 结果并解析成清晰的结果列表展示）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "搜索关键词" }
            },
            "required": ["query"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let query = args["query"].as_str().ok_or("缺少参数 query")?.to_string();

        // 多引擎搜索：DuckDuckGo → Bing → 百度，失败/空结果自动降级
        let (engine, results, tried) = search_multi(&query);
        let html = build_results_html(&query, &engine, &results);

        {
            let mut s = self.state.lock().unwrap();
            s.results = Some(results.clone());
            s.open_tab("html", &format!("搜索：{query}"), &html);
        }
        emit_update(&self.app, &self.state);

        let note = if results.is_empty() {
            Some(format!(
                "已尝试各搜索引擎，均未获取到结果。详情：{}。请基于你已有的知识继续完成任务，不要立即重试同一关键词。",
                tried.join("；")
            ))
        } else {
            None
        };
        Ok(json!({
            "ok": true,
            "query": query,
            "engine": engine,
            "results": results,
            "note": note,
        }))
    }
}

// ───────────────────────── web_search（纯文本结构化搜索，不打开浏览器） ─────────────────────────

/// web_search 工具：联网搜索并返回结构化结果（标题/链接/摘要），供白泽直接注入上下文回答并标注来源，
/// 不打开浏览器窗口。与 browser_search（可视化展示）互补——本工具用于「回答问题、引用来源」场景。
pub struct WebSearchTool;

impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }
    fn description(&self) -> &str {
        "联网搜索并返回结构化结果（标题/链接/摘要），供你直接用结果回答实时/时效性问题并在答案里标注来源链接。多引擎按语言自适应降级（中文查询走 Bing→百度→DuckDuckGo，英文查询走 DuckDuckGo→Bing→百度），全部失败会附各引擎失败原因。用于知识库里没有的、需要最新信息的问题"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "搜索关键词" },
                "max_results": { "type": "integer", "description": "最多返回条数，默认 5，范围 1~10" }
            },
            "required": ["query"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let query = args["query"].as_str().ok_or("缺少参数 query")?.to_string();
        let max = args["max_results"].as_u64().unwrap_or(5).clamp(1, 10) as usize;
        let (engine, results, tried) = search_multi(&query);
        if results.is_empty() {
            return Err(format!(
                "搜索失败或未获取到结果。各引擎详情：{}。请基于已有知识继续，或换个更具体的关键词，不要立即重试同一关键词。",
                tried.join("；")
            ));
        }
        let items: Vec<Value> = results
            .into_iter()
            .take(max)
            .map(|r| json!({ "title": r.title, "url": r.url, "summary": r.summary }))
            .collect();
        Ok(json!({
            "query": query,
            "engine": engine,
            "sources": tried,
            "results": items,
        }))
    }
}

// ───────────────────────── browser_render_html ─────────────────────────

pub struct BrowserRenderHtmlTool {
    app: AppHandle,
    state: Arc<Mutex<BrowserState>>,
}

impl BrowserRenderHtmlTool {
    pub fn new(app: AppHandle, state: Arc<Mutex<BrowserState>>) -> Self {
        Self { app, state }
    }
}

impl Tool for BrowserRenderHtmlTool {
    fn name(&self) -> &str {
        "browser_render_html"
    }
    fn description(&self) -> &str {
        "在内置浏览器窗口中渲染一段 HTML 代码或本地 HTML 文件（用于展示网页效果、图表、代码运行效果、数据可视化等）。传入完整 HTML 源码，或直接传本地 .html 文件绝对路径（会自动读取文件内容渲染）。这是展示本地 HTML 可视化内容的首选工具，可规避 file:// 内嵌限制。"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "html": { "type": "string", "description": "要渲染的完整 HTML 字符串，或本地 .html 文件绝对路径" },
                "title": { "type": "string", "description": "标签页标题（可选）" }
            },
            "required": ["html"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let mut html = args["html"].as_str().ok_or("缺少参数 html")?.to_string();
        let mut title = args["title"].as_str().unwrap_or("HTML 演示").to_string();

        // 若传入的是本地 .html 文件路径（不含 < 且可解析为本地 HTML 文件），读文件内容渲染
        if !html.contains('<') {
            if let Some(path) = resolve_local_file(&html) {
                if is_html_file(&path) {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        html = content;
                        title = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or(title);
                    }
                }
            }
        }

        let tab_id = {
            let mut s = self.state.lock().unwrap();
            s.open_tab("html", &title, &html)
        };
        emit_update(&self.app, &self.state);
        Ok(json!({ "ok": true, "tab": tab_id }))
    }
}

// ───────────────────────── browser_get（感知） ─────────────────────────

pub struct BrowserGetTool {
    state: Arc<Mutex<BrowserState>>,
}

impl BrowserGetTool {
    pub fn new(state: Arc<Mutex<BrowserState>>) -> Self {
        Self { state }
    }
}

impl Tool for BrowserGetTool {
    fn name(&self) -> &str {
        "browser_get"
    }
    fn description(&self) -> &str {
        "感知内置浏览器当前状态（当前网址、搜索结果列表或正在展示的 HTML 内容），了解用户正在浏览什么"
    }
    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, _args: Value) -> Result<Value, String> {
        let s = self.state.lock().unwrap();
        // 若当前是搜索结果，返回结构化结果（标题/链接/摘要），便于白泽感知
        if let Some(results) = &s.results {
            return Ok(json!({
                "mode": "search_results",
                "title": s.title,
                "results": results,
            }));
        }
        let preview = match &s.html {
            Some(h) => h.chars().take(1500).collect::<String>(),
            None => String::new(),
        };
        Ok(json!({
            "mode": "webpage",
            "url": s.url,
            "title": s.title,
            "html_len": s.html.as_ref().map(|h| h.chars().count()).unwrap_or(0),
            "html_preview": preview,
        }))
    }
}

// ───────────────────────── browser_read（感知网页文本） ─────────────────────────

/// 从 HTML 提取文本（标题 + 正文）
fn extract_text(html: &str) -> String {
    let title = Regex::new(r"(?is)<title[^>]*>(.*?)</title>")
        .ok()
        .and_then(|re| re.captures(html))
        .map(|c| strip_tags(c.get(1).map(|m| m.as_str()).unwrap_or("")));

    let re_script = Regex::new(r"(?is)<script[^>]*>.*?</script>").unwrap();
    let re_style = Regex::new(r"(?is)<style[^>]*>.*?</style>").unwrap();
    let cleaned = re_script.replace_all(html, "");
    let cleaned = re_style.replace_all(&cleaned, "");

    let text = strip_tags(&cleaned);
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");

    let mut out = String::new();
    if let Some(t) = title {
        if !t.is_empty() {
            out.push_str(&format!("标题：{t}\n\n"));
        }
    }
    out.push_str(&text.chars().take(2000).collect::<String>());
    out
}

pub struct BrowserReadTool {
    app: AppHandle,
    state: Arc<Mutex<BrowserState>>,
}

impl BrowserReadTool {
    pub fn new(app: AppHandle, state: Arc<Mutex<BrowserState>>) -> Self {
        Self { app, state }
    }
}

impl Tool for BrowserReadTool {
    fn name(&self) -> &str {
        "browser_read"
    }
    fn description(&self) -> &str {
        "抓取一个网页并读取其文本内容（标题+正文），用于感知网页实际内容（绕过 iframe 嵌入限制）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "要读取的网页完整网址" }
            },
            "required": ["url"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let url = args["url"].as_str().ok_or("缺少参数 url")?.to_string();
        let html = fetch_html(&url)?;
        let text = extract_text(&html);
        {
            let mut s = self.state.lock().unwrap();
            s.url = url.clone();
            s.html = Some(html);
            s.results = None;
            s.title = url.clone();
        }
        emit_update(&self.app, &self.state);
        Ok(json!({ "ok": true, "url": url, "text": text }))
    }
}

// ───────────────────────── 浏览器标签页工具 ─────────────────────────

/// 将 file:// URL 或原生绝对路径解析为本地文件路径（非本地返回 None）
fn resolve_local_file(raw: &str) -> Option<std::path::PathBuf> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    let decoded: String = if let Some(stripped) = s.strip_prefix("file://") {
        let no_host = stripped.strip_prefix("localhost").unwrap_or(stripped);
        percent_decode(no_host)
    } else if let Some(stripped) = s.strip_prefix("file:") {
        percent_decode(stripped)
    } else {
        s.to_string()
    };
    if decoded.is_empty() {
        return None;
    }
    // Windows：file:///C:/... 剥前缀后常带一个前导斜杠，剥离之
    let bytes = decoded.as_bytes();
    let cleaned = if bytes.len() >= 3
        && (bytes[0] == b'/' || bytes[0] == b'\\')
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':'
    {
        decoded[1..].to_string()
    } else {
        decoded
    };
    // 仅接受绝对路径（盘符 X: 或根前导 / \）
    let is_abs = cleaned.starts_with('/')
        || cleaned.starts_with('\\')
        || (cleaned.len() >= 2 && cleaned.as_bytes()[1] == b':');
    if is_abs {
        Some(std::path::PathBuf::from(cleaned))
    } else {
        None
    }
}

/// 是否为 HTML 文件（.html / .htm）
fn is_html_file(p: &std::path::Path) -> bool {
    p.extension()
        .map(|e| {
            let e = e.to_ascii_lowercase();
            e == "html" || e == "htm"
        })
        .unwrap_or(false)
}

pub struct BrowserOpenTool {
    app: AppHandle,
    state: Arc<Mutex<BrowserState>>,
}

impl BrowserOpenTool {
    pub fn new(app: AppHandle, state: Arc<Mutex<BrowserState>>) -> Self {
        Self { app, state }
    }
}

impl Tool for BrowserOpenTool {
    fn name(&self) -> &str {
        "browser_open"
    }
    fn description(&self) -> &str {
        "在内置浏览器（白泽·浏览器）中打开一个新标签页（kind 可选 markdown/text/html/video/url）。每个内容一个标签页。展示网页/HTML 效果：优先用 kind=\"html\" 直接把完整 HTML 源码放进 content 渲染（内嵌 CSS/JS，无外部依赖），或浏览本地 .html 文件时直接把文件绝对路径放进 content（kind 用 url 或 html 均可，会自动读取文件内容渲染，规避 file:// 内嵌限制）。kind=\"url\" 仅用于打开 http/https 网址；若某网站拒绝内嵌导致打不开，改用 browser_navigate。video 的 content 填视频文件绝对路径或在线视频 URL"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "kind": { "type": "string", "enum": ["markdown", "text", "html", "video", "url"], "description": "标签页类型（html=直接渲染 HTML 源码；url=打开 http/https 网址或本地文件路径）" },
                "content": { "type": "string", "description": "内容（html 填完整 HTML 源码；url 填 http/https 网址或本地 .html 文件绝对路径；video 填视频文件绝对路径或在线 URL）" },
                "title": { "type": "string", "description": "标签页标题（可选）" }
            },
            "required": ["kind", "content"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let mut kind = args["kind"].as_str().unwrap_or("markdown").to_string();
        let mut content = args["content"].as_str().ok_or("缺少参数 content")?.to_string();
        let mut title = args["title"].as_str().unwrap_or("").to_string();

        // url 类型若指向本地 HTML 文件：读文件内容转成 html 标签页渲染，
        // 规避内嵌 iframe 加载 file:// 的安全限制与空格/反斜杠路径编码问题
        let is_remote = content.starts_with("http://") || content.starts_with("https://");
        if kind == "url" && !is_remote {
            if let Some(path) = resolve_local_file(&content) {
                if is_html_file(&path) {
                    match std::fs::read_to_string(&path) {
                        Ok(html) => {
                            kind = "html".to_string();
                            if title.is_empty() {
                                title = path
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| "本地页面".into());
                            }
                            content = html;
                        }
                        Err(e) => {
                            return Err(format!(
                                "无法读取本地文件 {content}：{e}。请改用 browser_render_html 直接渲染 HTML 源码，或用 browser_navigate 用系统浏览器打开该文件。"
                            ));
                        }
                    }
                }
            }
        }

        let tab_id = {
            let mut s = self.state.lock().unwrap();
            s.open_tab(&kind, &title, &content)
        };
        emit_update(&self.app, &self.state);
        Ok(json!({ "ok": true, "tab": tab_id, "kind": kind }))
    }
}

pub struct BrowserCloseTool {
    app: AppHandle,
    state: Arc<Mutex<BrowserState>>,
}

impl BrowserCloseTool {
    pub fn new(app: AppHandle, state: Arc<Mutex<BrowserState>>) -> Self {
        Self { app, state }
    }
}

impl Tool for BrowserCloseTool {
    fn name(&self) -> &str {
        "browser_close"
    }
    fn description(&self) -> &str {
        "关闭浏览器中的一个标签页"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "标签页 id（browser_tabs 返回）" }
            },
            "required": ["id"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let id = args["id"].as_str().ok_or("缺少参数 id")?;
        {
            let mut s = self.state.lock().unwrap();
            s.close_tab(id);
        }
        emit_update(&self.app, &self.state);
        Ok(json!({ "ok": true }))
    }
}

pub struct BrowserCloseAllTool {
    app: AppHandle,
    state: Arc<Mutex<BrowserState>>,
}

impl BrowserCloseAllTool {
    pub fn new(app: AppHandle, state: Arc<Mutex<BrowserState>>) -> Self {
        Self { app, state }
    }
}

impl Tool for BrowserCloseAllTool {
    fn name(&self) -> &str {
        "browser_close_all"
    }
    fn description(&self) -> &str {
        "关闭浏览器中的所有标签页"
    }
    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, _args: Value) -> Result<Value, String> {
        {
            let mut s = self.state.lock().unwrap();
            s.tabs.clear();
        }
        emit_update(&self.app, &self.state);
        Ok(json!({ "ok": true }))
    }
}

pub struct BrowserTabsTool {
    state: Arc<Mutex<BrowserState>>,
}

impl BrowserTabsTool {
    pub fn new(state: Arc<Mutex<BrowserState>>) -> Self {
        Self { state }
    }
}

impl Tool for BrowserTabsTool {
    fn name(&self) -> &str {
        "browser_tabs"
    }
    fn description(&self) -> &str {
        "感知浏览器当前有哪些标签页（id/类型/标题），用于定位、切换和关闭"
    }
    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, _args: Value) -> Result<Value, String> {
        let s = self.state.lock().unwrap();
        Ok(json!({ "count": s.tabs.len(), "tabs": s.tabs }))
    }
}

pub struct BrowserSwitchTool {
    app: AppHandle,
    state: Arc<Mutex<BrowserState>>,
}

impl BrowserSwitchTool {
    pub fn new(app: AppHandle, state: Arc<Mutex<BrowserState>>) -> Self {
        Self { app, state }
    }
}

impl Tool for BrowserSwitchTool {
    fn name(&self) -> &str {
        "browser_switch"
    }
    fn description(&self) -> &str {
        "切换浏览器到指定的标签页"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "标签页 id（browser_tabs 返回）" }
            },
            "required": ["id"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let id = args["id"].as_str().ok_or("缺少参数 id")?;
        {
            let mut s = self.state.lock().unwrap();
            s.set_active_tab(id);
        }
        emit_update(&self.app, &self.state);
        Ok(json!({ "ok": true }))
    }
}

// ───────────────────────── browser_act（headless Chrome 交互自动化）─────────────────────────

/// 全局 Chrome 实例（懒加载，跨工具调用复用，user-data-dir 保持登录态）。
/// 用 `Mutex<Option<Arc<Browser>>>` 而非 `OnceLock`：连接因 Chrome 被手动关闭或长时间空闲
/// 而失效后，下一次调用会自动重建实例，而非永久卡在已死的连接上。
static BROWSER: Mutex<Option<Arc<headless_chrome::Browser>>> = Mutex::new(None);

/// 关闭受控浏览器会话并结束其 Chrome 进程。
/// 浏览器实例由 static 持有，进程退出时不会触发 drop —— 不显式关闭的话，
/// 每次会话（应用关闭/测试进程退出）都会留下一组孤儿 Chrome 占着专属 profile 目录。
pub fn shutdown_browser() {
    let mut guard = match BROWSER.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    // 丢弃最后一个 Arc：Browser drop 内部会终止 Chrome 子进程
    *guard = None;
    set_active_target(None);
}

/// 空闲超时：headless_chrome 的 CDP 连接在「无事件」超过该时长后会主动断开，默认仅 30 秒，
/// 导致用户短暂离开就触发「连接已关闭」。这里拉长到 30 分钟。
const CHROME_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// 设置页手动指定的浏览器可执行文件路径（持久化 settings 表，启动时恢复）
static CUSTOM_BROWSER_PATH: RwLock<Option<std::path::PathBuf>> = RwLock::new(None);

/// 保存/清除手动指定的浏览器路径（空串视为清除）
pub fn set_custom_browser_path(path: Option<&str>) {
    let trimmed = path.map(str::trim).filter(|s| !s.is_empty());
    *CUSTOM_BROWSER_PATH.write().unwrap() = trimmed.map(std::path::PathBuf::from);
}

fn custom_browser_path() -> Option<std::path::PathBuf> {
    CUSTOM_BROWSER_PATH.read().unwrap().clone()
}

/// 当前实际解析到的浏览器路径（供设置页展示探测结果）
pub fn resolved_browser_path() -> Option<std::path::PathBuf> {
    chrome_path()
}

/// 从注册表「App Paths」读取浏览器主程序（HKLM/HKCU 各查一次）
fn browser_path_from_registry(exe_name: &str) -> Option<std::path::PathBuf> {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    for hive in [
        "HKLM\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\App Paths",
        "HKCU\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\App Paths",
    ] {
        let key = format!("{hive}\\{exe_name}");
        use std::os::windows::process::CommandExt;
        let out = std::process::Command::new("reg")
            .args(["query", &key, "/ve"])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        if let Ok(out) = out {
            let text = String::from_utf8_lossy(&out.stdout);
            for line in text.lines() {
                if let Some(pos) = line.find("REG_SZ") {
                    let p = line[pos + "REG_SZ".len()..].trim();
                    if !p.is_empty() && std::path::Path::new(p).exists() {
                        return Some(std::path::PathBuf::from(p));
                    }
                }
            }
        }
    }
    None
}

fn chrome_path() -> Option<std::path::PathBuf> {
    // 1) 设置页手动指定的路径
    if let Some(p) = custom_browser_path() {
        if std::path::Path::new(&p).exists() {
            return Some(p);
        }
    }
    // 2) 环境变量 BAIZE_CHROME_PATH
    if let Ok(p) = std::env::var("BAIZE_CHROME_PATH") {
        let p = p.trim().to_string();
        if !p.is_empty() && std::path::Path::new(&p).exists() {
            return Some(std::path::PathBuf::from(p));
        }
    }
    // 3) 注册表 App Paths（chrome.exe / msedge.exe）
    for exe in ["chrome.exe", "msedge.exe"] {
        if let Some(p) = browser_path_from_registry(exe) {
            return Some(p);
        }
    }
    // 4) 常见安装路径（含用户级安装与 Chromium / Brave）
    let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".into());
    let pf86 =
        std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| "C:\\Program Files (x86)".into());
    let lad = std::env::var("LocalAppData").unwrap_or_else(|_| {
        std::env::var("USERPROFILE")
            .map(|u| format!("{u}\\AppData\\Local"))
            .unwrap_or_default()
    });
    let candidates = [
        format!("{pf}\\Google\\Chrome\\Application\\chrome.exe"),
        format!("{pf86}\\Google\\Chrome\\Application\\chrome.exe"),
        format!("{lad}\\Google\\Chrome\\Application\\chrome.exe"),
        format!("{pf}\\Microsoft\\Edge\\Application\\msedge.exe"),
        format!("{pf86}\\Microsoft\\Edge\\Application\\msedge.exe"),
        format!("{lad}\\Microsoft\\Edge\\Application\\msedge.exe"),
        format!("{pf}\\Chromium\\Application\\chrome.exe"),
        format!("{lad}\\Chromium\\Application\\chrome.exe"),
        format!("{pf}\\BraveSoftware\\Brave-Browser\\Application\\brave.exe"),
        format!("{lad}\\BraveSoftware\\Brave-Browser\\Application\\brave.exe"),
    ];
    candidates
        .iter()
        .find(|p| std::path::Path::new(p).exists())
        .map(std::path::PathBuf::from)
}


fn browser_profile_dir() -> std::path::PathBuf {
    // 统一收编到「安装目录\data\browser-profile」（paths::init 含旧目录一次性迁移，
    // 历史位置 LocalAppData\baize\browser-profile 的登录态会被复制过来），不再写 C 盘 AppData
    crate::paths::browser_profile_dir()
}

/// 清理占用受控浏览器专属 user-data-dir 的孤儿 Chrome（宿主进程被强杀时来不及 drop 所致）。
/// 不清理的话，残留实例锁死该目录，后续 Browser::new 会无限挂起且无任何报错。
/// 只按命令行中的专属 profile 路径匹配，不会误伤用户日常浏览的 Chrome。
/// 注意匹配 chrome.exe / msedge.exe / brave.exe —— chrome_path 可能回退到 Edge/Brave，
/// 只查 chrome.exe 会让 Edge 孤儿清不掉、锁死 profile 目录。
fn kill_orphan_profile_chrome() {
    let needle = browser_profile_dir().to_string_lossy().to_string();
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let ps = format!(
            "Get-CimInstance Win32_Process -Filter \"Name='chrome.exe' OR Name='msedge.exe' OR Name='brave.exe'\" \
             | Where-Object {{ $_.CommandLine -like '*{needle}*' }} \
             | ForEach-Object {{ Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }}"
        );
        let _ = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &ps])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
    }
    #[cfg(not(windows))]
    let _ = needle;
}

fn launch_chrome() -> Result<Arc<headless_chrome::Browser>, String> {
    use std::ffi::OsStr;
    // 去除 Chrome 的自动化提示标志（--enable-automation），让窗口表现为真实用户浏览器
    let enable_automation: &OsStr = OsStr::new("--enable-automation");
    let mut builder = headless_chrome::LaunchOptions::default_builder();
    // 可见（非 headless）桌面 Chrome + 固定窗口尺寸
    builder
        .headless(false)
        .window_size(Some((1280, 800)))
        .idle_browser_timeout(CHROME_IDLE_TIMEOUT)
        .ignore_default_args(vec![enable_automation]);
    if let Some(p) = chrome_path() {
        builder.path(Some(p));
    }
    // 持久化 user-data-dir，跨会话保留登录态 / Cookie / 历史
    builder.user_data_dir(Some(browser_profile_dir()));
    let options = builder.build().map_err(|e| e.to_string())?;
    let browser = headless_chrome::Browser::new(options).map_err(|e| e.to_string())?;
    Ok(Arc::new(browser))
}

fn get_browser() -> Result<Arc<headless_chrome::Browser>, String> {
    let mut guard = BROWSER.lock().map_err(|_| "浏览器实例锁异常".to_string())?;
    // 已有实例且连接仍存活（get_version 走 CDP，连接断开时立刻返回 Err），直接复用。
    if let Some(browser) = guard.as_ref() {
        if browser.get_version().is_ok() {
            return Ok(browser.clone());
        }
        // 连接已失效（Chrome 被手动关闭等）：丢弃旧实例，drop 会顺带结束残留 Chrome 进程。
        *guard = None;
    }
    // 冷启动放到独立线程并限时 40s：profile 目录被占用/杀软拦截时，Chrome 启动会
    // 无限挂起且无任何报错——限时返回明确错误，避免工具调用永久卡死。
    // （孤儿清理会结束占着专属 profile 的残留浏览器进程，为启动扫清障碍）
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        kill_orphan_profile_chrome();
        let _ = tx.send(launch_chrome());
    });
    let browser = match rx.recv_timeout(Duration::from_secs(40)) {
        Ok(r) => r?,
        Err(_) => {
            return Err(
                "受控浏览器启动超时（40s）：浏览器 profile 目录可能被占用，请稍后重试或重启白泽"
                    .into(),
            )
        }
    };
    *guard = Some(browser.clone());
    Ok(browser)
}

/// 当前活跃标签页的 target id（跨工具调用记住当前标签页）
static ACTIVE_TARGET: Mutex<Option<String>> = Mutex::new(None);

fn active_target_id() -> Option<String> {
    ACTIVE_TARGET.lock().ok().and_then(|g| g.clone())
}

fn set_active_target(id: Option<String>) {
    if let Ok(mut g) = ACTIVE_TARGET.lock() {
        *g = id;
    }
}

/// 列出 Chrome 当前所有标签页（含用户手动在可见窗口中打开的）
fn list_chrome_tabs() -> Result<Vec<Arc<headless_chrome::Tab>>, String> {
    let browser = get_browser()?;
    // register_missing_tabs 内部在 Chrome 连接断开时会 unwrap 导致 panic（headless_chrome 库 bug），
    // 用 catch_unwind 降级：连接异常时跳过补录，仅返回白泽自己创建的标签页。
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        browser.register_missing_tabs();
    }));
    // poison 恢复：register_missing_tabs 内部在连接异常时会 panic 毒化 mutex，
    // 此处用 into_inner 恢复，不再把「锁被毒化」当成硬错误返回。
    let guard = browser
        .get_tabs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Ok(guard.clone())
}

/// 按 target id 查找标签页
fn find_chrome_tab(id: &str) -> Result<Arc<headless_chrome::Tab>, String> {
    list_chrome_tabs()?
        .into_iter()
        .find(|t| t.get_target_id().as_str() == id)
        .ok_or_else(|| format!("未找到标签页 {id}"))
}

/// 新建标签页并设为活跃，同时去除 webdriver 自动化痕迹
fn new_chrome_tab() -> Result<Arc<headless_chrome::Tab>, String> {
    let browser = get_browser()?;
    let tab = browser.new_tab().map_err(|e| format!("创建标签页失败: {e}"))?;
    let _ = tab.enable_stealth_mode();
    set_active_target(Some(tab.get_target_id().clone()));
    Ok(tab)
}

/// 取得要操作的标签页：指定 id 时定位并激活之；未指定时复用活跃标签页，否则新建
fn resolve_tab(id: Option<&str>) -> Result<Arc<headless_chrome::Tab>, String> {
    match id {
        Some(id) => {
            let tab = find_chrome_tab(id)?;
            set_active_target(Some(id.to_string()));
            Ok(tab)
        }
        None => {
            if let Some(active) = active_target_id() {
                if let Ok(tab) = find_chrome_tab(&active) {
                    return Ok(tab);
                }
            }
            new_chrome_tab()
        }
    }
}

/// 截取当前标签页为 PNG 并保存到工作目录，返回（绝对路径, devicePixelRatio）。
/// 截图按设备像素输出，dpr 供调用方把 OCR / 视觉坐标映射回 CDP 点击用的 CSS 像素。
fn capture_tab_png(tab: &Arc<headless_chrome::Tab>) -> Result<(String, f64), String> {
    use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption;
    // from_surface=true 时截图按设备像素（CSS 像素 × deviceScaleFactor）输出，
    // 而 click_point / move_mouse_to_point 走 CDP 鼠标事件用的是 CSS 像素，
    // 因此一并返回 dpr，供调用方把 OCR / 视觉坐标 ÷ dpr 映射回 CSS 坐标。
    let data = tab
        .capture_screenshot(CaptureScreenshotFormatOption::Png, None, None, true)
        .map_err(|e| format!("截图失败: {e}"))?;
    let dpr = tab
        .evaluate("window.devicePixelRatio", false)
        .ok()
        .and_then(|r| r.value)
        .and_then(|v| v.as_f64())
        .filter(|d| *d > 0.0)
        .unwrap_or(1.0);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let name = format!("baize-browser-{ts}.png");
    // 收编到「安装目录\data\screens」，不再散落工作目录
    let dir = crate::paths::screens_dir();
    let _ = std::fs::create_dir_all(&dir);
    let full = dir.join(&name);
    std::fs::write(&full, &data).map_err(|e| format!("保存截图失败: {e}"))?;
    Ok((full.to_string_lossy().to_string(), dpr))
}

/// 在指定像素坐标处点击（先移动鼠标，再点击）
fn click_at(tab: &Arc<headless_chrome::Tab>, x: f64, y: f64) -> Result<(), String> {
    use headless_chrome::browser::tab::point::Point;
    let point = Point { x, y };
    tab.move_mouse_to_point(point).map_err(|e| format!("移动鼠标失败: {e}"))?;
    std::thread::sleep(Duration::from_millis(80));
    tab.click_point(point).map_err(|e| format!("点击失败: {e}"))?;
    Ok(())
}

/// 英文常见停用词/功能词：单独出现时信息量过低，不能作为 OCR 定位锚点。
/// 例如 "More information" 拆出的 "More" 会撞上正文 "Learn more" 里的 "more"，
/// 导致点击偏位；这些词必须被跳过，只保留信息量足够的目标词参与匹配。
const OCR_EN_STOPWORDS: &[&str] = &[
    "a", "an", "the", "and", "or", "but", "of", "to", "in", "on", "for", "with", "at",
    "by", "from", "as", "is", "are", "was", "were", "be", "been", "being", "have", "has",
    "had", "do", "does", "did", "will", "would", "shall", "should", "may", "might", "must",
    "can", "could", "this", "that", "these", "those", "it", "its", "he", "she", "they",
    "we", "you", "i", "my", "your", "his", "her", "their", "more", "most", "some", "any",
    "no", "not", "only", "own", "same", "so", "than", "too", "very", "just", "also",
    "here", "there", "about", "into", "over", "after", "before", "between", "under",
    "again", "further", "then", "once", "up", "down", "out", "off",
];

/// 在 OCR 词框列表中定位目标文字，返回所有命中框（(x,y,w,h)）。
/// 三级优先：完全相等 → 文本包含 → 词级等值（过滤停用词，取最长词）。
/// 不再「取第一个」，而是收集全部命中，交由上层在多候选时用 SOM 视觉消歧，避免同名文字点错。
fn match_ocr_word(words: &[Value], target: &str) -> Vec<(i32, i32, i32, i32)> {
    let t = target.trim();
    if t.is_empty() {
        return Vec::new();
    }
    // 坐标兼容整数与小数（Tesseract TSV 通常为整数，但部分封装输出浮点）
    let box_of = |w: &Value| -> Option<(i32, i32, i32, i32)> {
        let f = |k: &str| w[k].as_f64().map(|v| v.round() as i32);
        Some((f("x")?, f("y")?, f("w").unwrap_or(0), f("h").unwrap_or(0)))
    };
    let dedup = |v: Vec<(i32, i32, i32, i32)>| -> Vec<(i32, i32, i32, i32)> {
        let mut out: Vec<(i32, i32, i32, i32)> = Vec::new();
        for b in v {
            if !out.contains(&b) {
                out.push(b);
            }
        }
        out
    };

    // 1) 整词框与目标完全相等（忽略大小写）
    let mut exact = Vec::new();
    for w in words {
        let text = w["text"].as_str().unwrap_or("");
        if text.eq_ignore_ascii_case(t) {
            if let Some(b) = box_of(w) {
                exact.push(b);
            }
        }
    }
    if !exact.is_empty() {
        return dedup(exact);
    }
    // 2) OCR 词框文本包含目标（忽略大小写，处理单字 target 落在「Login Now」这类词框里）
    let mut contains = Vec::new();
    for w in words {
        let text = w["text"].as_str().unwrap_or("");
        if text.to_lowercase().contains(&t.to_lowercase()) {
            if let Some(b) = box_of(w) {
                contains.push(b);
            }
        }
    }
    if !contains.is_empty() {
        return dedup(contains);
    }
    // 3) 目标按词拆分，某个完整词与 OCR 词框等值（忽略大小写），取最长词优先。
    //    不再用「任意子串」（如 information 里的 for/in）去撞正文，避免点击偏位。
    //    同时过滤英文停用词：像 "More" 这种通用词撞上正文 "Learn more"，信息量不足以定位控件。
    let mut best_len = 0usize;
    let mut best = Vec::new();
    for token in t.split(|c: char| !c.is_alphanumeric()) {
        let token = token.trim();
        let n = token.chars().count();
        if n < 2 || OCR_EN_STOPWORDS.iter().any(|s| s.eq_ignore_ascii_case(token)) {
            continue;
        }
        if n < best_len {
            continue;
        }
        let mut hits = Vec::new();
        for w in words {
            let text = w["text"].as_str().unwrap_or("");
            if text.eq_ignore_ascii_case(token) {
                if let Some(b) = box_of(w) {
                    hits.push(b);
                }
            }
        }
        if hits.is_empty() {
            continue;
        }
        if n > best_len {
            best_len = n;
            best = hits;
        } else {
            best.extend(hits);
        }
    }
    dedup(best)
}

/// 框中心点（OCR / SOM 坐标均基于截图像素，需再 ÷ dpr 才对齐 CDP 点击坐标系）
fn box_center(b: (i32, i32, i32, i32)) -> (f64, f64) {
    (b.0 as f64 + b.2 as f64 / 2.0, b.1 as f64 + b.3 as f64 / 2.0)
}

/// 等待页面加载 / 渲染稳定，减少「截图时目标还没渲染出来」导致的定位失败。
fn wait_page_ready(tab: &Arc<headless_chrome::Tab>) {
    for _ in 0..30 {
        let ready = tab
            .evaluate("document.readyState === 'complete'", false)
            .ok()
            .and_then(|r| r.value)
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if ready {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// 「看→点」闭环：按可见文字 / 描述定位并点击控件。
/// 三级兜底：OCR 文字匹配 → Set-of-Marks 视觉选框 → 视觉模型坐标回归。
/// 未被命中时滚动一屏后重试（最多 4 屏），覆盖目标在视口外的情况。
fn click_by_description(tab: &Arc<headless_chrome::Tab>, target: &str) -> Result<Value, String> {
    wait_page_ready(tab);
    let mut last_err = String::new();

    for round in 0..4 {
        if round > 0 {
            // 目标未命中，滚动一屏（0.8 视口高度）继续寻找
            let _ = tab.evaluate(
                "window.scrollBy(0, Math.round(window.innerHeight * 0.8))",
                false,
            );
            std::thread::sleep(Duration::from_millis(150));
            wait_page_ready(tab);
        }
        let (path, dpr) = capture_tab_png(tab)?;

        // 一级：OCR 精确 / 模糊匹配（收集全部命中）
        let words: Vec<Value> = match crate::ocr::ocr_detect_gui(&path) {
            Ok((_text, w)) => w,
            Err(_) => Vec::new(),
        };
        let hits = match_ocr_word(&words, target);
        if hits.len() == 1 {
            let (x, y) = box_center(hits[0]);
            click_at(tab, x / dpr, y / dpr)?;
            return Ok(json!({ "ok": true, "method": "ocr", "x": x / dpr, "y": y / dpr, "target": target }));
        }

        // 二级：Set-of-Marks 视觉选框（多候选或未命中时消歧；有命中框则只标注命中框，减少干扰）
        let candidates: Vec<(i32, i32, i32, i32)> = if !hits.is_empty() {
            hits
        } else {
            words
                .iter()
                .filter_map(|w| {
                    Some((
                        w["x"].as_i64()? as i32,
                        w["y"].as_i64()? as i32,
                        w["w"].as_i64()? as i32,
                        w["h"].as_i64()? as i32,
                    ))
                })
                .collect()
        };
        if !candidates.is_empty() {
            let (annotated, centers) = match crate::som::annotate(&path, &candidates) {
                Ok(v) => v,
                Err(_) => (String::new(), Vec::new()),
            };
            if !annotated.is_empty() {
                if let Some(idx) = crate::som::som_select(&annotated, target, centers.len()) {
                    if let Some((x, y)) = centers.get(idx).copied() {
                        click_at(tab, x / dpr, y / dpr)?;
                        return Ok(json!({ "ok": true, "method": "som", "x": x / dpr, "y": y / dpr, "target": target }));
                    }
                }
            }
        }

        // 三级：视觉坐标回归
        match crate::visual_grounding::visual_locate(&path, target) {
            Ok((x, y)) => {
                click_at(tab, x / dpr, y / dpr)?;
                return Ok(json!({ "ok": true, "method": "visual_locate", "x": x / dpr, "y": y / dpr, "target": target }));
            }
            Err(e) => last_err = e,
        }
    }
    Err(format!(
        "未能在页面中找到「{target}」（已滚动重试）{}",
        if last_err.is_empty() {
            String::new()
        } else {
            format!(": {last_err}")
        }
    ))
}

/// 带超时上限的跳转等待：轮询 document.readyState，
/// 避免 wait_until_navigated 在误触跳转 / 反复导航 / 下载等场景下无限卡死整条测试链。
fn navigate_and_wait(tab: &Arc<headless_chrome::Tab>, url: &str, timeout: Duration) -> Result<(), String> {
    tab.navigate_to(url).map_err(|e| format!("跳转失败: {e}"))?;
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() >= timeout {
            return Err(format!("页面加载超时（{}s）：{url}", timeout.as_secs()));
        }
        let ready = tab
            .evaluate("document.readyState", false)
            .ok()
            .and_then(|v| v.value)
            .and_then(|v| v.as_str().map(|s| s.to_string()));
        match ready.as_deref() {
            Some("complete") => return Ok(()),
            // interactive 已可交互（DOM 就绪），3s 后不再死等 load 事件（长轮询页面可能永远到不了 complete）
            Some("interactive") if start.elapsed() >= Duration::from_secs(3) => return Ok(()),
            _ => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

/// 浏览器交互自动化工具：驱动可见桌面 Chrome（持久化登录态）
pub struct BrowserActTool;

impl Tool for BrowserActTool {
    fn name(&self) -> &str {
        "browser_act"
    }
    fn description(&self) -> &str {
        "驱动可见的桌面谷歌浏览器执行完整交互操作（持久化登录态，多标签页）：goto 跳转、click 点击、type 输入、wait 等待、screenshot 截图、evaluate 执行 JS、content 读取页面文本、new_tab 新建标签页、tabs 列出标签页、switch_tab 切换标签页、close_tab 关闭标签页、scroll 滚动、key 按键、reload 刷新、back 后退、forward 前进、state 感知浏览器状态、look 感知当前页面（截图+OCR 文本）、click_text 按可见文字定位并点击（看→点闭环）。这是白泽操作桌面谷歌浏览器的统一入口。用户提到「桌面浏览器/受控浏览器」时只使用本工具打开，不要叠加 browser_open / browser_navigate"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["goto", "click", "type", "wait", "screenshot", "evaluate", "content", "new_tab", "tabs", "switch_tab", "close_tab", "scroll", "key", "reload", "back", "forward", "state", "look", "click_text"], "description": "操作类型" },
                "url": { "type": "string", "description": "goto / new_tab 时的目标 URL" },
                "selector": { "type": "string", "description": "click / type 时的 CSS 选择器" },
                "text": { "type": "string", "description": "type 时输入的文本" },
                "target": { "type": "string", "description": "click_text 时要定位并点击的控件可见文字或描述，如「登录」「搜索」「下一步」" },
                "js": { "type": "string", "description": "evaluate 时的 JS 表达式" },
                "seconds": { "type": "number", "description": "wait 时等待秒数（默认 1）" },
                "tab_id": { "type": "string", "description": "目标标签页 target id（tabs 返回）。switch_tab / close_tab 必填；goto / click / type / scroll / key / reload / back / forward / screenshot / evaluate / content / look / click_text 可选，默认作用于当前活跃标签页" },
                "key": { "type": "string", "description": "key 时按下的键名，如 Enter、Tab、ArrowDown、ArrowUp、Escape、Backspace 等" },
                "amount": { "type": "number", "description": "scroll 时滚动像素，正数向下、负数向上（默认 500）" },
                "ignore_cache": { "type": "boolean", "description": "reload 时是否忽略缓存（默认 false）" }
            },
            "required": ["action"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        // 兜底：headless_chrome 库内部在 Chrome 连接异常时会 unwrap 导致 panic，
        // 用 catch_unwind 把 panic 转成 Err，避免拖垮整个白泽进程。
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let action = args["action"].as_str().ok_or("缺少参数 action")?;
        let tab_id = args["tab_id"].as_str();

        match action {
            "goto" => {
                let tab = resolve_tab(tab_id)?;
                let url = args["url"].as_str().ok_or("缺少参数 url")?;
                navigate_and_wait(&tab, url, Duration::from_secs(45))?;
                Ok(json!({ "ok": true, "url": url, "tab_id": tab.get_target_id() }))
            }
            "click" => {
                let tab = resolve_tab(tab_id)?;
                let sel = args["selector"].as_str().ok_or("缺少参数 selector")?;
                // 顶层文档快速探测（3s）：元素在 iframe 里时让失败尽快发生，
                // 上层调度立即走跨框架深度点击兜底，避免白等 crate 默认 20s 超时。
                tab.wait_for_element_with_custom_timeout(sel, Duration::from_secs(3))
                    .map_err(|e| format!("等待元素失败: {e}"))?;
                tab.find_element(sel)
                    .map_err(|e| format!("查找元素失败: {e}"))?
                    .click()
                    .map_err(|e| format!("点击失败: {e}"))?;
                Ok(json!({ "ok": true }))
            }
            "type" => {
                let tab = resolve_tab(tab_id)?;
                let sel = args["selector"].as_str().ok_or("缺少参数 selector")?;
                let text = args["text"].as_str().unwrap_or("");
                // 同 click：快速探测，iframe 内元素交由上层深度填充兜底
                tab.wait_for_element_with_custom_timeout(sel, Duration::from_secs(3))
                    .map_err(|e| format!("等待元素失败: {e}"))?;
                // 先聚焦元素，再输入文本（headless_chrome 的 type_str 作用于当前焦点）
                tab.find_element(sel)
                    .map_err(|e| format!("查找元素失败: {e}"))?
                    .click()
                    .map_err(|e| format!("聚焦失败: {e}"))?;
                tab.type_str(text).map_err(|e| format!("输入失败: {e}"))?;
                Ok(json!({ "ok": true }))
            }
            "wait" => {
                let secs = args["seconds"].as_f64().unwrap_or(1.0).clamp(0.1, 30.0);
                std::thread::sleep(Duration::from_secs_f64(secs));
                Ok(json!({ "ok": true, "seconds": secs }))
            }
            "screenshot" => {
                let tab = resolve_tab(tab_id)?;
                let (path, _dpr) = capture_tab_png(&tab)?;
                Ok(json!({ "ok": true, "path": path }))
            }
            "evaluate" => {
                let tab = resolve_tab(tab_id)?;
                let js = args["js"].as_str().ok_or("缺少参数 js")?;
                let v = tab.evaluate(js, true).map_err(|e| format!("执行 JS 失败: {e}"))?;
                Ok(json!({ "ok": true, "result": v.value }))
            }
            "content" => {
                let tab = resolve_tab(tab_id)?;
                let html = tab.get_content().map_err(|e| format!("读取内容失败: {e}"))?;
                let text = extract_text(&html);
                Ok(json!({ "ok": true, "text": text }))
            }
            "look" => {
                let tab = resolve_tab(tab_id)?;
                let (path, _dpr) = capture_tab_png(&tab)?;
                // OCR 可能因未安装 Tesseract 而失败，此时仍返回截图路径与页面信息
                let text = match crate::ocr::ocr_detect_gui(&path) {
                    Ok((t, _)) => t,
                    Err(_) => String::new(),
                };
                Ok(json!({
                    "ok": true,
                    "url": tab.get_url(),
                    "title": tab.get_title().unwrap_or_default(),
                    "screenshot": path,
                    "text": text,
                }))
            }
            "click_text" => {
                let tab = resolve_tab(tab_id)?;
                let target = args["target"].as_str().ok_or("缺少参数 target")?;
                click_by_description(&tab, target)
            }
            "new_tab" => {
                let tab = new_chrome_tab()?;
                if let Some(url) = args["url"].as_str() {
                    navigate_and_wait(&tab, url, Duration::from_secs(45))?;
                }
                Ok(json!({ "ok": true, "tab_id": tab.get_target_id(), "url": tab.get_url() }))
            }
            "tabs" => {
                let active = active_target_id();
                let tabs = list_chrome_tabs()?;
                let items: Vec<Value> = tabs
                    .iter()
                    .map(|t| {
                        json!({
                            "id": t.get_target_id(),
                            "url": t.get_url(),
                            "title": t.get_title().unwrap_or_default(),
                            "active": Some(t.get_target_id().as_str()) == active.as_deref(),
                        })
                    })
                    .collect();
                Ok(json!({ "count": items.len(), "tabs": items }))
            }
            "switch_tab" => {
                let id = tab_id.ok_or("缺少参数 tab_id")?;
                let tab = find_chrome_tab(id)?;
                tab.activate().map_err(|e| format!("切换标签页失败: {e}"))?;
                set_active_target(Some(id.to_string()));
                Ok(json!({ "ok": true, "tab_id": id }))
            }
            "close_tab" => {
                let id = tab_id.ok_or("缺少参数 tab_id")?;
                let tab = find_chrome_tab(id)?;
                tab.close(false).map_err(|e| format!("关闭标签页失败: {e}"))?;
                if active_target_id().as_deref() == Some(id) {
                    set_active_target(None);
                }
                Ok(json!({ "ok": true }))
            }
            "scroll" => {
                let tab = resolve_tab(tab_id)?;
                let amount = args["amount"].as_f64().unwrap_or(500.0);
                let js = format!("window.scrollBy(0, {amount})");
                tab.evaluate(&js, false).map_err(|e| format!("滚动失败: {e}"))?;
                Ok(json!({ "ok": true, "amount": amount }))
            }
            "key" => {
                let tab = resolve_tab(tab_id)?;
                let key = args["key"].as_str().ok_or("缺少参数 key")?;
                tab.press_key(key).map_err(|e| format!("按键失败: {e}"))?;
                Ok(json!({ "ok": true }))
            }
            "reload" => {
                let tab = resolve_tab(tab_id)?;
                let ignore_cache = args["ignore_cache"].as_bool().unwrap_or(false);
                tab.reload(ignore_cache, None).map_err(|e| format!("刷新失败: {e}"))?;
                Ok(json!({ "ok": true }))
            }
            "back" => {
                let tab = resolve_tab(tab_id)?;
                tab.evaluate("history.back()", false).map_err(|e| format!("后退失败: {e}"))?;
                std::thread::sleep(Duration::from_millis(300));
                Ok(json!({ "ok": true, "url": tab.get_url() }))
            }
            "forward" => {
                let tab = resolve_tab(tab_id)?;
                tab.evaluate("history.forward()", false).map_err(|e| format!("前进失败: {e}"))?;
                std::thread::sleep(Duration::from_millis(300));
                Ok(json!({ "ok": true, "url": tab.get_url() }))
            }
            "state" => {
                let active = active_target_id();
                let tabs = list_chrome_tabs()?;
                let current = tabs
                    .iter()
                    .find(|t| Some(t.get_target_id().as_str()) == active.as_deref())
                    .map(|t| {
                        json!({
                            "id": t.get_target_id(),
                            "url": t.get_url(),
                            "title": t.get_title().unwrap_or_default(),
                        })
                    });
                Ok(json!({ "tab_count": tabs.len(), "active_tab": current }))
            }
            _ => Err(format!("不支持的操作: {action}")),
        }
    })).map_err(|_| "浏览器操控异常：桌面 Chrome 连接可能已中断（是否被手动关闭？），请稍后重试".to_string())?
}
}

/// browser_act 命令的全局串行锁：确保 Chrome 操控（截图 / OCR / 点击 / 跳转）串行执行，
/// 避免前端快速重复调用导致 headless_chrome 实例与活跃标签页状态竞态。
static ACT_LOCK: Mutex<()> = Mutex::new(());

/// 供前端「Chrome 操控面板」直接调用的统一入口：等价于 BrowserActTool 的 run。
/// 前端通过 `browser_act` 命令转发到这里，实现手动操控桌面 Chrome（仅面板用，不进入工具审批流程）。
/// 以全局互斥锁串行化每次操控，作为面板手动触发的轻量频率/并发限制。
pub fn act(args: Value) -> Result<Value, String> {
    let _guard = ACT_LOCK.lock().map_err(|_| "浏览器操控正忙，请稍后重试".to_string())?;
    BrowserActTool.run(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_encode_decode_roundtrip() {
        let s = "Tauri 2 优缺点";
        let encoded = percent_encode(s);
        let decoded = percent_decode(&encoded);
        assert_eq!(decoded, s);
    }

    #[test]
    fn decode_uddg_extracts_target() {
        let href = "//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2F&rut=abc";
        assert_eq!(decode_uddg(href), "https://example.com/");
    }

    #[test]
    fn parse_duckduckgo_extracts_results() {
        let html = r#"<a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2F&rut=x">Example <b>Title</b></a><a class="result__snippet">This is <b>snippet</b> text</a>"#;
        let results = parse_duckduckgo(html);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, "https://example.com/");
        assert_eq!(results[0].title, "Example Title");
        assert_eq!(results[0].summary, "This is snippet text");
    }

    #[test]
    fn parse_bing_b_algo_blocks_align_summary() {
        let html = r#"<ul><li class="b_algo"><h2><a href="https://a.com">标题甲</a></h2><p>摘要甲</p></li><li class="b_algo"><h2><a href="https://b.com">标题乙</a></h2><p>摘要乙</p></li></ul>"#;
        let results = parse_bing(html);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].url, "https://a.com");
        assert_eq!(results[0].summary, "摘要甲");
        assert_eq!(results[1].summary, "摘要乙");
    }

    #[test]
    fn parse_baidu_multiline_abstract() {
        // 摘要跨行：旧正则无 (?s) 会匹配失败
        let html = "<h3 class=\"t\"><a href=\"https://x.com\">标题</a></h3>\n<div class=\"c-abstract\">\n跨行摘要内容\n</div>";
        let results = parse_baidu(html);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, "https://x.com");
        assert_eq!(results[0].summary, "跨行摘要内容");
    }

    #[test]
    fn has_cjk_detects_language() {
        assert!(has_cjk("最新 AI 新闻"));
        assert!(has_cjk("テスト"));
        assert!(!has_cjk("rust async runtime"));
    }

    #[test]
    fn extract_text_strips_tags() {
        let html = r#"<html><head><title>Hello</title><style>body{}</style><script>alert(1)</script></head><body><p>正文内容</p></body></html>"#;
        let text = extract_text(html);
        assert!(text.contains("Hello"));
        assert!(text.contains("正文内容"));
        assert!(!text.contains("alert"));
    }

    fn word(text: &str, x: f64, y: f64, w: f64, h: f64) -> Value {
        json!({ "text": text, "x": x, "y": y, "w": w, "h": h, "conf": "95" })
    }

    /// 停用词不应作为定位锚点：「More information」拆出的 "More" 撞上正文 "Learn more"，
    /// 而真正有信息量的 "information" 不在词框中，此时应返回空（交给更强的一级兜底），
    /// 而非误点 "more"。
    #[test]
    fn match_ocr_word_filters_stopwords() {
        let words = vec![
            word("Learn", 100.0, 200.0, 50.0, 20.0),
            word("more", 160.0, 200.0, 40.0, 20.0),
        ];
        assert!(match_ocr_word(&words, "More information").is_empty());
    }

    /// 非停用词整词命中：目标 "information" 应命中词框 "information" 的原始框。
    #[test]
    fn match_ocr_word_token_hit_non_stopword() {
        let words = vec![word("information", 10.0, 20.0, 100.0, 20.0)];
        let hits = match_ocr_word(&words, "More information");
        assert_eq!(hits, vec![(10, 20, 100, 20)], "仅非停用词 information 命中");
    }

    /// 整词框完全相等命中：目标 "Login" 精确命中词框（优先级最高），框坐标原样返回。
    #[test]
    fn match_ocr_word_exact_hit() {
        let words = vec![word("Login", 100.0, 100.0, 60.0, 30.0)];
        assert_eq!(match_ocr_word(&words, "Login"), vec![(100, 100, 60, 30)]);
    }
}
