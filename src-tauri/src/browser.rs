use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use regex::Regex;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

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

/// 内嵌可用性判定（打开 url 标签页时对目标站点做响应头探测，产出确定性结论，
/// 而不是靠前端「3.5 秒没加载完就猜」的启发式）。
#[derive(Clone, serde::Serialize)]
pub struct EmbedCheck {
    /// ok=可正常内嵌 | blocked=确定被拒绝/不适用内嵌 | unknown=探测失败（网络/超时），需回退启发式
    pub status: String,
    /// 人类可读原因（blocked / unknown 时非空）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// 命中的具体信号：响应头原文或失败阶段（供白泽与用户判断依据）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
}

impl EmbedCheck {
    fn ok() -> Self {
        Self { status: "ok".into(), reason: None, signal: None }
    }
    fn blocked(reason: &str, signal: &str) -> Self {
        Self {
            status: "blocked".into(),
            reason: Some(reason.to_string()),
            signal: Some(signal.to_string()),
        }
    }
    fn unknown(reason: &str) -> Self {
        Self { status: "unknown".into(), reason: Some(reason.to_string()), signal: None }
    }
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
    /// 仅 url 标签页：内嵌可用性判定（探测结果），供白泽感知「该站点打不开」的原因
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embed: Option<EmbedCheck>,
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
            embed: None,
        });
        id
    }

    /// 写入某标签页的内嵌判定结果（探测在 open_tab 之后进行，避免持锁做网络请求）
    pub fn set_tab_embed(&mut self, id: &str, embed: EmbedCheck) {
        if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
            t.embed = Some(embed);
        }
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

/// 图标兜底文字：取站点主域名段（baidu.com→baidu，m.baidu.com→baidu，36kr.com→36kr），最多 7 字符
fn site_label(domain: &str) -> String {
    let parts: Vec<&str> = domain.split('.').collect();
    const PREFIX: [&str; 9] = ["www", "m", "wap", "mobile", "news", "so", "search", "blog", "shop"];
    let mut idx = 0;
    if parts.len() > 1 && PREFIX.contains(&parts[0].to_ascii_lowercase().as_str()) {
        idx = 1;
    }
    parts[idx].chars().take(7).collect()
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
        let site_name = site_label(&domain);
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
            "<a class='item' style='animation-delay:{}ms' href='{}'>\
             <span class='num'>{}</span>\
             <span class='ava' style='background:linear-gradient(135deg,{c1},{c2})'>\
             <img alt='' loading='lazy' src='{}' \
             onload=\"this.style.opacity='1';if(this.nextElementSibling)this.nextElementSibling.style.display='none'\" \
             onerror=\"this.style.display='none'\">\
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
         overflow:hidden;text-overflow:ellipsis;white-space:nowrap;text-shadow:0 1px 2px rgba(0,0,0,.35)}}\
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
         <div class='foot'>白泽多引擎聚合搜索 · 点击卡片在白泽浏览器内打开来源，切回本页即返回</div>\
         <script>document.addEventListener('click',function(e){{\
         var a=e.target&&e.target.closest?e.target.closest('a.item'):null;if(!a)return;\
         e.preventDefault();parent.postMessage({{type:'baize-open-url',url:a.getAttribute('href')}},'*');}});</script>\
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

// ───────────────────────── 内嵌可用性探测（该站点能否被 iframe 承载） ─────────────────────────

/// 探测目标站点能否被 iframe 内嵌，产出**确定性**结论。
///
/// 为什么需要：内置浏览器用 iframe 承载网页，跨源下 JS 拿不到被拒绝的原因
/// （命中 X-Frame-Options 时浏览器只是不渲染，`onerror` 也不触发），前端只能靠
/// 「3.5 秒没 load 就猜」——慢站点会误报、白屏反而漏报。这里直接读响应头：
///   - `X-Frame-Options: DENY / SAMEORIGIN / ALLOW-FROM`
///   - `Content-Security-Policy: frame-ancestors 'none' / 'self' / 白名单`
///   - `Content-Type` 为 PDF / 下载类二进制（iframe 内白屏或触发下载）
///
/// 请求策略（实测校准）：
///   1. HEAD（8s 超时）；
///   2. 站点拒绝 HEAD（403/405/501）→ 换 GET；
///   3. HEAD 拿到的 Content-Type 不是 HTML（如知乎 HEAD 被反爬敷衍成 `text/plain`，
///      真实头里才有 `X-Frame-Options: SAMEORIGIN`）→ 用 GET 复核一次取真实响应头。
/// 全程只读响应头，不读正文。网络失败一律返回 unknown——宁可让前端回退启发式，
/// 也不误判为 blocked 而白弹受控浏览器。
fn probe_embed(url: &str) -> EmbedCheck {
    let url = url.to_string();
    std::thread::spawn(move || -> EmbedCheck {
        let client = match build_client_opts(Duration::from_secs(8), true) {
            Ok(c) => c,
            Err(e) => return EmbedCheck::unknown(&format!("无法建立探测请求: {e}")),
        };
        let head = match client.head(&url).send() {
            Ok(r) => r,
            Err(e) => return EmbedCheck::unknown(&format!("站点探测请求失败: {e}")),
        };
        // 站点拒绝 HEAD：GET 才是可信来源，直接用它的判定
        if matches!(head.status().as_u16(), 403 | 405 | 501) {
            if let Ok(r) = client.get(&url).send() {
                return judge_embed_headers(r.headers());
            }
            return judge_embed_headers(head.headers());
        }
        let verdict = judge_embed_headers(head.headers());
        // HEAD 无法判定内嵌性（Content-Type 非 HTML）：多半是反爬/WAF 对 HEAD 的敷衍响应，
        // 用 GET 复核一次，以真实响应头为准
        if verdict.status == "ok" && needs_get_recheck(head.headers()) {
            if let Ok(r) = client.get(&url).send() {
                return judge_embed_headers(r.headers());
            }
        }
        verdict
    })
    .join()
    .unwrap_or_else(|_| EmbedCheck::unknown("探测线程异常退出"))
}

/// HEAD 响应是否「不足以判定内嵌性」：没有 Content-Type，或不是 HTML。
/// 这类结果（text/plain / image/* …）不代表站点真的不能内嵌，需要用 GET 复核。
fn needs_get_recheck(headers: &reqwest::header::HeaderMap) -> bool {
    let ct = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    !(ct.contains("text/html") || ct.contains("application/xhtml"))
}

/// 按响应头判定内嵌可用性（纯函数，便于单测）
fn judge_embed_headers(headers: &reqwest::header::HeaderMap) -> EmbedCheck {
    let get = |k: &str| headers.get(k).and_then(|v| v.to_str().ok()).unwrap_or("");

    // 1) X-Frame-Options —— 最经典的拒绝内嵌信号
    let xfo = get("x-frame-options").trim().to_ascii_lowercase();
    if xfo.starts_with("deny") {
        return EmbedCheck::blocked(
            "该站点设置了 X-Frame-Options: DENY，禁止任何页面把它嵌进 iframe",
            &format!("X-Frame-Options: {xfo}"),
        );
    }
    if xfo.starts_with("sameorigin") {
        return EmbedCheck::blocked(
            "该站点设置了 X-Frame-Options: SAMEORIGIN，只允许同源页面内嵌",
            &format!("X-Frame-Options: {xfo}"),
        );
    }
    if xfo.starts_with("allow-from") {
        return EmbedCheck::blocked(
            "该站点设置了 X-Frame-Options: ALLOW-FROM（现代浏览器已不支持，等同拒绝内嵌）",
            &format!("X-Frame-Options: {xfo}"),
        );
    }

    // 2) CSP frame-ancestors —— 新一代等价机制（可能有多条 CSP 头，逐条查）
    for (k, v) in headers.iter() {
        if !k.as_str().eq_ignore_ascii_case("content-security-policy") {
            continue;
        }
        let Ok(val) = v.to_str() else { continue };
        for directive in val.split(';') {
            let d = directive.trim();
            if !d.to_ascii_lowercase().starts_with("frame-ancestors") {
                continue;
            }
            let rest = d["frame-ancestors".len()..].trim().to_ascii_lowercase();
            // `frame-ancestors *` 是明确放行，直接跳过
            if rest.split_whitespace().any(|s| s == "*") {
                continue;
            }
            let reason = if rest.contains("'none'") {
                "该站点 CSP 设置 frame-ancestors 'none'，禁止任何页面内嵌"
            } else {
                "该站点 CSP 设置 frame-ancestors 来源白名单，只允许指定来源内嵌"
            };
            return EmbedCheck::blocked(
                reason,
                &format!("Content-Security-Policy: frame-ancestors {rest}"),
            );
        }
    }

    // 3) 明确无法在跨源 iframe 内渲染的内容：PDF 与「下载类」二进制。
    //    只收录确定渲染不了的类型——text/plain、json、图片、音视频在 iframe 里其实能正常显示，
    //    误判成 blocked 会让本来能看的页面被弹去受控浏览器。
    const NON_RENDERABLE: &[&str] = &[
        "application/pdf",
        "application/octet-stream",
        "application/zip",
        "application/x-zip",
        "application/x-7z-compressed",
        "application/x-rar",
        "application/x-msdownload",
        "application/x-msi",
        "application/vnd.android.package-archive",
    ];
    let ct = get("content-type").trim().to_ascii_lowercase();
    let mime = ct.split(';').next().unwrap_or("").trim().to_string();
    if !mime.is_empty() && NON_RENDERABLE.contains(&mime.as_str()) {
        return EmbedCheck::blocked(
            "该地址是文件（PDF / 压缩包 / 安装包等），iframe 内只会白屏或触发下载",
            &format!("Content-Type: {ct}"),
        );
    }

    EmbedCheck::ok()
}

// ───────────── 元素定位线框（直接在网页里画一条线，随操作同步出现） ─────────────
//
// 历史包袱：最初把方框画在「屏幕全屏透明覆盖层」上，需要把「元素在视口里的坐标」
// 换算成「元素在物理屏幕上的坐标」——中间要过窗口外框 / 标题栏 / 工具栏偏移、
// 目标窗口最小化、覆盖层 WebView 尚未加载完（首次 1~2s 事件被静默丢弃）四道坎，
// 任何一道出问题用户看到的都是「什么都没有」。现在改为**在页面内注入**一个
// position:fixed 的线框：坐标天然对齐（getBoundingClientRect 就是同一坐标系）、
// 一次 evaluate 几毫秒上屏、滚动/移窗都跟着元素走，也不再有屏幕像素定位的误差。

/// 页内线框元素的 id
const MARK_ID: &str = "__baize_mark__";

/// 页内线框的**公共样式函数**（被下面两段脚本复用）：只在页面里维护一个
/// `div#__baize_mark__`，反复使用；`hold` 毫秒后自动 `display:none` 隐藏。
/// 样式刻意做到最简：一条 2px 红实线，无动画、无标签、无特效。
/// 用 `setProperty(...,'important')` 落样式，避免被目标页面的 CSS 覆盖掉。
const MARK_STYLE_FN: &str = r#"var __baizeMark=function(x,y,w,h,hold){
if(typeof document==='undefined'||!document.documentElement)return 0;
var id='__BAIZE_ID__';
var d=document.getElementById(id);
if(!d){d=document.createElement('div');d.id=id;(document.body||document.documentElement).appendChild(d);}
var s=d.style,p=function(k,v){s.setProperty(k,v,'important');};
p('position','fixed');p('pointer-events','none');p('z-index','2147483647');
p('box-sizing','border-box');p('display','block');p('background','transparent');
p('margin','0');p('padding','0');p('border','2px solid #ff3b30');p('border-radius','2px');
p('left',x+'px');p('top',y+'px');p('width',w+'px');p('height',h+'px');
clearTimeout(window.__baize_mark_t__);
window.__baize_mark_t__=setTimeout(function(){var o=document.getElementById(id);
if(o){o.style.setProperty('display','none','important');}},hold);
return 1;}"#;

/// 按坐标画线（坐标 = 视口 CSS 像素）
const MARK_AT_JS: &str = r#"(function(){
__BAIZE_STYLE__
return __baizeMark(__BAIZE_X__,__BAIZE_Y__,__BAIZE_W__,__BAIZE_H__,__BAIZE_HOLD__);})()"#;

/// 按 CSS 选择器画线：**一次 evaluate 同时完成「取矩形 + 画线」**。
/// 早先这里要两次往返（`element_rect_css` 取矩形、再注入标记），合并后每次
/// 点击/输入少一趟 CDP。
/// 元素在视口外时先 scrollIntoView（headless_chrome 的 click 本来也会滚，提前滚
/// 只为让线框画在看得见的位置，否则 position:fixed 的线会被画到屏幕外）。
/// 返回 `JSON.stringify([left,top,width,height])`，找不到元素或元素退化返回 `"null"`。
const MARK_SELECTOR_JS: &str = r#"(function(){
if(typeof document==='undefined'||!document.documentElement)return 'null';
__BAIZE_STYLE__
var e=document.querySelector(__BAIZE_SEL__);
if(!e)return 'null';
var r=e.getBoundingClientRect();
if(!(r.width>=2&&r.height>=2))return 'null';
// 同样只在元素真的不在视口里时才滚，且只滚最小距离——不为了画一条线去动用户的屏幕
if(!(r.top>=-2&&r.bottom<=innerHeight+2&&r.left>=-2&&r.right<=innerWidth+2)){
  try{e.scrollIntoView({block:'nearest',inline:'nearest'});}catch(err){}r=e.getBoundingClientRect();}
if(!(r.width>=2&&r.height>=2))return 'null';
__baizeMark(r.left,r.top,r.width,r.height,__BAIZE_HOLD__);
return JSON.stringify([r.left,r.top,r.width,r.height]);})()"#;

/// 定位式滚动：把目标元素滚到视口内（已经可见就不动），返回它滚动后的矩形。
/// 「滚动」应该是**为了找东西/让目标可见**才发生，而不是盲滚固定像素——
/// 盲滚会把页面丢到用户没在看的位置，还得再滚回来。
const SCROLL_SELECTOR_JS: &str = r#"(function(){
if(typeof document==='undefined'||!document.documentElement)return 'null';
var e=document.querySelector(__BAIZE_SEL__);
if(!e)return 'null';
var r=e.getBoundingClientRect();
var need=(r.top<-2||r.bottom>innerHeight+2||r.left<-2||r.right>innerWidth+2);
if(need){try{e.scrollIntoView({block:'nearest',inline:'nearest'});}catch(err){}
  r=e.getBoundingClientRect();}
return JSON.stringify([r.left,r.top,r.width,r.height,need,window.scrollY||0]);})()"#;

/// DOM 文字定位脚本：**一次 evaluate，没有截图、没有 OCR、没有视觉模型**。
///
/// 这是 `click_text` / `highlight(target)` 的第一级。原先无论目标是什么都先截全屏图、
/// 跑一遍 Windows OCR（0.2~0.5s）、命中不了还要再过 Set-of-Marks 或视觉模型（秒级）——
/// 而「点登录 / 点搜索 / 点下一步」绝大多数在 DOM 里就能唯一定位，一次 evaluate 十几毫秒
/// 就够，而且比像素识别更准。只有 DOM 里找不到（文字是图片、在 iframe 里、纯图标按钮）
/// 才回落到原来的 OCR 链。
///
/// 打分选最像「那个控件」的元素：文本精确匹配 0 分 ＜ aria-label/title/placeholder 精确 1 分
/// ＜ 文本包含（文本越短越像按钮本身）＜ label/value 包含；
/// 再叠加两个轻量偏向：可交互元素（a/button/input/label/summary 或 role=button/link/tab…）
/// 减 0.25 分——同样叫「登录」时优先点按钮而不是同名的小标题；视口内元素减 0.5 分。
const FIND_BY_TEXT_JS: &str = r#"(function(){
if(typeof document==='undefined'||!document.documentElement)return 'null';
var want=(__BAIZE_TARGET__||'').toLowerCase();
if(!want)return 'null';
var norm=function(s){return (s||'').replace(/\s+/g,' ').trim();};
var sel='a,button,[role=button],[role=link],[role=tab],[role=menuitem],[role=option],'
 +'input[type=submit],input[type=button],input[type=reset],input[type=image],label,summary,'
 +'li,span,strong,em,b,i,p,h1,h2,h3,h4,h5,td,th,div';
var all=document.querySelectorAll(sel),best=null,bestScore=99,i,r;
for(i=0;i<all.length;i++){
  var e=all[i];
  r=e.getBoundingClientRect();
  if(!(r.width>=2&&r.height>=2))continue;
  var cs=getComputedStyle(e);
  if(cs.visibility==='hidden'||cs.display==='none'||parseFloat(cs.opacity||'1')<0.05)continue;
  if(e.disabled)continue;
  var txt=norm(e.innerText||e.textContent).toLowerCase();
  var val=norm(e.value).toLowerCase();
  var al=norm(e.getAttribute('aria-label')).toLowerCase();
  var ti=norm(e.getAttribute('title')).toLowerCase();
  var ph=norm(e.getAttribute('placeholder')).toLowerCase();
  var score=99;
  if(txt===want||val===want)score=0;
  else if(al===want||ti===want||ph===want)score=1;
  else if(txt.indexOf(want)>=0)score=2+Math.min(3,(e.innerText||e.textContent||'').length/24);
  else if(al.indexOf(want)>=0||ti.indexOf(want)>=0||ph.indexOf(want)>=0)score=6;
  else if(val.indexOf(want)>=0)score=7;
  if(score<99){
    var tag=(e.tagName||'').toLowerCase();
    var role=(e.getAttribute('role')||'').toLowerCase();
    if(tag==='a'||tag==='button'||tag==='input'||tag==='label'||tag==='summary'
       ||tag==='select'||tag==='textarea'||role==='button'||role==='link'
       ||role==='tab'||role==='menuitem'||role==='option')score-=0.25;
    if(r.top>=-2&&r.bottom<=innerHeight+2&&r.left>=-2&&r.right<=innerWidth+2)score-=0.5;
    if(score<bestScore){bestScore=score;best=e;}
  }
}
if(!best)return 'null';
r=best.getBoundingClientRect();
// **只在需要时才滚**，而且滚最小的距离（block:'nearest'）。
// 这里原来是**无条件** scrollIntoView({block:'center'})：只要按文字点一下，就把元素硬拽到
// 屏幕正中——元素本来就在眼前也照拽不误，用户看到的就是「白泽点一下页面自己滑一下」。
// 定位本身不需要滚屏，滚屏只应该发生在「目标确实不在视口里」的时候。
var need=(r.top<-2||r.bottom>innerHeight+2||r.left<-2||r.right>innerWidth+2);
if(need){try{best.scrollIntoView({block:'nearest',inline:'nearest'});}catch(err){}r=best.getBoundingClientRect();}
if(!(r.width>=2&&r.height>=2))return 'null';
return JSON.stringify({x:r.left,y:r.top,w:r.width,h:r.height,scrolled:need,
  text:norm(best.innerText||best.textContent||best.value||'').slice(0,40),
  tag:(best.tagName||'').toLowerCase()});})()"#;

/// 页面元素扫描脚本：**一次 evaluate** 枚举当前视口内的可交互元素，并在页面里给每个
/// 元素画一个**编号线框**（`1..n`），返回编号清单。
///
/// 这是「先把可点的地方列出来，再决定点哪个」的模式——与 Browser Use / SOM 的体验相同，
/// 但**不截图、不跑 OCR、不调视觉模型**：候选元素和它的文字都是 DOM 里直接读出来的，
/// 编号与元素的对应也留在 DOM（`data-baize-mark` 属性）里，因此
/// ① 快（枚举 + 画框通常几十毫秒，纯前端）；② 没有「编号 → 坐标」的映射误差，
/// 后续 `click_mark` 按编号取元素、取到的就是当初框出来的那一个。
///
/// `__BAIZE_SCOPE__` 给文字时先定位「那个模块」的容器（id / aria-label / class / 文本），
/// 找就枚举它内部；找不到则退回整页并在返回里标明（`scope_miss`），不静默换范围。
const SCAN_JS: &str = r#"(function(){
if(typeof document==='undefined'||!document.documentElement)return 'null';
var norm=function(s){return (s||'').replace(/\s+/g,' ').trim();};
var vw=window.innerWidth||0,vh=window.innerHeight||0;
// ① 清掉上一次的标记（框 + 元素上的编号）
var old=document.getElementById('__baize_marks__');
if(old&&old.parentNode)old.parentNode.removeChild(old);
var oldEls=document.querySelectorAll('[data-baize-mark]');
for(var q=0;q<oldEls.length;q++){try{oldEls[q].removeAttribute('data-baize-mark');}catch(e){}}
// ② 划定范围
var root=document.documentElement,scope=__BAIZE_SCOPE__,scopeMiss=false,scopeName='(整页)';
if(scope){
  var want=String(scope).toLowerCase(),best=null,bestScore=1e9;
  var boxes=document.querySelectorAll('aside,nav,section,header,footer,main,form,div,ul,[role=navigation],[role=region],[role=tabpanel],[role=dialog],[role=complementary],[role=toolbar]');
  for(var i=0;i<boxes.length&&i<1500;i++){
    var e0=boxes[i],r0=e0.getBoundingClientRect();
    if(r0.width<120||r0.height<120)continue;
    // 整页级容器不算「模块」——除非 id/aria-label 精确命中
    var id=norm(e0.id).toLowerCase(),al=norm(e0.getAttribute('aria-label')).toLowerCase();
    var exact=(id===want||al===want);
    if(!exact&&r0.width>=vw*0.98&&r0.height>=vh*0.98)continue;
    var cl=norm(e0.className&&e0.className.toString?e0.className.toString():'').toLowerCase();
    var s=1e9;
    if(exact)s=0;
    else if(id.indexOf(want)>=0||al.indexOf(want)>=0)s=1;
    else if(cl.indexOf(want)>=0)s=3;
    else{var tx=(e0.textContent||'').toLowerCase();if(tx.length<20000&&tx.indexOf(want)>=0)s=5+Math.min(3,tx.length/2000);}
    if(s<1e9&&s<bestScore){bestScore=s;best=e0;}
  }
  if(best){root=best;scopeName='(模块内)';
    // 指定了模块就先把模块带进视口：否则枚举出来的元素全在视口外、编号框一个也画不出来，
    // 用户看到的就是「说了扫这个模块却什么都没框」。只在模块确实不在视口里时才滚（最小距离）。
    try{var rb=best.getBoundingClientRect();
      if(rb.top<0||rb.bottom>vh||rb.left<0||rb.right>vw){best.scrollIntoView({block:'nearest',inline:'nearest'});}}catch(err){}
  }else{scopeMiss=true;scopeName='(未匹配到该模块，已退回整页)';}
}
// ③ 枚举可交互元素 + 画编号框
var sel='a[href],button,input:not([type=hidden]),select,textarea,summary,'
 +'[role=button],[role=link],[role=tab],[role=menuitem],[role=option],[role=checkbox],[role=radio],[role=switch],'
 +'[onclick],[contenteditable="true"],[tabindex]:not([tabindex="-1"])';
var list=root.querySelectorAll(sel),limit=__BAIZE_LIMIT__;
var host=document.createElement('div');host.id='__baize_marks__';
var hp=function(k,v){host.style.setProperty(k,v,'important');};
hp('position','fixed');hp('left','0');hp('top','0');hp('width','0');hp('height','0');
hp('margin','0');hp('padding','0');hp('pointer-events','none');hp('z-index','2147483647');
var items=[],n=0,offAbove=0,offBelow=0;
for(var i=0;i<list.length;i++){
  if(n>=limit)break;
  var e=list[i],r=e.getBoundingClientRect();
  if(!(r.width>=8&&r.height>=8))continue;
  var inView=!(r.bottom<0||r.top>vh||r.right<0||r.left>vw);
  var cs=getComputedStyle(e);
  if(cs.visibility==='hidden'||cs.display==='none'||parseFloat(cs.opacity||'1')<0.05)continue;
  if(e.disabled)continue;
  // 只列视口内的（框得出来也点得到）；视口外的**只计数**——把「下面还有 3 个、上面还有 1 个」
  // 告诉白泽，它才知道到底要不要滚、往哪滚；否则它只能为了「找找看」把页面盲滚一遍。
  if(!inView){if(r.top>=vh)offBelow++;else offAbove++;continue;}
  var label=norm(e.innerText||e.textContent||e.value||e.getAttribute('aria-label')||e.getAttribute('placeholder')||e.getAttribute('title')||'');
  label=label.slice(0,50);
  if(!label){var t2=norm(e.getAttribute('type')||e.getAttribute('name')||'');label=t2?('['+t2+']'):'';}
  n++;
  try{e.setAttribute('data-baize-mark',String(n));}catch(err){}
  var b=document.createElement('div'),bs=b.style;
  var bp=function(k,v){bs.setProperty(k,v,'important');};
  bp('position','fixed');bp('left',r.left+'px');bp('top',r.top+'px');
  bp('width',r.width+'px');bp('height',r.height+'px');
  bp('border','1px solid #ff3b30');bp('box-sizing','border-box');
  bp('margin','0');bp('padding','0');bp('background','transparent');bp('border-radius','2px');
  var num=document.createElement('span'),ns=num.style;
  var np=function(k,v){ns.setProperty(k,v,'important');};
  np('position','absolute');np('font','10px/12px monospace');np('background','#ff3b30');
  np('color','#fff');np('padding','0 3px');np('border-radius','2px');
  if(r.top>=16){np('left','-1px');np('top','-14px');}else{np('left','2px');np('top','2px');}
  num.textContent=String(n);
  b.appendChild(num);
  host.appendChild(b);
  items.push({n:n,tag:(e.tagName||'').toLowerCase(),type:(e.getAttribute('type')||''),text:label,
    x:Math.round(r.left),y:Math.round(r.top),w:Math.round(r.width),h:Math.round(r.height)});
}
(document.body||document.documentElement).appendChild(host);
clearTimeout(window.__baize_marks_t__);
window.__baize_marks_t__=setTimeout(function(){
  var o=document.getElementById('__baize_marks__');
  if(o)o.style.setProperty('display','none','important');
},__BAIZE_HOLD__);
return JSON.stringify({count:items.length,scope_miss:scopeMiss,scope:scopeName,
  off_view:{above:offAbove,below:offBelow},items:items});})()"#;

/// 扫描编号框的保留时长：比单次操作的线框长得多（用户要看清一屏编号再决定点哪个）
const MARK_HOLD_SCAN: u64 = 8000;

/// 毫秒时间戳（仅用于本地节流 / 缓存判断，不参与任何业务语义）
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 最近一次「确认标签页窗口可见」的时间戳（毫秒，0 = 未知）。
/// 窗口可见性是低频变化的状态，而画线/点击是高频动作——每次都探一次会白搭一趟 CDP。
/// 5 秒内已确认可见就直接放行，只在缓存过期或探测失败时才真正查 + 必要时 bringToFront。
static TAB_VISIBLE_AT: AtomicU64 = AtomicU64::new(0);

/// 画线框前确保标签页窗口是可见的。
///
/// 受控浏览器窗口被最小化时（用户手动最小化、或 GUI 任务的 window_minimize_all 顺手
/// 收掉），`screenX/screenY` 会返回约 -32000（Windows 惯例）——线框画在页面里同样看不见，
/// 用户既不知道白泽在动什么，还会以为「什么都没做就点了」。这里用 CDP 的
/// Page.bringToFront 把窗口带回前台（Chrome 会顺带取消最小化）。
/// 返回 false = 恢复不了，此时跳过标记（点击/输入照常执行，不阻塞任务）。
fn ensure_tab_window_visible(tab: &Arc<headless_chrome::Tab>) -> bool {
    // 刚确认过可见（5s 内）→ 直接放行，省掉这趟探测
    let now = now_ms();
    if now.saturating_sub(TAB_VISIBLE_AT.load(Ordering::Relaxed)) < 5000 {
        return true;
    }
    let probe = |t: &Arc<headless_chrome::Tab>| -> Option<(f64, f64)> {
        let raw = t
            .evaluate(
                "JSON.stringify([window.screenX||0,window.screenY||0])",
                false,
            )
            .ok()?
            .value?;
        let arr: Value = serde_json::from_str(raw.as_str()?).ok()?;
        Some((arr.get(0)?.as_f64()?, arr.get(1)?.as_f64()?))
    };
    let minimised = |p: &(f64, f64)| p.0 < -10000.0 || p.1 < -10000.0;
    if let Some(p) = probe(tab) {
        if !minimised(&p) {
            TAB_VISIBLE_AT.store(now, Ordering::Relaxed);
            return true;
        }
    }
    let _ = tab.bring_to_front();
    std::thread::sleep(Duration::from_millis(250));
    match probe(tab) {
        Some(p) if !minimised(&p) => {
            crate::windows::diag_log("[定位线框] 受控浏览器窗口原处于最小化，已带回前台");
            TAB_VISIBLE_AT.store(now_ms(), Ordering::Relaxed);
            true
        }
        _ => {
            crate::windows::diag_log("[定位线框] 受控浏览器窗口最小化且无法恢复，跳过标记");
            false
        }
    }
}

/// 把线框样式函数塞进脚本模板并替换元素 id
fn mark_template(tpl: &str) -> String {
    let style = MARK_STYLE_FN.replace("__BAIZE_ID__", MARK_ID);
    tpl.replace("__BAIZE_STYLE__", &style)
}

/// 画一条线框住目标元素：坐标 = **相对视口的 CSS 像素**（`getBoundingClientRect` 的返回值）。
/// `hold_ms` 到期后自动隐藏（隐藏而非删除——复用同一个 div，连续操作时线框「跟着跳」
/// 而不是闪一下重建）。
fn mark_rect_css(tab: &Arc<headless_chrome::Tab>, x: f64, y: f64, w: f64, h: f64, hold_ms: u64) {
    // 退化框（display:none / 零尺寸元素）不画，避免屏幕角落出现一个莫名其妙的小点
    if !x.is_finite() || !y.is_finite() || !(w >= 2.0) || !(h >= 2.0) {
        return;
    }
    if !ensure_tab_window_visible(tab) {
        return;
    }
    let js = mark_template(MARK_AT_JS)
        .replace("__BAIZE_HOLD__", &hold_ms.to_string())
        .replace("__BAIZE_X__", &format!("{:.2}", x))
        .replace("__BAIZE_Y__", &format!("{:.2}", y))
        .replace("__BAIZE_W__", &format!("{:.2}", w))
        .replace("__BAIZE_H__", &format!("{:.2}", h));
    let _ = tab.evaluate(&js, false);
}

/// 按 CSS 选择器**一次 evaluate** 完成「取矩形 + 画线」。
/// 返回元素矩形（视口 CSS 像素）；元素不存在 / 尺寸退化 / 窗口不可见时返回 None。
fn mark_selector_css(
    tab: &Arc<headless_chrome::Tab>,
    selector: &str,
    hold_ms: u64,
) -> Option<(f64, f64, f64, f64)> {
    let Ok(sel) = serde_json::to_string(selector) else {
        return None;
    };
    if !ensure_tab_window_visible(tab) {
        return None;
    }
    let js = mark_template(MARK_SELECTOR_JS)
        .replace("__BAIZE_SEL__", &sel)
        .replace("__BAIZE_HOLD__", &hold_ms.to_string());
    let raw = tab.evaluate(&js, false).ok()?.value?;
    let s = raw.as_str()?;
    if s == "null" {
        return None;
    }
    let arr: Value = serde_json::from_str(s).ok()?;
    Some((
        arr.get(0)?.as_f64()?,
        arr.get(1)?.as_f64()?,
        arr.get(2)?.as_f64()?,
        arr.get(3)?.as_f64()?,
    ))
}

/// 画线框：坐标是「相对视口的**设备像素**」（与截图 / OCR / SOM 同一坐标系，需 ÷ dpr）
fn mark_rect_device(
    tab: &Arc<headless_chrome::Tab>,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    dpr: f64,
    hold_ms: u64,
) {
    let d = if dpr > 0.0 { dpr } else { 1.0 };
    mark_rect_css(tab, x / d, y / d, w / d, h / d, hold_ms);
}

/// 点位线框（视觉接地只给坐标、没有矩形时，用一个小方框标出落点）
fn mark_point_device(tab: &Arc<headless_chrome::Tab>, x: f64, y: f64, dpr: f64, hold_ms: u64) {
    const S: f64 = 44.0; // 设备像素
    mark_rect_device(tab, x - S / 2.0, y - S / 2.0, S, S, dpr, hold_ms);
}

/// 一次在 DOM 里按可见文字定位元素（**服务端零成本**：无截图、无 OCR、无视觉模型）。
/// 返回 (视口 CSS 像素矩形, 命中的元素文字, 标签名)。
fn dom_locate_by_text(
    tab: &Arc<headless_chrome::Tab>,
    target: &str,
) -> Option<(f64, f64, f64, f64, String, String)> {
    let t = serde_json::to_string(target).ok()?;
    let js = FIND_BY_TEXT_JS.replace("__BAIZE_TARGET__", &t);
    let raw = tab.evaluate(&js, false).ok()?.value?;
    let s = raw.as_str()?;
    if s == "null" {
        return None;
    }
    let v: Value = serde_json::from_str(s).ok()?;
    Some((
        v.get("x")?.as_f64()?,
        v.get("y")?.as_f64()?,
        v.get("w")?.as_f64()?,
        v.get("h")?.as_f64()?,
        v.get("text").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        v.get("tag").and_then(|x| x.as_str()).unwrap_or("").to_string(),
    ))
}

/// 「定位 → 画线 → 点击」的统一收尾（坐标为视口 CSS 像素）：先画线让用户看清，
/// 紧接着点元素中心。线框点击后仍会在页面上留 MARK_HOLD_ACT 毫秒。
fn click_rect_css(tab: &Arc<headless_chrome::Tab>, x: f64, y: f64, w: f64, h: f64) -> Result<(), String> {
    mark_rect_css(tab, x, y, w, h, MARK_HOLD_ACT);
    std::thread::sleep(MARK_LEAD);
    click_at(tab, x + w / 2.0, y + h / 2.0)
}

/// DOM 文字定位命中的统一收尾：画线 → 点中心 → 带上「命中到的是什么」。
fn finish_text_click(
    tab: &Arc<headless_chrome::Tab>,
    target: &str,
    hit: (f64, f64, f64, f64, String, String),
) -> Result<Value, String> {
    let (x, y, w, h, text, tag) = hit;
    click_rect_css(tab, x, y, w, h)?;
    Ok(json!({
        "ok": true, "method": "dom", "x": x + w / 2.0, "y": y + h / 2.0,
        "target": target, "matched": format!("{tag}:{text}"),
    }))
}

/// 扫描「当前页面 / 指定模块」内可交互元素并在页面里标出**编号线框**，返回编号清单。
///
/// 全部在**一次 evaluate** 里完成（枚举 + 画框 + 记编号），没有截图、没有 OCR、
/// 没有视觉模型，页面上也不会多出覆盖层——所以它是一次几十毫秒的纯前端动作，
/// 不会把「连贯性」拖没了。编号与元素的对应同时写进 DOM（`data-baize-mark`），
/// 后续 `click_mark(index)` 按编号取到的就是当初被框出来的那一个元素。
fn scan_interactive(
    tab: &Arc<headless_chrome::Tab>,
    scope: Option<&str>,
    limit: usize,
) -> Result<Value, String> {
    // 窗口最小化时框在页面上同样看不见，先试着带回前台（失败不阻断：清单本身仍有价值）
    ensure_tab_window_visible(tab);
    let scope_lit = match scope.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => serde_json::to_string(s).unwrap_or_else(|_| "null".into()),
        None => "null".into(),
    };
    let js = SCAN_JS
        .replace("__BAIZE_SCOPE__", &scope_lit)
        .replace("__BAIZE_LIMIT__", &limit.to_string())
        .replace("__BAIZE_HOLD__", &MARK_HOLD_SCAN.to_string());
    let raw = tab
        .evaluate(&js, false)
        .map_err(|e| format!("扫描页面元素失败: {e}"))?
        .value
        .ok_or("扫描页面元素失败：CDP 没有返回值")?;
    let s = raw
        .as_str()
        .ok_or("扫描页面元素失败：返回值不是字符串（页面可能已经跳走）")?;
    if s == "null" {
        return Err("扫描页面元素失败：当前文档不可用".into());
    }
    serde_json::from_str::<Value>(s).map_err(|e| format!("解析扫描结果失败: {e}"))
}

/// 读取顶层文档中某 CSS 选择器元素的矩形（相对视口 CSS 像素）。
/// 仅用于「不想画线、只要坐标」的场景；点击/输入路径请直接用 `mark_selector_css`（少一趟往返）。
#[allow(dead_code)]
fn element_rect_css(tab: &Arc<headless_chrome::Tab>, selector: &str) -> Option<(f64, f64, f64, f64)> {
    let sel = serde_json::to_string(selector).ok()?;
    let js = format!(
        "JSON.stringify((function(){{var e=document.querySelector({sel});if(!e)return null;\
         var r=e.getBoundingClientRect();return [r.left,r.top,r.width,r.height];}})())"
    );
    let raw = tab.evaluate(&js, false).ok()?.value?;
    let s = raw.as_str()?;
    if s == "null" {
        return None;
    }
    let arr: Value = serde_json::from_str(s).ok()?;
    Some((
        arr.get(0)?.as_f64()?,
        arr.get(1)?.as_f64()?,
        arr.get(2)?.as_f64()?,
        arr.get(3)?.as_f64()?,
    ))
}

/// 画线/操作之间的极短停顿：让线框先上屏，用户能看清「白泽动的是哪个元素」。
/// 页内注入只要几毫秒（不用再等覆盖层 WebView 就绪），这里只留一个感知衔接时间。
const MARK_LEAD: Duration = Duration::from_millis(120);

/// 点击 / 输入类动作的线框保留时长（动作之后仍留一会儿，让用户看清刚才动的是哪个元素）
const MARK_HOLD_ACT: u64 = 1200;

/// 只标记不点击（highlight）的保留时长
const MARK_HOLD_SHOW: u64 = 2600;

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
        "在独立的原生浏览器窗口中打开一个网址（可加载任意站点、绕过 iframe 嵌入限制）。当内置浏览器（白泽·浏览器）里的 url 标签页因网站安全策略（X-Frame-Options/CSP）拒绝内嵌、或需要完整登录/脚本环境而打不开时，用这个工具在独立浏览器窗口打开网页。注意：内置浏览器打开网址时会自动探测内嵌可用性，确定被拒绝时会自动改用受控桌面浏览器（browser_act）打开——要走「白泽能继续操控页面」的路线请直接用 browser_act 的 new_tab，本工具只负责用系统浏览器给用户看。"
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
        "在内置浏览器窗口中进行内容搜索（多引擎聚合，结果以可视化列表展示）。结果卡片可点击在浏览器内打开来源网页；信息取回后如不再需要浏览器，可用 browser_close_window 收起窗口"
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
        "在内置浏览器（白泽·浏览器）中打开一个新标签页（kind 可选 markdown/text/html/video/url）。每个内容一个标签页。展示网页/HTML 效果：优先用 kind=\"html\" 直接把完整 HTML 源码放进 content 渲染（内嵌 CSS/JS，无外部依赖），或浏览本地 .html 文件时直接把文件绝对路径放进 content（kind 用 url 或 html 均可，会自动读取文件内容渲染，规避 file:// 内嵌限制）。kind=\"url\" 仅用于打开 http/https 网址；打开网址时会自动探测该站点能否被内嵌（检查 X-Frame-Options / CSP frame-ancestors / 非 HTML 内容），返回值 embed.status=blocked 表示确定无法内嵌、embed.reason 是具体原因，此时内置浏览器会显示该原因并自动改用受控桌面浏览器打开，后续对页面的操作请用 browser_act。video 的 content 填视频文件绝对路径或在线视频 URL。\
【当前标签页会切走】新开的标签页会立即成为内置浏览器的当前标签页，原来那个被置为非激活；返回值里 active_tab 是新的当前页、previous_tab 是你刚才在操作的页面。如果原来那个页面还要继续用，先 browser_switch 切回去再操作；不确定当前是哪个标签页时用 browser_tabs（带 active 标记的就是当前页）。"
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

        // 打开网页前先探测内嵌可用性：命中 X-Frame-Options / CSP frame-ancestors /
        // 非 HTML 内容时给出确定性结论，前端据此自动转交受控桌面浏览器，白泽也能感知原因。
        // 探测放在加锁之前（网络 I/O 不持锁）。
        let embed: Option<EmbedCheck> = if kind == "url" && is_remote {
            Some(probe_embed(&content))
        } else {
            None
        };

        let (tab_id, previous) = {
            let mut s = self.state.lock().unwrap();
            // 打开前先记住「刚才在操作哪个标签页」：新标签页会顶替它成为当前页，
            // 不记住就没法在返回值里提醒模型「你刚离开的是哪一个」。
            let prev = s
                .tabs
                .iter()
                .find(|t| t.active)
                .map(|t| (t.id.clone(), t.title.clone(), t.kind.clone()));
            let id = s.open_tab(&kind, &title, &content);
            if let Some(e) = embed.clone() {
                s.set_tab_embed(&id, e);
            }
            (id, prev)
        };
        emit_update(&self.app, &self.state);

        // 新标签页会成为内置浏览器的当前页，原来那个被置为非激活。
        // 这里把「谁变成了当前页」和「你刚才在操作谁」一并告诉白泽——多标签页里最容易出的
        // 错不是打不开页面，而是**在别的标签页上继续发指令**，而界面上完全看不出来。
        let current = {
            let s = self.state.lock().unwrap();
            s.tabs.iter().find(|t| t.id == tab_id).map(|t| {
                json!({
                    "id": t.id,
                    "title": if t.title.is_empty() { t.kind.clone() } else { t.title.clone() },
                    "kind": t.kind,
                })
            })
        };

        let mut out = json!({ "ok": true, "tab": tab_id, "kind": kind });
        out["active_tab"] = current.unwrap_or(json!({ "id": tab_id }));
        if let Some(e) = &embed {
            out["embed"] = serde_json::to_value(e).unwrap_or(Value::Null);
        }
        let mut hints: Vec<String> = Vec::new();
        if embed.as_ref().map(|e| e.status == "blocked").unwrap_or(false) {
            hints.push(
                "该站点拒绝被 iframe 内嵌，内置浏览器已标注具体原因，并会自动改用受控桌面浏览器打开。\
                 后续对这个页面的点击/输入/读取请走受控浏览器（browser_act），不要再依赖内置 iframe。"
                    .into(),
            );
        }
        if let Some((prev_id, prev_title, _)) = &previous {
            hints.push(format!(
                "内置浏览器的当前标签页现在是新开的 {}；你刚才在操作的是 {}{}。\
                 如果原来那个页面还要继续用，先 browser_switch 切回它再发指令，\
                 不要在没确认当前标签页的情况下继续操作（browser_tabs 里带 active 标记的就是当前页）。",
                tab_id,
                prev_id,
                if prev_title.is_empty() { String::new() } else { format!("「{prev_title}」") }
            ));
        }
        if !hints.is_empty() {
            out["hint"] = json!(hints.join("\n"));
        }
        Ok(out)
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
        let id = args["id"].as_str().ok_or("缺少参数 id")?.to_string();
        let (was_active, next) = {
            let mut s = self.state.lock().unwrap();
            let was_active = s.tabs.iter().any(|t| t.id == id && t.active);
            s.close_tab(&id);
            let next = s.tabs.iter().find(|t| t.active).map(|t| {
                json!({
                    "id": t.id,
                    "title": if t.title.is_empty() { t.kind.clone() } else { t.title.clone() },
                    "kind": t.kind,
                })
            });
            (was_active, next)
        };
        emit_update(&self.app, &self.state);
        let mut out = json!({ "ok": true, "closed": id, "active_tab": next });
        // 关掉的正好是当前页时，明确告诉白泽「现在轮到哪个页面了」，
        // 免得它照着刚才的上下文继续对已经消失的页面发指令。
        if was_active {
            out["hint"] = json!(
                "你关掉的是内置浏览器的当前标签页，当前页已换成返回值里的 active_tab（可能为空）。\
                 接下来的操作以它为准；还有别的页面要处理时，先 browser_tabs 看清有哪几个标签页，再 browser_switch 切过去。"
            );
        }
        Ok(out)
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
        let cur = {
            let mut s = self.state.lock().unwrap();
            if !s.tabs.iter().any(|t| t.id == id) {
                return Err(format!(
                    "内置浏览器里没有标签页 {id}，用 browser_tabs 看现有标签页"
                ));
            }
            s.set_active_tab(id);
            s.tabs.iter().find(|t| t.id == id).map(|t| {
                json!({
                    "id": t.id,
                    "title": if t.title.is_empty() { t.kind.clone() } else { t.title.clone() },
                    "kind": t.kind,
                })
            })
        };
        emit_update(&self.app, &self.state);
        Ok(json!({
            "ok": true,
            "active_tab": cur,
            "note": format!("内置浏览器的当前标签页已切到 {id}，后续对内置浏览器的操作都以它为准。"),
        }))
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
    reset_tab_memory();
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

/// 默认探测的 CDP 端点（Chrome / Edge 远程调试的惯例端口）
const CDP_PROBE_DEFAULT: &str = "http://127.0.0.1:9222";

/// 当前受控浏览器实例的来源：空 = 白泽自己启动的受控实例；
/// 非空 = 已直连「机主自己启动的浏览器」调试端点。
static BROWSER_ORIGIN: Mutex<String> = Mutex::new(String::new());

fn browser_origin() -> String {
    BROWSER_ORIGIN.lock().map(|g| g.clone()).unwrap_or_default()
}

/// 探测是否有「机主自己启动、带远程调试端口」的浏览器可以直连。
///
/// 这是「白泽能不能操控我自己那个浏览器窗口」的通道：Chromium 系浏览器用
/// `--remote-debugging-port=9222` 启动后会在本机开放 CDP 端点，白泽可直接连上，
/// 于是操作对象就是**用户当前正在用的那个窗口**（同一 profile、同一登录态），
/// 而不是另起一个白泽专属实例。
///
/// 默认探测 9222，可用环境变量 BAIZE_CDP_ENDPOINT 指定其它端点。
/// 探测失败（绝大多数情况——日常浏览器不会开调试端口）返回 None，走白泽自己启动的实例。
fn existing_cdp_endpoint() -> Option<String> {
    let base = std::env::var("BAIZE_CDP_ENDPOINT")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| CDP_PROBE_DEFAULT.to_string());
    // reqwest 的 blocking 客户端不能在 async 上下文里创建：和站点探测一样放到独立线程
    std::thread::spawn(move || -> Option<String> {
        // 本地回环不走环境代理，否则代理一挂就探测不到
        let client = build_client_opts(Duration::from_millis(900), false).ok()?;
        let resp = client.get(format!("{base}/json/version")).send().ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let v: Value = resp.json().ok()?;
        v.get("webSocketDebuggerUrl")?.as_str().map(str::to_string)
    })
    .join()
    .ok()
    .flatten()
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
        // 附带的本地缓存一并作废（可见性缓存属于旧实例的观测）
        TAB_VISIBLE_AT.store(0, Ordering::Relaxed);
        reset_tab_memory();
    }
    // 优先直连「机主自己启动的浏览器」：本机若已有带调试端口的 Chromium 实例，
    // 直接连上它——操控对象就是用户当前那个窗口（同一 profile / 登录态），
    // 不另开新窗。没有（日常情况）才启动白泽专属受控实例。
    if let Some(ws) = existing_cdp_endpoint() {
        match headless_chrome::Browser::connect(ws.clone()) {
            Ok(b) => {
                if let Ok(mut g) = BROWSER_ORIGIN.lock() {
                    *g = format!("已直连机主自己启动的浏览器（{ws}）");
                }
                println!("[浏览器] 已连接现有调试端点 {ws}");
                let b = Arc::new(b);
                *guard = Some(b.clone());
                reset_tab_memory();
                return Ok(b);
            }
            Err(e) => println!("[浏览器] 连接现有调试端点失败({e})，改为启动受控实例"),
        }
    }
    if let Ok(mut g) = BROWSER_ORIGIN.lock() {
        g.clear();
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
    reset_tab_memory();
    // 这是白泽自己启动的实例（专属 profile，里面的页面都是白泽开的）：把现有标签页
    // 全部登记归属并挂上事件监听——后面「只关自己开的页面」的收尾清理才有依据可循。
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        browser.register_missing_tabs();
        let tabs = browser
            .get_tabs()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        mark_all_tabs_ours(&tabs);
        for t in &tabs {
            subscribe_tab_events(t);
        }
    }));
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

/// 最近一次解析到的标签页。每个动作结束后统一读它的 id / url / 标题回填进返回值，
/// 让模型**每一步都能核对「我操作的到底是不是我以为的那个页面」**。
///
/// 这解决的是浏览器多标签页里最隐蔽的一类错误：不是点不到元素，而是
/// **在错的页面上点对了元素**——点了 target=_blank 的链接后浏览器弹出新标签页，
/// 模型以为自己在看新页面，实际后续动作仍落在原标签页上，两边各说各话。
static LAST_TAB: Mutex<Option<Arc<headless_chrome::Tab>>> = Mutex::new(None);

fn remember_tab(tab: &Arc<headless_chrome::Tab>) {
    if let Ok(mut g) = LAST_TAB.lock() {
        *g = Some(tab.clone());
    }
}

fn last_tab() -> Option<Arc<headless_chrome::Tab>> {
    LAST_TAB.lock().ok().and_then(|g| g.clone())
}

/// 「这次为什么落在另一个标签页上」的说明（原活跃标签页被关掉只能新建空白页等）。
/// 每次动作开始时清空，避免同一句提示反复出现。
static RESOLVE_NOTE: Mutex<Option<String>> = Mutex::new(None);

fn set_resolve_note(s: &str) {
    if let Ok(mut g) = RESOLVE_NOTE.lock() {
        *g = Some(s.to_string());
    }
}

fn clear_resolve_note() {
    if let Ok(mut g) = RESOLVE_NOTE.lock() {
        *g = None;
    }
}

fn take_resolve_note() -> Option<String> {
    RESOLVE_NOTE.lock().ok().and_then(|mut g| g.take())
}

// ───────────── 标签页归属 / JS 对话框 / 下载目录 ─────────────

/// 白泽自己创建的标签页（target id）。
/// 用来把「白泽开的」和「机主原本就开着的」分开：收尾清理只允许动前者，
/// 后者一律不碰——误关机主的页面比多留几个空白标签页严重得多。
static TABS_OURS: Mutex<Option<std::collections::HashSet<String>>> = Mutex::new(None);

fn mark_tab_ours(id: &str) {
    if let Ok(mut g) = TABS_OURS.lock() {
        g.get_or_insert_with(std::collections::HashSet::new)
            .insert(id.to_string());
    }
}

fn unmark_tab_ours(id: &str) {
    if let Ok(mut g) = TABS_OURS.lock() {
        if let Some(s) = g.as_mut() {
            s.remove(id);
        }
    }
}

fn tab_is_ours(id: &str) -> bool {
    TABS_OURS
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|s| s.contains(id)))
        .unwrap_or(false)
}

/// 把一批标签页全部登记为「白泽的」——只在浏览器是**白泽启动**的实例上调用：
/// 那种实例里的所有页面本来就是白泽自己开的（机主原有的窗口在另一个实例里）。
fn mark_all_tabs_ours(tabs: &[Arc<headless_chrome::Tab>]) {
    if let Ok(mut g) = TABS_OURS.lock() {
        let s = g.get_or_insert_with(std::collections::HashSet::new);
        for t in tabs {
            s.insert(t.get_target_id().as_str().to_string());
        }
    }
}

/// 空白页判定：这类标签页留着只占内存，收尾时可以清理。
/// 只认真正的「空页」——一个已经打开了网站的页面（哪怕是首页）都不算，
/// 免得把白泽正在用的页面当成垃圾收掉。
fn is_blank_url(url: &str) -> bool {
    let u = url.trim().to_ascii_lowercase();
    u.is_empty()
        || u == "about:blank"
        || u == "data:,"
        || u.starts_with("chrome://newtab")
        || u.starts_with("chrome://new-tab-page")
        || u.starts_with("edge://newtab")
        || u.starts_with("edge://new-tab-page")
}

/// 待处理的 JS 对话框（alert / confirm / prompt / beforeunload）。
///
/// 对话框会**阻塞该标签页的脚本**：之后任何 `evaluate`（含 `document.title`）或
/// 依赖脚本完成的点击都会一直挂到超时（crate 默认 20s），表现就是「白泽卡住不动」。
/// 所以系统只做两件事：① 记录「哪个页面弹了什么」；② 在下一步动作前**礼貌地拒绝执行**
/// 并把处理权交给模型（action=handle_dialog）。**绝不擅自代按**——
/// 「确认删除？」这类框一旦被自动点掉就是不可逆的破坏，宁可多花一轮让模型自己判断。
static PENDING_DIALOG: Mutex<Option<Value>> = Mutex::new(None);

/// 已注册过事件监听的标签页（target id），避免重复注册
static DIALOG_SUBSCRIBED: Mutex<Option<std::collections::HashSet<String>>> = Mutex::new(None);

/// 给标签页挂一次性事件监听：记录对话框的开/关。
/// 事件是**按 session 分发的**（`listen_to_target_events(session_id)`），所以不会串到别的标签页。
fn subscribe_tab_events(tab: &Arc<headless_chrome::Tab>) {
    let id = tab.get_target_id().as_str().to_string();
    {
        let Ok(mut g) = DIALOG_SUBSCRIBED.lock() else {
            return;
        };
        if !g
            .get_or_insert_with(std::collections::HashSet::new)
            .insert(id.clone())
        {
            return;
        }
    }
    use headless_chrome::protocol::cdp::types::Event;
    let tid = id;
    let listener = move |event: &Event| match event {
        Event::PageJavascriptDialogOpening(ev) => {
            // 对话框类型（alert / confirm / prompt / beforeunload）对判断「这个确定该不该点」
            // 很关键，一并记下来给模型看。
            let kind = serde_json::to_value(&ev.params.Type)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| "dialog".to_string());
            let info = json!({
                "tab_id": tid,
                "kind": kind,
                "message": ev.params.message,
                "url": ev.params.url,
            });
            crate::windows::diag_log(&format!(
                "[浏览器·对话框] {} 弹出 [{}]：{}",
                tid, kind, ev.params.message
            ));
            if let Ok(mut g) = PENDING_DIALOG.lock() {
                *g = Some(info);
            }
        }
        Event::PageJavascriptDialogClosed(_) => {
            if let Ok(mut g) = PENDING_DIALOG.lock() {
                let same = g
                    .as_ref()
                    .and_then(|v| v.get("tab_id"))
                    .and_then(|v| v.as_str())
                    == Some(tid.as_str());
                if same {
                    *g = None;
                }
            }
        }
        _ => {}
    };
    // 返回的 Weak 不保留：监听跟着标签页活，标签页被 drop 时自然失效
    let _ = tab.add_event_listener(std::sync::Arc::new(listener));
}

fn pending_dialog_for(tab_id: &str) -> Option<Value> {
    PENDING_DIALOG.lock().ok().and_then(|g| {
        g.as_ref()
            .filter(|v| v.get("tab_id").and_then(|x| x.as_str()) == Some(tab_id))
            .cloned()
    })
}

/// 本步动作作用在哪个标签页上就查哪个（没显式给 tab_id 时用最近一次解析到的标签页）
fn pending_dialog_for_action(tab_id: Option<&str>) -> Option<Value> {
    let id = match tab_id {
        Some(s) => s.to_string(),
        None => last_tab()?.get_target_id().as_str().to_string(),
    };
    pending_dialog_for(&id)
}

fn clear_pending_dialog(tab_id: &str) {
    if let Ok(mut g) = PENDING_DIALOG.lock() {
        let same = g
            .as_ref()
            .and_then(|v| v.get("tab_id"))
            .and_then(|v| v.as_str())
            == Some(tab_id);
        if same {
            *g = None;
        }
    }
}

/// 安全取标题：标签页上挂着未处理的对话框时，`document.title` 会被阻塞的脚本挡住，
/// 这个 evaluate 会一直挂到超时——所以有对话框就直接返回空标题。
fn tab_title(tab: &Arc<headless_chrome::Tab>) -> String {
    if pending_dialog_for(tab.get_target_id().as_str()).is_some() {
        return String::new();
    }
    tab.get_title().unwrap_or_default()
}

/// 下载目录（None = 还没设过，用浏览器默认，即系统「下载」文件夹）
static DOWNLOAD_DIR: Mutex<Option<String>> = Mutex::new(None);

/// 系统默认下载目录（%USERPROFILE%\Downloads）
fn default_download_dir() -> String {
    match std::env::var("USERPROFILE") {
        Ok(p) if !p.trim().is_empty() => format!("{p}\\Downloads"),
        _ => String::new(),
    }
}

fn current_download_dir() -> String {
    DOWNLOAD_DIR
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(default_download_dir)
}

/// 把下载目录应用到标签页。用 CDP 的 `Page.setDownloadBehavior`（协议里标注为 deprecated，
/// 但各个 Chrome 版本仍在实现；它按标签页生效，正好够用）。
/// 失败时把原因如实带出去，让白泽知道「下载会落到浏览器默认目录」。
fn apply_download_dir(tab: &Arc<headless_chrome::Tab>, dir: &str) -> Result<(), String> {
    if dir.trim().is_empty() {
        return Err("下载目录为空".into());
    }
    use headless_chrome::protocol::cdp::Page;
    tab.call_method(Page::SetDownloadBehavior {
        behavior: Page::SetDownloadBehaviorBehaviorOption::Allow,
        download_path: Some(dir.to_string()),
    })
    .map(|_| ())
    .map_err(|e| format!("设置下载目录失败（该 Chrome 版本可能不支持此接口）: {e}"))
}

/// 浏览器实例重建（或关闭）时清空标签页记忆。
/// 新实例自带一个初始页面，若不清空，「已见集合」里没有它，就会被误报成
/// 「本步弹出了新标签页」——这种假警报会让模型去查一个根本不存在的弹窗。
fn reset_tab_memory() {
    if let Ok(mut g) = LAST_TAB.lock() {
        *g = None;
    }
    if let Ok(mut g) = TABS_KNOWN.lock() {
        *g = None;
    }
    if let Ok(mut g) = TABS_OURS.lock() {
        *g = None;
    }
    if let Ok(mut g) = DIALOG_SUBSCRIBED.lock() {
        *g = None;
    }
    if let Ok(mut g) = PENDING_DIALOG.lock() {
        *g = None;
    }
    clear_resolve_note();
}

/// 是否已有可用的浏览器实例。只查已有实例，**绝不因为「想看一眼标签页」而把浏览器启动起来**
/// （get_browser 在没有实例时会真的拉起 Chrome，用它来探测会有副作用）。
fn browser_alive() -> bool {
    match BROWSER.lock() {
        Ok(g) => g.as_ref().map(|b| b.get_version().is_ok()).unwrap_or(false),
        Err(_) => false,
    }
}

/// 已经登记过的标签页 target id，用来判断「本次动作期间是不是冒出了新标签页」。
/// 用「已见集合」而不是「上次快照」：首次同步会把机主原本开着的标签页一并补录进来，
/// 那一批不能算新开——登记过一次之后就不会再被误报成弹窗。
static TABS_KNOWN: Mutex<Option<std::collections::HashSet<String>>> = Mutex::new(None);

/// 登记并返回「这次新出现的标签页」（id / url / 标题）。首次调用只登记不报告。
fn note_new_tabs(tabs: &[Arc<headless_chrome::Tab>]) -> Vec<Value> {
    let mut guard = match TABS_KNOWN.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let first_ever = guard.is_none();
    let known = guard.get_or_insert_with(std::collections::HashSet::new);
    let mut fresh: Vec<Value> = Vec::new();
    for t in tabs {
        let id = t.get_target_id().as_str().to_string();
        if known.insert(id.clone()) && !first_ever {
            fresh.push(json!({
                "tab_id": id,
                "url": t.get_url(),
                "title": tab_title(t),
                "ours": tab_is_ours(&id),
            }));
        }
    }
    fresh
}

/// 给本步的返回值补上「操作的页面是谁」＋「有没有冒出新的标签页」，
/// 并在弹出新标签页时把操作对象重新拉到前台。
///
/// 为什么要拉回前台：新标签页会抢走浏览器的前台位置，而白泽仍在原标签页上继续操作，
/// 于是「用户正在看的页面」和「白泽正在动的页面」不是同一个——页面内的红色定位线框
/// 画在后台标签页里，用户等于什么都看不见。操作对象回到前台后两者重新对齐；
/// 模型若确实要操作新页面，返回值里给了 tab_id，switch_tab 即可。
///
/// 失败路径也要带上页面信息：模型得知道「这一步是在哪个页面上失败的」，
/// 否则会把「在错的页面上找不到元素」误判成页面本身有问题。
fn enrich_page_context(out: Result<Value, String>, action: &str) -> Result<Value, String> {
    // 连接已死的极端情况下，这一小段 CDP 交互本身也可能抛 panic。回填只是锦上添花，
    // 绝不能因此把「动作其实成功了」改写成失败——所以整段兜底，出错就按原样返回。
    let fallback = out.clone();
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        enrich_page_context_inner(out, action)
    })) {
        Ok(v) => v,
        Err(_) => fallback,
    }
}

fn enrich_page_context_inner(out: Result<Value, String>, action: &str) -> Result<Value, String> {
    // 页面弹了 JS 对话框时，脚本是**被阻塞**的：这一步的标题读取会挂到超时，
    // 一并跳过（对话框状态本身稍后会作为 dialog 字段回填）。
    let dialog = if matches!(action, "handle_dialog" | "tabs" | "state") {
        None
    } else {
        pending_dialog_for_action(None)
    };
    let blocked_tab = dialog
        .as_ref()
        .and_then(|d| d.get("tab_id"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    // 点击 / 按键 / 执行 JS 是最可能弹出新标签页的（target=_blank、window.open、广告弹窗），
    // 这几个动作强制刷新一次标签页清单；其余动作走节流缓存（多数时候是零开销），
    // 这样延迟出现的新标签页也会在下一次动作里被报出来。
    // 对话框挡着时跳过这一步：`register_missing_tabs` 要对新 target 做 Page.enable，
    // 没必要在一个被阻塞的页面上多花时间，而且对话框信息比标签页增删要紧得多。
    let popup_capable = matches!(action, "click" | "click_text" | "key" | "evaluate");
    let new_tabs: Vec<Value> = if browser_alive() && blocked_tab.is_none() {
        match list_chrome_tabs_ex(popup_capable) {
            Ok(tabs) => note_new_tabs(&tabs),
            Err(_) => Vec::new(),
        }
    } else {
        Vec::new()
    };
    // 主动切换标签页的动作（new_tab / switch_tab）不报「冒出弹窗」，那是它自己的意图；
    // 但清单照样登记，避免下一次动作重复报告同一个标签页。
    let announce_popup = !matches!(action, "new_tab" | "switch_tab" | "tabs" | "state");
    let new_tabs = if announce_popup { new_tabs } else { Vec::new() };

    if !new_tabs.is_empty() {
        if let Some(tab) = last_tab() {
            let _ = tab.activate();
        }
    }

    let mut patch = serde_json::Map::new();
    if let Some(tab) = last_tab() {
        patch.insert("tab_id".into(), json!(tab.get_target_id().as_str()));
        patch.insert("page_url".into(), json!(tab.get_url()));
        let title = tab_title(&tab);
        if !title.is_empty() {
            patch.insert("page_title".into(), json!(title));
        }
    }

    let mut notes: Vec<String> = Vec::new();
    if let Some(d) = &dialog {
        patch.insert("dialog".into(), d.clone());
        let kind = d.get("kind").and_then(|m| m.as_str()).unwrap_or("dialog");
        let msg = d.get("message").and_then(|m| m.as_str()).unwrap_or("");
        notes.push(format!(
            "页面弹出了一个 JS 对话框（类型 {kind}）：「{msg}」。它会**阻塞该标签页的脚本**——\
             在它被处理掉之前，任何点击/输入/执行 JS 都会一直等下去（最后超时）。\
             下一步请先调用 browser_act 的 handle_dialog（accept=true 点「确定」，\
             false 点「取消」；若这是 prompt 还可用 text 填内容），处理完再继续。\
             系统不会替你按，因为「确定」可能是不可逆的（如「确认删除」）。"
        ));
    }
    if let Some(n) = take_resolve_note() {
        notes.push(n);
    }
    if !new_tabs.is_empty() {
        let urls: Vec<String> = new_tabs
            .iter()
            .filter_map(|t| t.get("url").and_then(|u| u.as_str()).map(str::to_string))
            .collect();
        notes.push(format!(
            "本步让浏览器多出了新标签页：{}。你的操作对象仍在原标签页（已重新切回前台，\
             页面内的定位线框也画在它上面）——要继续处理新页面请 switch_tab；\
             如果那只是无关的广告/推广弹窗，用 close_tab 关掉它再继续。",
            urls.join(" | ")
        ));
    }
    if !notes.is_empty() {
        patch.insert("note".into(), json!(notes.join("\n")));
    }

    match out {
        Ok(Value::Object(mut m)) => {
            if !new_tabs.is_empty() {
                m.insert("new_tabs".into(), json!(new_tabs));
            }
            for (k, v) in patch {
                m.insert(k, v);
            }
            Ok(Value::Object(m))
        }
        Ok(other) => Ok(other),
        Err(e) => {
            // 失败时拼一行紧凑摘要追加到错误信息后面（工具错误是纯文本，装不下 JSON）
            let mut tail = String::new();
            if let Some(u) = patch.get("page_url").and_then(|v| v.as_str()) {
                tail.push_str(&format!("\n[本步页面] {u}"));
            }
            if let Some(t) = patch.get("page_title").and_then(|v| v.as_str()) {
                tail.push_str(&format!("（{t}）"));
            }
            for n in &notes {
                tail.push_str(&format!("\n[提示] {n}"));
            }
            Err(format!("{e}{tail}"))
        }
    }
}

/// 最近一次「全量补录外部标签页」的时间戳（毫秒）。
/// `register_missing_tabs()` 要走一趟 `/json/list`（对新 target 还要逐个 attach），
/// 而它被**每一个动作**都调一次；同一轮里批量派发 5 个 browser_act 就是 5 趟。
/// 标签页增删是低频事件，1.2 秒节流足够；需要精确清单（tabs / state 动作）或
/// 按 id 查不到时，会强制刷新一次，不会因此看不到用户新开的标签页。
static TABS_SYNCED_AT: AtomicU64 = AtomicU64::new(0);
const TABS_SYNC_INTERVAL_MS: u64 = 1200;

/// 列出 Chrome 当前所有标签页（含用户手动在可见窗口中打开的）。
/// `force=true` 时忽略节流，强制全量补录。
fn list_chrome_tabs_ex(force: bool) -> Result<Vec<Arc<headless_chrome::Tab>>, String> {
    let browser = get_browser()?;
    let now = now_ms();
    if force
        || now.saturating_sub(TABS_SYNCED_AT.load(Ordering::Relaxed)) >= TABS_SYNC_INTERVAL_MS
    {
        TABS_SYNCED_AT.store(now, Ordering::Relaxed);
        // register_missing_tabs 内部在 Chrome 连接断开时会 unwrap 导致 panic（headless_chrome 库 bug），
        // 用 catch_unwind 降级：连接异常时跳过补录，仅返回白泽自己创建的标签页。
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            browser.register_missing_tabs();
        }));
    }
    // poison 恢复：register_missing_tabs 内部在连接异常时会 panic 毒化 mutex，
    // 此处用 into_inner 恢复，不再把「锁被毒化」当成硬错误返回。
    let guard = browser
        .get_tabs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Ok(guard.clone())
}

/// 列出标签页（走节流，热路径用）
fn list_chrome_tabs() -> Result<Vec<Arc<headless_chrome::Tab>>, String> {
    list_chrome_tabs_ex(false)
}

/// 按 target id 查找标签页：先走节流缓存，查不到再强制刷新一次
/// （用户刚手动新开的标签页可能还没进缓存，不能直接报「找不到」）
fn find_chrome_tab(id: &str) -> Result<Arc<headless_chrome::Tab>, String> {
    let pick = |tabs: &[Arc<headless_chrome::Tab>]| -> Option<Arc<headless_chrome::Tab>> {
        tabs.iter()
            .find(|t| t.get_target_id().as_str() == id)
            .cloned()
    };
    if let Some(tab) = pick(&list_chrome_tabs()?) {
        return Ok(tab);
    }
    pick(&list_chrome_tabs_ex(true)?).ok_or_else(|| format!("未找到标签页 {id}"))
}

/// 新建标签页并设为活跃，同时去除 webdriver 自动化痕迹
fn new_chrome_tab() -> Result<Arc<headless_chrome::Tab>, String> {
    let browser = get_browser()?;
    let tab = browser.new_tab().map_err(|e| format!("创建标签页失败: {e}"))?;
    let _ = tab.enable_stealth_mode();
    set_active_target(Some(tab.get_target_id().clone()));
    remember_tab(&tab);
    register_tab(&tab, true);
    Ok(tab)
}

/// 标签页的「出生登记」：登记归属、挂对话框监听、下发已设置的下载目录。
/// 每个会被操作的页面都走一遍，保证对话框这种「会让下一步挂死」的事件不漏。
fn register_tab(tab: &Arc<headless_chrome::Tab>, ours: bool) {
    let id = tab.get_target_id().as_str().to_string();
    if ours {
        mark_tab_ours(&id);
    }
    subscribe_tab_events(tab);
    // 只有白泽/用户显式设过下载目录才下发，避免给每个标签页都白搭一趟 CDP
    let dir = DOWNLOAD_DIR.lock().ok().and_then(|g| g.clone());
    if let Some(dir) = dir {
        let _ = apply_download_dir(tab, &dir);
    }
}

/// 找一个「白泽自己开的、当前是空白页、不是正在操作的那个」标签页来复用。
/// 目的很直白：别让每个任务都多留一个空白标签页——留着只占内存，
/// 专门关它又要多一次往返，复用最划算（也就是用户说的「减轻浏览器压力」）。
fn reuse_blank_tab() -> Option<Arc<headless_chrome::Tab>> {
    let tabs = list_chrome_tabs().ok()?;
    let active = active_target_id();
    tabs.into_iter().find(|t| {
        let id = t.get_target_id().as_str();
        tab_is_ours(id) && Some(id.to_string()) != active && is_blank_url(&t.get_url())
    })
}

/// 取得要操作的标签页：指定 id 时定位并激活之；未指定时复用活跃标签页，否则新建
fn resolve_tab(id: Option<&str>) -> Result<Arc<headless_chrome::Tab>, String> {
    match id {
        Some(id) => {
            let tab = find_chrome_tab(id)?;
            set_active_target(Some(id.to_string()));
            remember_tab(&tab);
            register_tab(&tab, false);
            Ok(tab)
        }
        None => {
            if let Some(active) = active_target_id() {
                if let Ok(tab) = find_chrome_tab(&active) {
                    remember_tab(&tab);
                    register_tab(&tab, false);
                    return Ok(tab);
                }
                // 原本记着的活跃标签页不见了（用户手动关掉、页面自己 close、崩溃）。
                // 以前这里会**静默**新建一个空白页，模型却以为还停在原页面上——
                // 这是「操作错页面」里最危险的一种。现在明确记一条说明随返回值给模型。
                set_resolve_note(
                    "原先操作的那个标签页已经不存在了（可能被关闭或崩溃），本次动作落在一个新建的空白标签页上。\
                     请先用 tabs 看一眼现在有哪些页面，再用 switch_tab / goto 回到目标页面，别在这个空白页上继续操作。",
                );
            }
            // 优先复用白泽自己留下的空白标签页，没有才新建（少一个标签页 = 少一份内存）
            if let Some(tab) = reuse_blank_tab() {
                set_active_target(Some(tab.get_target_id().as_str().to_string()));
                remember_tab(&tab);
                register_tab(&tab, false);
                return Ok(tab);
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

/// 在指定像素坐标处点击（先移动鼠标，再点击）。
/// 中间的 40ms 是为了让页面收到一次真实的 mousemove 后再按下——hover 类菜单/懒加载
/// 按钮需要这一拍；原来 80ms 属于保守值，40ms 足够且每次动作省掉 40ms 累计延迟。
fn click_at(tab: &Arc<headless_chrome::Tab>, x: f64, y: f64) -> Result<(), String> {
    use headless_chrome::browser::tab::point::Point;
    let point = Point { x, y };
    tab.move_mouse_to_point(point).map_err(|e| format!("移动鼠标失败: {e}"))?;
    std::thread::sleep(Duration::from_millis(40));
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

/// 等页面「可交互」，**不等资源加载完**。
///
/// 关键取舍（这是「点了要等半天、没有连贯性」的主因）：`readyState === 'complete'` 要等
/// 图片 / 视频 / 埋点 / 长轮询全部结束，很多站点几分钟都到不了 —— 早先每次 click_text 进来
/// 都死等它，最多白等 3 秒。而 DOM 到 `interactive` 就已经能定位、能点击了，
/// 所以这里只要 readyState 离开 `loading` 就返回，只留 60ms 给首屏脚本渲染。
/// 真遇到「DOM 一直没就绪」的重页面，1.2s 硬上限兜底，不再无限等。
fn wait_page_ready(tab: &Arc<headless_chrome::Tab>) {
    let start = std::time::Instant::now();
    let hard = Duration::from_millis(1200);
    loop {
        let rs = tab
            .evaluate("document.readyState", false)
            .ok()
            .and_then(|r| r.value)
            .and_then(|v| v.as_str().map(|s| s.to_string()));
        match rs.as_deref() {
            Some("interactive") | Some("complete") => {
                std::thread::sleep(Duration::from_millis(60));
                return;
            }
            _ => {
                if start.elapsed() >= hard {
                    crate::windows::diag_log("[浏览器·等待] readyState 仍为 loading，放弃等待（1200ms）");
                    return;
                }
                std::thread::sleep(Duration::from_millis(40));
            }
        }
    }
}

/// 「看→点」闭环：按可见文字 / 描述定位并点击控件。
///
/// 四级：**DOM 文字定位**（一次 evaluate，十几毫秒）→ OCR 文字匹配 → Set-of-Marks 视觉选框
/// → 视觉模型坐标回归。前三级失败时滚动一屏后重试（最多 4 屏），覆盖目标在视口外的情况。
///
/// 为什么把 DOM 放在最前面：绝大多数「点登录 / 点搜索 / 点下一步」在 DOM 里就能唯一定位，
/// 而截图 + OCR 要 0.2~0.5s，视觉模型更是秒级。DOM 命中时不仅更快，也比像素识别更准
/// （不会被相近文案或模糊字形骗到）。只有文字本身是图片、在 iframe 里、纯图标按钮时才
/// 需要回落到像素那条路。
fn click_by_description(tab: &Arc<headless_chrome::Tab>, target: &str) -> Result<Value, String> {
    // 零级：DOM 文字定位（服务端零成本，不走截图/OCR/视觉模型）。
    // **先直接试，不等页面**——页面已就绪时这里十几毫秒就命中；为了「等页面完全加载」
    // 先睡一觉，正是「点完一个交互不能立刻点下一个」的来源。
    if let Some(hit) = dom_locate_by_text(tab, target) {
        return finish_text_click(tab, target, hit);
    }
    // 首次没命中：可能是页面还在渲染（SPA 首屏脚本尚未跑完）。等 DOM 可交互后重试一次，
    // 命中就仍然走快路径；仍没命中才承认「DOM 里没有这个文字」，进入成本更高的像素链。
    wait_page_ready(tab);
    if let Some(hit) = dom_locate_by_text(tab, target) {
        return finish_text_click(tab, target, hit);
    }

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
            // 定位可视化：先在网页里把命中的词框出来，紧接着点
            mark_rect_device(
                tab,
                hits[0].0 as f64,
                hits[0].1 as f64,
                hits[0].2 as f64,
                hits[0].3 as f64,
                dpr,
                MARK_HOLD_ACT,
            );
            std::thread::sleep(MARK_LEAD);
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
                        // 定位可视化：标出视觉模型选中的那个候选框（SOM 编号对应框）
                        if let Some(b) = candidates.get(idx) {
                            mark_rect_device(
                                tab,
                                b.0 as f64,
                                b.1 as f64,
                                b.2 as f64,
                                b.3 as f64,
                                dpr,
                                MARK_HOLD_ACT,
                            );
                            std::thread::sleep(MARK_LEAD);
                        }
                        click_at(tab, x / dpr, y / dpr)?;
                        return Ok(json!({ "ok": true, "method": "som", "x": x / dpr, "y": y / dpr, "target": target }));
                    }
                }
            }
        }

        // 三级：视觉坐标回归
        match crate::visual_grounding::visual_locate(&path, target) {
            Ok((x, y)) => {
                // 只有坐标没有矩形：画一个小方框标出落点
                mark_point_device(tab, x, y, dpr, MARK_HOLD_ACT);
                std::thread::sleep(MARK_LEAD);
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
            // interactive = DOM 已就绪，能定位、能点击了。剩下的资源（图片/视频/埋点/长轮询）
            // 很多页面几分钟都加载不完，为它们把整条链卡住不值得——留 500ms 给首屏脚本渲染即走，
            // 后续每一步动作自己会等它需要的那个元素出现。
            Some("interactive") if start.elapsed() >= Duration::from_millis(500) => return Ok(()),
            _ => std::thread::sleep(Duration::from_millis(120)),
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
        "驱动可见的桌面谷歌浏览器执行完整交互操作（持久化登录态，多标签页）：goto 跳转、click 点击、type 输入、wait 等待、screenshot 截图、evaluate 执行 JS、content 读取页面文本、new_tab 新建标签页、tabs 列出标签页、switch_tab 切换标签页、close_tab 关闭标签页、scroll 滚动、key 按键、reload 刷新、back 后退、forward 前进、state 感知浏览器状态、look 感知当前页面（截图+OCR 文本）、click_text 按可见文字定位并点击（看→点闭环）、highlight 只把目标元素框出来不点击（给用户看清指代）、scan 把当前页面或指定模块的可交互元素用编号线框全部标出来（返回编号清单）、click_mark 按 scan 的编号点击、handle_dialog 处理页面弹出的 JS 对话框、tidy_tabs 收尾清理白泽自己开的空白标签页、upload 直接给文件输入框塞文件（绕过系统文件选择器）、set_download_dir / downloads 设置并查看下载目录。这是白泽操作桌面谷歌浏览器的统一入口。用户提到「桌面浏览器/受控浏览器」时只使用本工具打开，不要叠加 browser_open / browser_navigate。click / type / click_text / highlight 会在**网页页面内**用一条红色实线框出目标元素（线框直接注入页面、随操作即时出现，点击后仍在页面上停留约 1 秒，用户能看清白泽动的是哪个元素），不要指望屏幕上有额外的覆盖层方框。\
【每一步都会告诉你「现在在哪个标签页」】每一步返回值都带 tab_id / page_url / page_title，那是**本步实际操作**的标签页：动手前先核对一眼是不是你要的页面，不是就用 switch_tab 切回去或重新 goto。点击/按键/执行 JS 若让浏览器多开了一个标签页（target=_blank、登录跳转、广告弹窗），返回值会多出 new_tabs 并在 note 里说明；此时你的操作对象**仍在原标签页**且已重新切回前台（页面内的红色线框也画在它上面）——确实要处理新页面就 switch_tab，无关弹窗用 close_tab 关掉。原来操作的标签页若已被关掉，note 会说明「本次落在新建的空白标签页上」，先 tabs 看清现有页面再回目标页。失败时返回信息里也会带 [本步页面]，别把「在错的页面上找不到元素」当成页面本身有问题。\
【提速：同轮批量派发】把已经能确定的操作在==同一轮==里一次性发出多个 browser_act 调用（系统按顺序执行，只算一次模型往返）；序列首步是 goto 时后面可以放心跟 click/type。每做一步就单独等一轮，是浏览器操作慢的主因。\
【定位零成本】click_text / highlight 先在 DOM 里按文字定位（一次 evaluate，通常十几毫秒），命中即点，不需要先截图、也不需要先 look 看页面；反过来也不要为了看清页面反复调 look / screenshot（每次截图 0.2s 起，OCR 更慢）。只有 DOM 定位失败（文字是图片 / 在 iframe 里 / 纯图标按钮）时才自动回落 OCR 与视觉定位。\
【scan + click_mark：凡按「位置 / 区域 / 功能模块」描述的目标，都先 scan 再点】用户按**位置或区域**描述目标时（「右上角那个按钮」「侧边栏里的 XX」「这个面板上的图标」「那一排的第三个」「页面下方的操作区」「评论区/工具栏里」），**默认流程就是先 scan、再 click_mark**——不要直接拿一个猜的选择器去 click_text。scan 会把该区域里的可交互元素全部用**编号线框**标在网页上，用户能亲眼看到你锁定的范围与每个编号对应哪个控件，你挑中编号后用 click_mark 带 index=n 点它：编号与元素在 DOM 里是绑定的，不会像「按文字再猜一次」那样点错。scan 的 scope 就用用户说的那个模块/区域名（如「侧边栏」「工具栏」「搜索区」「评论区」），找不到会在 scope_miss 里说明并退回整页；指定了 scope 时脚本会先把该模块带进视口再枚举，所以框一定画在看得见的地方。用户说「看看这块有什么」/「页面上有哪些能点的」时同样直接 scan（可带 scope），不要为了「看看」去 screenshot / look / 盲滚。只有用户给了**页面上唯一的明确文字**（「点登录」「点搜索」）才直接 click_text 一步到位，省掉这一扫；文字定位不确定或第一次没点对时，回到 scan。（scan 是一次 evaluate 完成，不截图、不跑 OCR、不调视觉模型，几十毫秒。）返回的 items 里每个元素带编号 n、标签名、可见文字、位置。清单只含**当前视口内**元素：返回的 off_view 会告诉你视口外上方/下方各还有几个可交互元素，需要往下找就用 scroll 带 selector / target **定位式滚动**（滚到目标就停）再 scan，不要用 amount 盲滚。\n【不要无意识滚动页面（重要）】定位和点击**不会**主动滚动页面——只有当目标确实不在视口里时才滚，而且滚最小距离。所以：① 不要为了「看看页面上有什么」而滚动；② 滚动只应发生在「找按钮 / 找内容」这种确实需要的时候，且优先用 scroll 的 selector / target 定位式滚动；③ 一个动作做完不要把页面滚回顶部或滚回原处——那会顺手把用户正在看的上下文带走；④ 页面自己滚动通常是目标元素在视口外被带进来了，属于必要动作，不必额外补偿。\n【标签页卫生：少开一个是一个，收尾该关就关】① 开新页面优先复用：new_tab 会自动复用白泽自己留下的空白标签页（返回值 reused=true），不要为了「换个页面看看」反复 new_tab；② 动作返回的 note 提示有多余空白标签页时，用 tidy_tabs 一次清掉（它只关白泽自己开的空白页，机主自己开的页面一律不动）；③ 一个任务做完要主动决定：这个标签页是你为它开的（tabs 里 ours=true）、任务已结束且不再需要，就用 close_tab 关掉它（省略 tab_id 就是关当前操作的那个），别把一堆标签页留给机主；④ 机主自己开的页面（tabs 里 ours=false）永远不要关。\n【页面弹出 JS 对话框时（alert / confirm / prompt）】对话框会**阻塞该标签页的脚本**：它挡住之后所有的点击/输入/执行 JS，直到被处理掉。所以当返回值里出现 dialog 字段（dialog.kind 是 alert / confirm / prompt / beforeunload，dialog.message 是框上的文字）、或某一步报「有一个未处理的 JS 对话框挡着」时，唯一正确的下一步是先调 handle_dialog（accept=true 点「确定」、false 点「取消」；prompt 用 text 填内容），处理完再继续原本的操作。系统不会替你按——「确定」可能是不可逆的（如「确认删除」），由你按对话框文案和任务目标判断；拿不准就先 dismiss（取消），并告知用户。\n【文件上传与下载】上传用 upload（selector 指 file 输入框，通常是 input[type=file]；files 是文件绝对路径，可传数组）——它直接把文件交给页面，不弹系统文件选择器。若找不到输入框，多半是要先点一下上传按钮让 input 进入 DOM：先 click_text / click_mark 点它，再 upload；若点按钮直接弹出了系统的文件选择窗口（CDP 够不着的原生对话框），才需要 GUI 自动化。下载：要固定下载位置先 set_download_dir（默认是系统的「下载」文件夹），触发下载后用 downloads 看目录里最近的文件（in_progress=true 表示还在写、还没下完），不要靠「猜文件名」判断下载成功没有。\n【点击后不要等页面加载完（连贯性）】点完一个交互，下一步**直接发**（同一轮里批量派发），不要调 wait 等「页面加载完成」——后端已经不会等资源全部加载（那要等图片/埋点/长轮询，很多站点永远等不完），你的下一个动作自己会等它需要的元素出现。只有两种情况才用 wait：① 页面明确要等一个固定时长的动画/倒计时；② 点击后要做的事依赖「异步结果已经出现」而你又没有具体元素可以等。默认答案是不等。\n【本工具不会接管屏幕】浏览器操作由 CDP 直接作用于浏览器内部，也不阻断用户键鼠：不要调用 screen_release，也不要先做 window_prepare / window_minimize_all / window_focus 之类的清屏准备——那是桌面 GUI 任务的纪律，用在这里只会造成误接管。\
【两种浏览器来源】① 默认由白泽启动受控实例：用的是机主已安装的 Chrome/Edge 程序 + 白泽专属 profile（登录态跨会话保留），是一个新窗口，不是机主正在用的那个；② 若机主以自己的浏览器加 --remote-debugging-port=9222 启动（或用环境变量 BAIZE_CDP_ENDPOINT 指定端点），本工具会直接连上那个浏览器，操作对象就是机主当前窗口。action=state 返回的 origin 字段会说明当前连的是哪一种。\
【降级到 GUI 自动化的唯一条件】仅当返回值里带「可降级」标注时才改用桌面 GUI 自动化（受控浏览器起不来、需要操作系统级对话框等 CDP 够不着的情形）；页面内找不到元素、选择器写错、加载超时一律继续用本工具修正，不要动屏幕"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["goto", "click", "type", "wait", "screenshot", "evaluate", "content", "new_tab", "tabs", "switch_tab", "close_tab", "tidy_tabs", "scroll", "key", "reload", "back", "forward", "state", "look", "click_text", "highlight", "scan", "click_mark", "handle_dialog", "upload", "set_download_dir", "downloads"], "description": "操作类型" },
                "url": { "type": "string", "description": "goto / new_tab 时的目标 URL" },
                "selector": { "type": "string", "description": "click / type / highlight / upload / scroll 时的 CSS 选择器（highlight 与 target 二选一；upload 指 file 输入框；scroll 指要滚到视口内的元素）" },
                "text": { "type": "string", "description": "type 时输入的文本；handle_dialog 且对话框是 prompt 时填入的内容" },
                "target": { "type": "string", "description": "click_text / highlight / scroll 时要定位的控件可见文字或描述，如「登录」「搜索」「下一步」" },
                "scope": { "type": "string", "description": "scan 时限定范围：某个模块/区域的名称或描述（如「侧边栏」「搜索区」「评论区」「工具栏」，也可用其 id / aria-label / class 片段）。留空表示扫描整个视口" },
                "limit": { "type": "number", "description": "scan 时最多标出多少个元素（默认 40，上限 80）；downloads 时最多列几个文件（默认 8）" },
                "index": { "type": "number", "description": "click_mark 时要点击的编号，来自最近一次 scan 返回的 n" },
                "js": { "type": "string", "description": "evaluate 时的 JS 表达式" },
                "seconds": { "type": "number", "description": "wait 时等待秒数（默认 1）" },
                "tab_id": { "type": "string", "description": "目标标签页 target id（tabs 返回）。switch_tab 必填；close_tab 可省略（省略=关掉当前操作的那个标签页）；tidy_tabs 用它指定「保留哪一个」；goto / click / type / scroll / key / reload / back / forward / screenshot / evaluate / content / look / click_text 可选，默认作用于当前活跃标签页" },
                "key": { "type": "string", "description": "key 时按下的键名，如 Enter、Tab、ArrowDown、ArrowUp、Escape、Backspace 等" },
                "amount": { "type": "number", "description": "scroll 时盲滚的像素，正数向下、负数向上（默认 500）。**能给出 selector 或 target 就别用它**——定位式滚动不会把页面丢到你没在看的地方" },
                "accept": { "type": "boolean", "description": "handle_dialog 时是否点「确定」（true 确定 / false 取消，默认 true）" },
                "files": { "type": "string", "description": "upload 时要上传的文件绝对路径；多个文件用数组" },
                "path": { "type": "string", "description": "set_download_dir 时的下载目录绝对路径（省略=系统的「下载」文件夹）" },
                "all": { "type": "boolean", "description": "tidy_tabs 时是否连「白泽开的非空白页」一起收（默认 false，只收空白页，最保守）" },
                "ignore_cache": { "type": "boolean", "description": "reload 时是否忽略缓存（默认 false）" }
            },
            "required": ["action"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        // 提到闭包外：错误收尾时要用它标注「这一步能不能降级到 GUI 自动化」
        let action_name = args["action"].as_str().unwrap_or("").to_string();
        let action_t0 = std::time::Instant::now();
        // 兜底：headless_chrome 库内部在 Chrome 连接异常时会 unwrap 导致 panic，
        // 用 catch_unwind 把 panic 转成 Err，避免拖垮整个白泽进程。
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let action = args["action"].as_str().ok_or("缺少参数 action")?;
        let tab_id = args["tab_id"].as_str();
        // 上一轮遗留的「标签页说明」不带到这一步来
        clear_resolve_note();

        // 对话框守卫（见 PENDING_DIALOG 注释）：页面弹了 alert / confirm 会**阻塞该标签页的脚本**，
        // 此时再发点击或执行 JS 只会一直挂到超时。这里礼貌地拒绝执行，把处理权交给白泽——
        // 是不是要给这个「确定」由它自己判断，系统绝不代按（「确定」可能是不可逆的）。
        if !matches!(
            action,
            "handle_dialog" | "tabs" | "state" | "set_download_dir" | "downloads"
        ) {
            if let Some(d) = pending_dialog_for_action(tab_id) {
                let kind = d.get("kind").and_then(|m| m.as_str()).unwrap_or("dialog");
                let msg = d.get("message").and_then(|m| m.as_str()).unwrap_or("");
                let url = d.get("url").and_then(|u| u.as_str()).unwrap_or("");
                return Err(format!(
                    "页面当前有一个未处理的 JS 对话框挡着（类型 {kind}）：「{msg}」（页面 {url}）。\
                     它会把这个标签页的脚本停住——所以这一步没有执行，否则会一直挂到超时。\n\
                     请先调用 browser_act 的 handle_dialog：accept=true 点「确定」、false 点「取消」；\
                     若这是 prompt 类型，还可以用 text 填输入内容。\
                     系统不会替你按，因为「确定」可能是不可逆的（例如「确认删除」）。"
                ));
            }
        }

        let out = match action {
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
                // 复用这里拿到的元素句柄直接点，省掉一次多余的 find_element 往返。
                let el = tab
                    .wait_for_element_with_custom_timeout(sel, Duration::from_secs(3))
                    .map_err(|e| format!("等待元素失败: {e}"))?;
                // 定位可视化：一次 evaluate 同时「取矩形 + 画线」，紧接着点。
                // 线框会在页面上留 MARK_HOLD_ACT 毫秒，用户能看清白泽动的是哪个元素。
                if mark_selector_css(&tab, sel, MARK_HOLD_ACT).is_some() {
                    std::thread::sleep(MARK_LEAD);
                }
                el.click().map_err(|e| format!("点击失败: {e}"))?;
                Ok(json!({ "ok": true }))
            }
            "type" => {
                let tab = resolve_tab(tab_id)?;
                let sel = args["selector"].as_str().ok_or("缺少参数 selector")?;
                let text = args["text"].as_str().unwrap_or("");
                // 同 click：快速探测，iframe 内元素交由上层深度填充兜底
                let el = tab
                    .wait_for_element_with_custom_timeout(sel, Duration::from_secs(3))
                    .map_err(|e| format!("等待元素失败: {e}"))?;
                // 定位可视化：先在网页里框出要填充的输入框，紧接着输入
                if mark_selector_css(&tab, sel, MARK_HOLD_ACT).is_some() {
                    std::thread::sleep(MARK_LEAD);
                }
                // 先聚焦元素，再输入文本（headless_chrome 的 type_str 作用于当前焦点）
                el.click().map_err(|e| format!("聚焦失败: {e}"))?;
                tab.type_str(text).map_err(|e| format!("输入失败: {e}"))?;
                Ok(json!({ "ok": true }))
            }
            // 只标记不点击：把目标元素框出来，给用户看清「白泽正在看/指的是哪个元素」
            // （用带标签的块：这几处早期命中要直接结束分支，但不能用 return 跳过
            //  统一的「页面身份回填 + 计时留痕」，那是每一步都该带的）
            "highlight" => 'hl: {
                let tab = resolve_tab(tab_id)?;
                let target = args["target"].as_str();
                let sel = args["selector"].as_str();
                if let Some(target) = target {
                    // 复用「看→点」的定位链，仅把点击换成画线框。
                    // 零级同样是 DOM 文字定位：命中就没必要截图跑 OCR。
                    // 与 click_text 同一策略——先直接试（页面已就绪时十几毫秒），
                    // 没命中才等 DOM 可交互后重试一次，仍没有才回落像素链。
                    let mut hit = dom_locate_by_text(&tab, target);
                    if hit.is_none() {
                        wait_page_ready(&tab);
                        hit = dom_locate_by_text(&tab, target);
                    }
                    if let Some((x, y, w, h, text, tag)) = hit {
                        mark_rect_css(&tab, x, y, w, h, MARK_HOLD_SHOW);
                        break 'hl Ok(json!({
                            "ok": true, "method": "dom",
                            "x": x + w / 2.0, "y": y + h / 2.0,
                            "target": target, "matched": format!("{tag}:{text}"),
                        }));
                    }
                    let (path, dpr) = capture_tab_png(&tab)?;
                    let words: Vec<Value> = match crate::ocr::ocr_detect_gui(&path) {
                        Ok((_t, w)) => w,
                        Err(_) => Vec::new(),
                    };
                    let hits = match_ocr_word(&words, target);
                    if let Some(b) = hits.first() {
                        mark_rect_device(
                            &tab,
                            b.0 as f64,
                            b.1 as f64,
                            b.2 as f64,
                            b.3 as f64,
                            dpr,
                            MARK_HOLD_SHOW,
                        );
                        break 'hl Ok(json!({
                            "ok": true, "method": "ocr",
                            "x": box_center(*b).0 / dpr, "y": box_center(*b).1 / dpr,
                            "target": target,
                        }));
                    }
                    let candidates: Vec<(i32, i32, i32, i32)> = words
                        .iter()
                        .filter_map(|w| {
                            Some((
                                w["x"].as_i64()? as i32,
                                w["y"].as_i64()? as i32,
                                w["w"].as_i64()? as i32,
                                w["h"].as_i64()? as i32,
                            ))
                        })
                        .collect();
                    if !candidates.is_empty() {
                        if let Ok((annotated, centers)) = crate::som::annotate(&path, &candidates) {
                            if let Some(idx) = crate::som::som_select(&annotated, target, centers.len()) {
                                if let (Some((x, y)), Some(b)) = (centers.get(idx).copied(), candidates.get(idx)) {
                                    mark_rect_device(
                                        &tab,
                                        b.0 as f64,
                                        b.1 as f64,
                                        b.2 as f64,
                                        b.3 as f64,
                                        dpr,
                                        MARK_HOLD_SHOW,
                                    );
                                    break 'hl Ok(json!({
                                        "ok": true, "method": "som",
                                        "x": x / dpr, "y": y / dpr, "target": target,
                                    }));
                                }
                            }
                        }
                    }
                    match crate::visual_grounding::visual_locate(&path, target) {
                        Ok((x, y)) => {
                            mark_point_device(&tab, x, y, dpr, MARK_HOLD_SHOW);
                            Ok(json!({
                                "ok": true, "method": "visual_locate",
                                "x": x / dpr, "y": y / dpr, "target": target,
                            }))
                        }
                        Err(e) => Err(format!("未能定位「{target}」：{e}")),
                    }
                } else if let Some(sel) = sel {
                    match mark_selector_css(&tab, sel, MARK_HOLD_SHOW) {
                        Some((x, y, w, h)) => Ok(json!({
                            "ok": true, "method": "selector", "selector": sel,
                            "x": x + w / 2.0, "y": y + h / 2.0,
                        })),
                        None => Err(format!("页面上找不到元素 {sel}")),
                    }
                } else {
                    Err("highlight 需要 selector 或 target".into())
                }
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
            // 扫描：把当前页面（或指定模块）里可交互的元素用编号线框标出来，返回编号清单。
            // 适合「不知道该点哪/看不清有哪些可点」的场景：先看清单，再 click_mark 点编号。
            "scan" => {
                let tab = resolve_tab(tab_id)?;
                let scope = args["scope"].as_str();
                let limit = args["limit"].as_u64().unwrap_or(40).clamp(1, 80) as usize;
                let mut out = scan_interactive(&tab, scope, limit)?;
                if let Some(o) = out.as_object_mut() {
                    o.insert(
                        "hint".into(),
                        json!(
                            "编号线框已在页面上标出（保留约 8 秒）。要操作其中某个元素就用 \
                             click_mark 带 index 点对应编号——不要再用 click_text 重新猜文字，\
                             编号与元素是绑定的，不会点错。清单只含**当前视口内**的可交互元素：\
                             目标不在清单里就看 off_view（above/below 是视口外上下各还有几个），\
                             然后用 scroll 带 selector / target **定位式滚动**到那一块再 scan；\
                             不要用 scroll 的 amount 盲滚，也不要把页面来回滚一遍找东西。\
                             列表被 limit 截断时可调大 limit 或缩小 scope。\
                             页面上出现了新编号就说明是最近一次 scan 结果，旧编号一律作废。"
                        ),
                    );
                }
                Ok(out)
            }
            // 按 scan 的编号点击（编号与元素绑定在 DOM 上，所以布局变了也能点到对的那一个）
            "click_mark" => {
                let tab = resolve_tab(tab_id)?;
                // 注意：这里不能用 `return` 提前收尾——那会跳过统一的「页面身份回填 + 计时留痕」。
                match args["index"].as_i64() {
                    Some(n) if n > 0 => {
                        let sel = format!("[data-baize-mark=\"{n}\"]");
                        match mark_selector_css(&tab, &sel, MARK_HOLD_ACT) {
                            Some((x, y, w, h)) => {
                                std::thread::sleep(MARK_LEAD);
                                click_at(&tab, x + w / 2.0, y + h / 2.0)?;
                                Ok(json!({ "ok": true, "index": n, "method": "scan_mark" }))
                            }
                            None => Err(format!(
                                "编号 {n} 已失效（页面变过了，或你之后又 scan 过一次）。先重新 scan 拿新编号"
                            )),
                        }
                    }
                    _ => Err("缺少参数 index：要填 scan 返回的正整数编号".into()),
                }
            }
            "new_tab" => {
                // 已经有白泽留下的空白标签页就直接用它，不再多开一个——
                // 用户要的「减轻浏览器压力」在源头上就该这么做：少开一个是一个。
                let (tab, reused) = match reuse_blank_tab() {
                    Some(t) => {
                        set_active_target(Some(t.get_target_id().as_str().to_string()));
                        remember_tab(&t);
                        register_tab(&t, false);
                        (t, true)
                    }
                    None => (new_chrome_tab()?, false),
                };
                if let Some(url) = args["url"].as_str() {
                    navigate_and_wait(&tab, url, Duration::from_secs(45))?;
                }
                Ok(json!({
                    "ok": true, "tab_id": tab.get_target_id(), "url": tab.get_url(),
                    "reused": reused,
                }))
            }
            "tabs" => {
                let active = active_target_id();
                // 列清单要准：忽略节流，强制全量补录
                let tabs = list_chrome_tabs_ex(true)?;
                let mut ours_n = 0usize;
                let mut blank_n = 0usize;
                let items: Vec<Value> = tabs
                    .iter()
                    .map(|t| {
                        let id = t.get_target_id().as_str().to_string();
                        let url = t.get_url();
                        let ours = tab_is_ours(&id);
                        let blank = is_blank_url(&url);
                        if ours {
                            ours_n += 1;
                        }
                        if blank {
                            blank_n += 1;
                        }
                        json!({
                            "id": id,
                            "url": url,
                            "title": tab_title(t),
                            "active": Some(t.get_target_id().as_str()) == active.as_deref(),
                            // ours=false 是机主自己的页面：只读、绝不关
                            "ours": ours,
                            "blank": blank,
                            "dialog": pending_dialog_for(t.get_target_id().as_str()).is_some(),
                        })
                    })
                    .collect();
                let stale = items
                    .iter()
                    .filter(|t| {
                        t["ours"].as_bool().unwrap_or(false)
                            && t["blank"].as_bool().unwrap_or(false)
                            && t["active"].as_bool().unwrap_or(false) == false
                    })
                    .count();
                let mut out = json!({
                    "count": items.len(),
                    "tabs": items,
                    "summary": {
                        "total": tabs.len(),
                        "ours": ours_n,
                        "user": tabs.len().saturating_sub(ours_n),
                        "blank": blank_n,
                        "cleanable": stale,
                    },
                });
                if stale > 0 {
                    out["hint"] = json!(format!(
                        "有 {stale} 个白泽自己开的空白标签页可以清掉（它们只占内存）：\
                         用 tidy_tabs 一次关掉。tabs 里 ours=false 的是机主自己的页面，不要关。"
                    ));
                }
                Ok(out)
            }
            "switch_tab" => {
                let id = tab_id.ok_or("缺少参数 tab_id")?;
                let tab = find_chrome_tab(id)?;
                tab.activate().map_err(|e| format!("切换标签页失败: {e}"))?;
                set_active_target(Some(id.to_string()));
                remember_tab(&tab);
                register_tab(&tab, false);
                Ok(json!({ "ok": true, "tab_id": id }))
            }
            "close_tab" => {
                // tab_id 可省略：省略就是关掉「当前操作的那个标签页」——正是任务收尾时
                // 「我开的新标签页要不要留」这个决定的最省事写法。
                let target_id = match tab_id {
                    Some(s) => s.to_string(),
                    None => last_tab()
                        .map(|t| t.get_target_id().as_str().to_string())
                        .ok_or("缺少参数 tab_id，且当前没有正在操作的标签页")?,
                };
                let tab = find_chrome_tab(&target_id)?;
                let url = tab.get_url();
                tab.close(false).map_err(|e| format!("关闭标签页失败: {e}"))?;
                unmark_tab_ours(&target_id);
                clear_pending_dialog(&target_id);
                if active_target_id().as_deref() == Some(target_id.as_str()) {
                    set_active_target(None);
                }
                Ok(json!({ "ok": true, "closed": target_id, "url": url }))
            }
            // 收尾清理：只关「白泽自己开的、不是当前操作对象、且已经是空白页」的标签页。
            // 机主自己开的页面一律不碰——多留几个标签页只是占点内存，误关页面才是事故。
            "tidy_tabs" => {
                let active = active_target_id();
                let keep = tab_id
                    .map(str::to_string)
                    .or_else(|| active.clone())
                    .or_else(|| last_tab().map(|t| t.get_target_id().as_str().to_string()));
                // all=true 时连「白泽开的非空白页」一起收（默认 false，只收空白页，最保守）
                let include_others = args["all"].as_bool().unwrap_or(false);
                let tabs = list_chrome_tabs_ex(true)?;
                let mut closed: Vec<Value> = Vec::new();
                let mut kept_user = 0usize;
                for t in &tabs {
                    let id = t.get_target_id().as_str().to_string();
                    if Some(id.clone()) == keep {
                        continue;
                    }
                    if !tab_is_ours(&id) {
                        kept_user += 1; // 机主自己的页面：只看不动
                        continue;
                    }
                    let url = t.get_url();
                    if !include_others && !is_blank_url(&url) {
                        continue;
                    }
                    if t.close(false).is_ok() {
                        unmark_tab_ours(&id);
                        closed.push(json!({ "tab_id": id, "url": url }));
                    }
                }
                Ok(json!({
                    "ok": true,
                    "closed": closed,
                    "closed_count": closed.len(),
                    "kept_user_tabs": kept_user,
                    "hint": if closed.is_empty() {
                        "没有可清理的空白标签页（机主自己开的页面不在清理范围内）。"
                    } else {
                        "已关掉白泽自己开的空白/多余标签页，浏览器压力随之下降。"
                    },
                }))
            }
            "handle_dialog" => {
                let tab = resolve_tab(tab_id)?;
                let accept = args["accept"].as_bool().unwrap_or(true);
                let text = args["text"].as_str().map(str::to_string);
                let dialog = tab.get_dialog();
                let res = if accept {
                    dialog.accept(text.clone())
                } else {
                    dialog.dismiss()
                };
                res.map_err(|e| {
                    format!(
                        "处理对话框失败：{e}。若返回值里并没有提示「页面弹出对话框」，\
                         说明当前没有对话框在等——这种情况不需要调这个动作。"
                    )
                })?;
                let tid = tab.get_target_id().as_str().to_string();
                clear_pending_dialog(&tid);
                Ok(json!({
                    "ok": true,
                    "accepted": accept,
                    "text": text,
                    "note": if accept { "已点「确定」" } else { "已点「取消」" },
                }))
            }
            // 文件上传：直接给页面上的 file 输入框塞文件，绕开系统文件选择器
            "upload" => {
                let tab = resolve_tab(tab_id)?;
                let sel = args["selector"].as_str().ok_or(
                    "缺少参数 selector：file 输入框的 CSS 选择器（通常是 input[type=file]）",
                )?;
                let files: Vec<String> = match &args["files"] {
                    Value::Array(a) => a
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect(),
                    Value::String(s) => vec![s.clone()],
                    _ => Vec::new(),
                };
                if files.is_empty() {
                    return Err("缺少参数 files：要上传的文件绝对路径（可以是数组）".into());
                }
                for f in &files {
                    if !std::path::Path::new(f).is_file() {
                        return Err(format!("文件不存在，无法上传：{f}"));
                    }
                }
                let refs: Vec<&str> = files.iter().map(|s| s.as_str()).collect();
                let el = tab.find_element(sel).map_err(|e| {
                    format!(
                        "找不到文件输入框 {sel}：{e}。若上传控件要先点一下按钮才把 input 放进 DOM，\
                         先点那个按钮（click_text / click_mark）再调 upload；\
                         若点按钮直接弹出了系统的文件选择窗口，那属于 CDP 够不着的原生对话框。"
                    )
                })?;
                el.set_input_files(&refs)
                    .map_err(|e| format!("设置上传文件失败: {e}"))?;
                Ok(json!({ "ok": true, "selector": sel, "files": files }))
            }
            // 下载目录：设一次即对之后打开的标签页生效（默认是系统的「下载」文件夹）
            "set_download_dir" => {
                let path = args["path"]
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                let dir = path.unwrap_or_else(default_download_dir);
                if dir.trim().is_empty() {
                    return Err("无法确定下载目录：请显式传 path（绝对路径）".into());
                }
                std::fs::create_dir_all(&dir)
                    .map_err(|e| format!("创建下载目录失败 {dir}：{e}"))?;
                if let Ok(mut g) = DOWNLOAD_DIR.lock() {
                    *g = Some(dir.clone());
                }
                let tab = resolve_tab(tab_id)?;
                let applied = apply_download_dir(&tab, &dir);
                let mut out = json!({ "ok": true, "download_dir": dir });
                if let Err(e) = applied {
                    out["warning"] =
                        json!(format!("{e}；本次下载会落到浏览器默认目录"));
                }
                Ok(out)
            }
            // 看下载目录里最近的文件：确认「刚才那一下下载成功了没、文件叫什么」
            "downloads" => {
                let dir = current_download_dir();
                let limit = args["limit"].as_u64().unwrap_or(8).clamp(1, 50) as usize;
                let mut items: Vec<Value> = Vec::new();
                let mut failed = String::new();
                match std::fs::read_dir(&dir) {
                    Ok(rd) => {
                        let mut entries: Vec<(std::time::SystemTime, Value)> = Vec::new();
                        for e in rd.flatten() {
                            let p = e.path();
                            if !p.is_file() {
                                continue;
                            }
                            let name = e.file_name().to_string_lossy().to_string();
                            let Ok(meta) = e.metadata() else { continue };
                            let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                            // .crdownload = Chrome 正在写的临时文件，说明这次下载还没结束
                            let in_progress = name.to_ascii_lowercase().ends_with(".crdownload");
                            entries.push((
                                modified,
                                json!({
                                    "name": name,
                                    "path": p.to_string_lossy(),
                                    "size_kb": (meta.len() as f64 / 1024.0).round() as u64,
                                    "in_progress": in_progress,
                                    "modified_epoch": modified
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .map(|d| d.as_secs())
                                        .unwrap_or(0),
                                }),
                            ));
                        }
                        entries.sort_by(|a, b| b.0.cmp(&a.0));
                        items = entries.into_iter().take(limit).map(|(_, v)| v).collect();
                    }
                    Err(e) => failed = format!("读取下载目录失败 {dir}：{e}"),
                }
                let mut out =
                    json!({ "ok": true, "dir": dir, "count": items.len(), "items": items });
                if !failed.is_empty() {
                    out["warning"] = json!(failed);
                }
                Ok(out)
            }
            "scroll" => {
                let tab = resolve_tab(tab_id)?;
                // 定位式滚动（首选）：把目标滚进视口就停，绝不盲滚——盲滚会把页面丢到
                // 用户没在看的地方，做完了还得滚回来。只有实在给不出目标时才用 amount。
                if let Some(sel) = args["selector"].as_str() {
                    let lit = serde_json::to_string(sel)
                        .map_err(|e| format!("选择器解析失败: {e}"))?;
                    let js = SCROLL_SELECTOR_JS.replace("__BAIZE_SEL__", &lit);
                    let raw = tab.evaluate(&js, false).map_err(|e| format!("滚动失败: {e}"))?;
                    let s = raw
                        .value
                        .as_ref()
                        .and_then(|v| v.as_str())
                        .unwrap_or("null")
                        .to_string();
                    if s == "null" {
                        Err(format!("页面上找不到元素 {sel}"))
                    } else {
                        let arr: Value = serde_json::from_str(&s)
                            .map_err(|e| format!("滚动结果解析失败: {e}"))?;
                        Ok(json!({
                            "ok": true, "method": "selector",
                            "moved": arr.get(4).and_then(|v| v.as_bool()).unwrap_or(false),
                            "y": arr.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0),
                            "page_y": arr.get(5).and_then(|v| v.as_f64()).unwrap_or(0.0),
                        }))
                    }
                } else if let Some(target) = args["target"].as_str() {
                    // 复用 DOM 文字定位：命中时它自己会在「需要滚才滚」的前提下把目标带进视口
                    match dom_locate_by_text(&tab, target) {
                        Some((x, y, w, h, text, tag)) => Ok(json!({
                            "ok": true, "method": "text",
                            "found": format!("{tag}:{text}"),
                            "x": x + w / 2.0, "y": y + h / 2.0,
                        })),
                        None => Err(format!("页面上找不到「{target}」")),
                    }
                } else {
                    let amount = args["amount"].as_f64().unwrap_or(500.0);
                    let js = format!("window.scrollBy(0, {amount})");
                    tab.evaluate(&js, false).map_err(|e| format!("滚动失败: {e}"))?;
                    Ok(json!({ "ok": true, "method": "amount", "amount": amount }))
                }
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
                // 状态要准：忽略节流，强制全量补录
                let tabs = list_chrome_tabs_ex(true)?;
                let current = tabs
                    .iter()
                    .find(|t| Some(t.get_target_id().as_str()) == active.as_deref())
                    .map(|t| {
                        json!({
                            "id": t.get_target_id(),
                            "url": t.get_url(),
                            "title": tab_title(t),
                        })
                    });
                let blank = tabs.iter().filter(|t| is_blank_url(&t.get_url())).count();
                let ours = tabs
                    .iter()
                    .filter(|t| tab_is_ours(t.get_target_id().as_str()))
                    .count();
                Ok(json!({
                    "tab_count": tabs.len(),
                    "own_tabs": ours,
                    "blank_tabs": blank,
                    "active_tab": current,
                    "origin": browser_origin(),
                    "download_dir": current_download_dir(),
                    "pending_dialog": pending_dialog_for_action(None),
                }))
            }
            _ => Err(format!("不支持的操作: {action}")),
        };
        // 耗时留痕（只记录、不影响行为）：把「进入网站之后每次定位/点击到底慢在哪」
        // 变成可查的事实，而不是靠猜。写进 exe 旁的 baize-overlay.log。
        // 注意只计动作本身，不含外层统一的「页面身份回填」，这样才能看出单步开销。
        crate::windows::diag_log(&format!(
            "[浏览器·计时] {action} {}ms",
            action_t0.elapsed().as_millis()
        ));
        out
    }));
        // 收尾：把两层 Result 压成一层（外层 = 库内部 panic 兜底，内层 = 动作本身的成败），
        // 再统一标注「这一步能不能降级到 GUI 自动化」，最后回填页面身份。
        // 顺序有讲究：先标注再回填，两者都会往错误信息里追加说明，互不覆盖。
        let out = match result {
            Ok(v) => v,
            Err(_) => Err(
                "浏览器操控异常：桌面 Chrome 连接可能已中断（是否被手动关闭？），请稍后重试"
                    .to_string(),
            ),
        };
        let out = out.map_err(|e| annotate_gui_fallback(&action_name, &e));
        // 统一回填「本步操作的页面是谁」＋「有没有冒出新的标签页」。放在这一层而不是闭包内：
        // 闭包里各分支用 `?` 提前返回的错误也从这里出去，失败同样带上页面身份——
        // 「在错的页面上找不到元素」和「页面本身有问题」是两回事，得让模型分得清。
        enrich_page_context(out, &action_name)
    }
}

/// 判断 browser_act 的失败是否属于「CDP 通道确实够不着」——只有这一类才建议降级到
/// 桌面 GUI 自动化。其余失败（页面里找不到元素、选择器写错、网络超时）都是**页面内**
/// 问题，继续用 browser_act 修正即可，跑去动屏幕只会把简单问题复杂化。
///
/// 这是「什么时候才该上 GUI 自动化」的判定边界，务必窄：宁可让模型多试一次 CDP，
/// 也不要动不动就接管屏幕。
fn annotate_gui_fallback(action: &str, err: &str) -> String {
    let needs_gui = [
        // 受控浏览器根本起不来：没装 Chrome/Edge、profile 被占用、启动超时
        "启动超时", "auto detect a chrome", "chrome executable", "profile 目录",
        // 实例已死：用户手动关了窗口 / 浏览器崩溃
        "连接可能已中断", "连接已中断",
        // 浏览器外壳与系统级对话框：CDP 完全够不着（文件选择器 / 另存为 / 打印 / 提权）
        "文件选择", "另存为", "保存对话框", "打印对话框",
    ]
    .iter()
    .any(|k| err.contains(k));
    if needs_gui {
        format!(
            "{err}\n【可降级】这一步已超出 CDP 通道的能力（action={action}）：\
             受控浏览器起不来或需要操作系统级对话框时，浏览器内部接口碰不到，\
             可改用桌面 GUI 自动化补齐——先用 screen_elements 看屏幕拿坐标，\
             再用 click_element / mouse_click / paste_text 操作（系统会自动进入屏幕接管）。\
             仅限上述情形；页面内找不到元素、选择器写错、加载超时等，继续用 browser_act 修正，不要动屏幕。"
        )
    } else {
        err.to_string()
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

/// 前端命令：在白泽内置浏览器中打开一个网页标签页（搜索结果页点击卡片 → 内置浏览器内跳转）。
/// 结果页自身保留为独立标签页，切回即「返回搜索结果」，不会丢失搜索上下文。
/// 必须为 async：Tauri v2 同步命令运行在主线程上，emit_update → ensure_browser_window
/// 需要在主线程创建窗口，同步命令会死锁（全应用无响应）。
#[tauri::command]
pub async fn browser_open_url_tab(
    app: AppHandle,
    state: tauri::State<'_, crate::AppState>,
    url: String,
) -> Result<Value, String> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(format!("非法 URL: {url}"));
    }
    let title = {
        // 取域名做标签页标题，加载完成后用户可从页面自行辨识
        url.split("://")
            .nth(1)
            .unwrap_or(&url)
            .split('/')
            .next()
            .unwrap_or("网页")
            .to_string()
    };
    // 内嵌探测放阻塞线程池：async 命令直接在 tokio worker 里做同步 HTTP 会饿死运行时
    let probe_url = url.clone();
    let embed = tokio::task::spawn_blocking(move || probe_embed(&probe_url))
        .await
        .map_err(|e| format!("内嵌探测失败: {e}"))?;
    {
        let mut s = state.browser.lock().unwrap();
        let id = s.open_tab("url", &title, &url);
        s.set_tab_embed(&id, embed.clone());
    }
    emit_update(&app, &state.browser.clone());
    Ok(json!({ "ok": true, "url": url, "embed": embed }))
}

/// 前端命令：按需探测某网址能否被内置 iframe 内嵌。
/// 打开标签页时已自动探测，此命令用于「重新检测」与异常站点复核；
/// 传 tab_id 时把结果同步写回该标签页并广播，前端与白泽随即看到最新判定。
#[tauri::command]
pub async fn browser_check_embed(
    app: AppHandle,
    state: tauri::State<'_, crate::AppState>,
    url: String,
    tab_id: Option<String>,
) -> Result<Value, String> {
    let probe_url = url.clone();
    let embed = tokio::task::spawn_blocking(move || probe_embed(&probe_url))
        .await
        .map_err(|e| format!("内嵌探测失败: {e}"))?;
    if let Some(id) = tab_id.as_deref() {
        {
            let mut s = state.browser.lock().unwrap();
            s.set_tab_embed(id, embed.clone());
        }
        emit_update(&app, &state.browser.clone());
    }
    Ok(json!({ "ok": true, "url": url, "embed": embed }))
}
// ───────────────────────── browser_close_window（由白泽自行决定关闭浏览器窗口） ─────────────────────────

/// browser_close_window 工具：关闭内置浏览器窗口（整个窗口，而非单个标签页；
/// 关标签页用 browser_close，清空标签用 browser_close_all）。
/// 典型场景：browser_search / 网页浏览完成后，信息已取回，由白泽判断不再需要浏览器时
/// 主动收起窗口，保持用户桌面清爽——要不要关、何时关，由白泽根据任务进展自行决定。
pub struct BrowserWindowCloseTool {
    app: AppHandle,
}

impl BrowserWindowCloseTool {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl Tool for BrowserWindowCloseTool {
    fn name(&self) -> &str {
        "browser_close_window"
    }
    fn description(&self) -> &str {
        "关闭内置浏览器窗口（整个窗口）。在完成搜索或网页查阅、信息已取回且用户大概率不再需要查看浏览器内容时调用；若用户正在浏览或可能还要看页面，则不要调用"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {}
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, _args: Value) -> Result<Value, String> {
        let closed = match self.app.get_webview_window("browser") {
            Some(win) => {
                win.close().map_err(|e| format!("关闭浏览器窗口失败: {e}"))?;
                true
            }
            None => false, // 本来就没开，视为已关
        };
        Ok(json!({ "ok": true, "closed": closed }))
    }
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

    fn hm(pairs: &[(&str, &str)]) -> reqwest::header::HeaderMap {
        let mut h = reqwest::header::HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                reqwest::header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                reqwest::header::HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    /// X-Frame-Options 三种取值都判定为拒绝内嵌（大小写不敏感）
    #[test]
    fn embed_judge_detects_x_frame_options() {
        for v in ["DENY", "deny", "SAMEORIGIN", "sameorigin", "ALLOW-FROM https://a.com"] {
            let r = judge_embed_headers(&hm(&[("x-frame-options", v)]));
            assert_eq!(r.status, "blocked", "{v} 应判定为 blocked");
            assert!(r.signal.unwrap().contains("X-Frame-Options"));
        }
    }

    /// CSP frame-ancestors：'none' / 'self' 拒绝；`*` 明确放行
    #[test]
    fn embed_judge_detects_csp_frame_ancestors() {
        let blocked = judge_embed_headers(&hm(&[(
            "content-security-policy",
            "default-src 'self'; frame-ancestors 'none'",
        )]));
        assert_eq!(blocked.status, "blocked");

        let self_only = judge_embed_headers(&hm(&[(
            "content-security-policy",
            "frame-ancestors 'self' https://a.com",
        )]));
        assert_eq!(self_only.status, "blocked");

        let wildcard = judge_embed_headers(&hm(&[(
            "content-security-policy",
            "frame-ancestors *",
        )]));
        assert_eq!(wildcard.status, "ok", "frame-ancestors * 是明确放行");
    }

    /// 非 HTML 内容只对**确实渲染不了**的类型判 blocked；text/plain、json、图片、音视频
    /// 在 iframe 里其实能正常显示，误判会把本来能看的页面弹去受控浏览器。
    #[test]
    fn embed_judge_content_type_only_blocks_unrenderable_files() {
        for ct in [
            "application/pdf",
            "application/octet-stream",
            "application/zip; name=a.zip",
            "application/x-msdownload",
        ] {
            assert_eq!(
                judge_embed_headers(&hm(&[("content-type", ct)])).status,
                "blocked",
                "{ct} 应判定为 blocked"
            );
        }
        for ct in [
            "text/plain; charset=utf-8",
            "application/json",
            "image/png",
            "video/mp4",
            "text/html; charset=utf-8",
        ] {
            assert_eq!(
                judge_embed_headers(&hm(&[("content-type", ct)])).status,
                "ok",
                "{ct} 不应被判为 blocked"
            );
        }
        // 没有任何相关头：视为可内嵌（站点可能用 JS 反嵌，只能靠运行时兜底）
        assert_eq!(judge_embed_headers(&hm(&[])).status, "ok");
    }

    /// HEAD 拿到非 HTML（或没有 Content-Type）时不足以判定内嵌性，需 GET 复核——
    /// 实测：`https://www.zhihu.com` 的 HEAD 被反爬敷衍成 `text/plain`，
    /// 真实响应头（GET）里才有 `X-Frame-Options: SAMEORIGIN` + `frame-ancestors *.zhihu.com`
    #[test]
    fn embed_recheck_needed_when_head_is_not_html() {
        assert!(needs_get_recheck(&hm(&[("content-type", "text/plain; charset=utf-8")])));
        assert!(needs_get_recheck(&hm(&[("content-type", "application/json")])));
        assert!(needs_get_recheck(&hm(&[])));
        assert!(!needs_get_recheck(&hm(&[("content-type", "text/html; charset=utf-8")])));
        assert!(!needs_get_recheck(&hm(&[("content-type", "application/xhtml+xml")])));
    }
}
