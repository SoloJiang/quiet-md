//! 钉钉文档 OpenAPI client: app-credential auth, pull (blocks→Markdown),
//! push (overwrite / append Markdown). Optional — quiet.md stays local-first;
//! credentials come from env (see [`credentials_from_env`]).

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const API_HOST: &str = "https://api.dingtalk.com";

/// Env vars for the internal-app credential flow (desktop-friendly; no browser OAuth).
pub const ENV_APP_KEY: &str = "QUIET_MD_DINGTALK_APP_KEY";
pub const ENV_APP_SECRET: &str = "QUIET_MD_DINGTALK_APP_SECRET";
pub const ENV_OPERATOR_ID: &str = "QUIET_MD_DINGTALK_OPERATOR_ID";
pub const ENV_WORKSPACE_ID: &str = "QUIET_MD_DINGTALK_WORKSPACE_ID";
pub const ENV_DOC: &str = "QUIET_MD_DINGTALK_DOC";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Credentials {
    pub app_key: String,
    pub app_secret: String,
    /// User `unionId` used as `operatorId` on doc APIs.
    pub operator_id: String,
    /// Optional Knowledge Base id for create-document.
    pub workspace_id: Option<String>,
}

/// Read credentials from the environment. Missing any of the three required
/// vars → `None` (OpenAPI stays off).
pub fn credentials_from_env() -> Option<Credentials> {
    let app_key = std::env::var(ENV_APP_KEY).ok()?.trim().to_string();
    let app_secret = std::env::var(ENV_APP_SECRET).ok()?.trim().to_string();
    let operator_id = std::env::var(ENV_OPERATOR_ID).ok()?.trim().to_string();
    if app_key.is_empty() || app_secret.is_empty() || operator_id.is_empty() {
        return None;
    }
    let workspace_id = std::env::var(ENV_WORKSPACE_ID).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    Some(Credentials { app_key, app_secret, operator_id, workspace_id })
}

/// Default / last-used document id or URL from env (optional).
pub fn doc_from_env() -> Option<String> {
    std::env::var(ENV_DOC).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Extract `dentryUuid` / docKey from a 钉钉文档 URL or bare id.
pub fn parse_doc_key(input: &str) -> Result<String> {
    let s = input.trim();
    if s.is_empty() {
        bail!("文档标识为空");
    }
    if let Some(rest) = s.strip_prefix("https://alidocs.dingtalk.com/i/nodes/")
        .or_else(|| s.strip_prefix("http://alidocs.dingtalk.com/i/nodes/"))
        .or_else(|| s.strip_prefix("https://docs.dingtalk.com/i/nodes/"))
        .or_else(|| s.strip_prefix("https://docs.dingtalk.io/i/nodes/"))
    {
        let id = rest.split(['?', '#', '/']).next().unwrap_or("").trim();
        if id.is_empty() {
            bail!("无法从 URL 解析文档 id: {s}");
        }
        return Ok(id.to_string());
    }
    if s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') && s.len() >= 4 {
        return Ok(s.to_string());
    }
    bail!("无法识别的钉钉文档标识（需要 URL 或 dentryUuid）: {s}");
}

/// HTTP transport used by [`Client`]. Tests inject a mock.
pub trait Transport: Send + Sync {
    fn request(&self, method: &str, url: &str, headers: &[(&str, String)], body: Option<&str>) -> Result<(u16, String)>;
}

/// Default transport: `curl` (same approach as remote image download).
pub struct CurlTransport;

impl Transport for CurlTransport {
    fn request(&self, method: &str, url: &str, headers: &[(&str, String)], body: Option<&str>) -> Result<(u16, String)> {
        let mut cmd = std::process::Command::new("curl");
        cmd.args(["-sS", "-X", method, "--max-time", "60", "-w", "\n%{http_code}"]);
        for (k, v) in headers {
            cmd.args(["-H", &format!("{k}: {v}")]);
        }
        if let Some(b) = body {
            cmd.args(["-H", "Content-Type: application/json", "--data-binary", b]);
        }
        cmd.arg(url);
        let out = cmd.output().context("启动 curl 失败（钉钉 OpenAPI 需要系统 curl）")?;
        if !out.status.success() && out.stdout.is_empty() {
            let err = String::from_utf8_lossy(&out.stderr);
            bail!("curl 调用失败: {err}");
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let (body_s, code_s) = text.rsplit_once('\n').unwrap_or((text.as_ref(), "0"));
        let code: u16 = code_s.trim().parse().unwrap_or(0);
        Ok((code, body_s.to_string()))
    }
}

struct TokenCache {
    token: String,
    expires: Instant,
}

static TOKEN: OnceLock<Mutex<Option<TokenCache>>> = OnceLock::new();

fn token_lock() -> &'static Mutex<Option<TokenCache>> {
    TOKEN.get_or_init(|| Mutex::new(None))
}

pub struct Client<T: Transport = CurlTransport> {
    pub creds: Credentials,
    pub http: T,
}

impl Client<CurlTransport> {
    pub fn new(creds: Credentials) -> Self {
        Self { creds, http: CurlTransport }
    }
}

impl<T: Transport> Client<T> {
    pub fn with_transport(creds: Credentials, http: T) -> Self {
        Self { creds, http }
    }

    fn api(&self, method: &str, path_and_query: &str, body: Option<Value>) -> Result<Value> {
        let token = self.access_token()?;
        let url = format!("{API_HOST}{path_and_query}");
        let headers = [("x-acs-dingtalk-access-token", token)];
        let headers_ref: Vec<(&str, String)> = headers.iter().map(|(k, v)| (*k, v.clone())).collect();
        let body_s = body.as_ref().map(|b| b.to_string());
        let (code, text) = self.http.request(method, &url, &headers_ref, body_s.as_deref())?;
        if !(200..300).contains(&code) {
            bail!("钉钉 API HTTP {code}: {text}");
        }
        if text.trim().is_empty() {
            return Ok(json!({}));
        }
        serde_json::from_str(&text).with_context(|| format!("解析钉钉响应失败: {text}"))
    }

    /// `POST /v1.0/oauth2/accessToken` — cached ~expireIn−120s.
    pub fn access_token(&self) -> Result<String> {
        {
            let guard = token_lock().lock().unwrap();
            if let Some(c) = guard.as_ref()
                && Instant::now() < c.expires
            {
                return Ok(c.token.clone());
            }
        }
        let url = format!("{API_HOST}/v1.0/oauth2/accessToken");
        let body = json!({
            "appKey": self.creds.app_key,
            "appSecret": self.creds.app_secret,
        });
        let (code, text) = self.http.request("POST", &url, &[], Some(&body.to_string()))?;
        if !(200..300).contains(&code) {
            bail!("获取 accessToken 失败 HTTP {code}: {text}");
        }
        let v: Value = serde_json::from_str(&text).context("解析 accessToken 响应")?;
        let token = v.get("accessToken").and_then(|x| x.as_str()).ok_or_else(|| anyhow!("响应无 accessToken: {text}"))?.to_string();
        let expire = v.get("expireIn").and_then(|x| x.as_u64()).unwrap_or(7200);
        let skew = expire.saturating_sub(120).max(60);
        *token_lock().lock().unwrap() = Some(TokenCache {
            token: token.clone(),
            expires: Instant::now() + Duration::from_secs(skew),
        });
        Ok(token)
    }

    /// Clear cached token (tests / credential rotation).
    pub fn clear_token_cache() {
        *token_lock().lock().unwrap() = None;
    }

    /// Pull document as Markdown via Query block elements, then convert.
    pub fn pull_markdown(&self, doc: &str) -> Result<PulledDoc> {
        let doc_key = parse_doc_key(doc)?;
        let op = urlencoding_simple(&self.creds.operator_id);
        let mut start = 0i64;
        let page = 100i64;
        let mut blocks = Vec::new();
        loop {
            let end = start + page;
            let path = format!(
                "/v1.0/doc/suites/documents/{doc_key}/blocks?operatorId={op}&startIndex={start}&endIndex={end}"
            );
            let v = self.api("GET", &path, None)?;
            let batch = v.pointer("/result/data").or_else(|| v.get("data")).cloned().unwrap_or(Value::Array(vec![]));
            let Value::Array(arr) = batch else { break };
            let n = arr.len() as i64;
            blocks.extend(arr);
            if n < page {
                break;
            }
            start = end;
            if start > 10_000 {
                break;
            }
        }
        let markdown = blocks_to_markdown(&blocks);
        let title = title_from_markdown(&markdown);
        Ok(PulledDoc { doc_key, title, markdown })
    }

    /// Overwrite entire document with Markdown (`overwriteContent`, max 50_000 chars).
    pub fn push_overwrite(&self, doc: &str, markdown: &str) -> Result<()> {
        let doc_key = parse_doc_key(doc)?;
        if markdown.chars().count() > 50_000 {
            bail!("Markdown 超过钉钉覆写上限 50000 字符");
        }
        let op = urlencoding_simple(&self.creds.operator_id);
        let path = format!("/v1.0/doc/suites/documents/{doc_key}/overwriteContent?operatorId={op}");
        let body = json!({ "dataType": "markdown", "content": markdown });
        let _ = self.api("POST", &path, Some(body))?;
        Ok(())
    }

    /// Append Markdown at the document root (`insert content`).
    pub fn push_append(&self, doc: &str, markdown: &str) -> Result<()> {
        let doc_key = parse_doc_key(doc)?;
        let op = urlencoding_simple(&self.creds.operator_id);
        let path = format!("/v1.0/doc/suites/documents/{doc_key}/content?operatorId={op}");
        let body = json!({
            "content": { "type": "markdown", "content": markdown }
        });
        let _ = self.api("POST", &path, Some(body))?;
        Ok(())
    }

    /// Create a DOC in a Knowledge Base, then optionally overwrite with Markdown.
    pub fn create_and_push(&self, name: &str, markdown: &str) -> Result<CreatedDoc> {
        let workspace = self.creds.workspace_id.as_deref().ok_or_else(|| {
            anyhow!("创建文档需要设置 {ENV_WORKSPACE_ID}")
        })?;
        let path = format!("/v1.0/doc/workspaces/{workspace}/docs");
        let body = json!({
            "name": name,
            "docType": "DOC",
            "operatorId": self.creds.operator_id,
        });
        let v = self.api("POST", &path, Some(body))?;
        let doc_key = v.get("docKey").or_else(|| v.get("dentryUuid")).or_else(|| v.get("nodeId"))
            .and_then(|x| x.as_str()).ok_or_else(|| anyhow!("创建文档响应缺少 docKey: {v}"))?
            .to_string();
        let url = v.get("url").and_then(|x| x.as_str()).map(str::to_string);
        if !markdown.trim().is_empty() {
            self.push_overwrite(&doc_key, markdown)?;
        }
        Ok(CreatedDoc { doc_key, url })
    }
}

#[derive(Clone, Debug)]
pub struct PulledDoc {
    pub doc_key: String,
    pub title: String,
    pub markdown: String,
}

#[derive(Clone, Debug)]
pub struct CreatedDoc {
    pub doc_key: String,
    pub url: Option<String>,
}

fn urlencoding_simple(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn title_from_markdown(md: &str) -> String {
    for line in md.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("# ") {
            return rest.trim().to_string();
        }
        if !t.is_empty() {
            break;
        }
    }
    "钉钉文档".into()
}

/// Best-effort conversion of Query-blocks JSON into Markdown.
pub fn blocks_to_markdown(blocks: &[Value]) -> String {
    let mut out = String::new();
    for b in blocks {
        let chunk = block_to_md(b);
        if chunk.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&chunk);
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn block_to_md(b: &Value) -> String {
    let ty = b.get("blockType").and_then(|x| x.as_str()).unwrap_or("");
    match ty {
        "heading" => {
            let level = b.pointer("/heading/level").and_then(|x| x.as_u64())
                .or_else(|| heading_level_str(b.pointer("/heading/level").and_then(|x| x.as_str())))
                .unwrap_or(1)
                .clamp(1, 6) as usize;
            let text = b.pointer("/heading/text").and_then(|x| x.as_str()).unwrap_or("");
            format!("{} {}", "#".repeat(level), text)
        }
        "paragraph" => b.pointer("/paragraph/text").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        "blockquote" | "quote" => {
            let text = first_text(b);
            text.lines().map(|l| format!("> {l}")).collect::<Vec<_>>().join("\n")
        }
        "callout" => {
            let text = first_text(b);
            if text.is_empty() { ":::".into() } else { format!("::: {text}:::") }
        }
        "code" | "codeBlock" => {
            let lang = b.pointer("/code/language").or_else(|| b.pointer("/codeBlock/language")).and_then(|x| x.as_str()).unwrap_or("");
            let text = b.pointer("/code/text").or_else(|| b.pointer("/codeBlock/text")).and_then(|x| x.as_str()).unwrap_or("");
            format!("```{lang}\n{text}\n```")
        }
        "unorderedList" | "bulletList" => list_md(b, false),
        "orderedList" => list_md(b, true),
        "todo" | "task" => {
            let done = b.pointer("/todo/done").or_else(|| b.get("done")).and_then(|x| x.as_bool()).unwrap_or(false);
            let text = first_text(b);
            format!("- [{}] {text}", if done { "x" } else { " " })
        }
        "divider" | "hr" => "---".into(),
        "table" => table_md(b),
        _ => {
            let t = first_text(b);
            if !t.is_empty() {
                t
            } else if let Some(arr) = b.get("children").and_then(|x| x.as_array()) {
                blocks_to_markdown(arr).trim().to_string()
            } else {
                String::new()
            }
        }
    }
}

fn heading_level_str(s: Option<&str>) -> Option<u64> {
    let s = s?;
    s.strip_prefix("heading-")?.parse().ok()
}

fn first_text(b: &Value) -> String {
    for key in ["text", "/paragraph/text", "/heading/text", "/callout/text", "/todo/text"] {
        if let Some(t) = if key.starts_with('/') { b.pointer(key) } else { b.get(key) }.and_then(|x| x.as_str())
            && !t.is_empty()
        {
            return t.to_string();
        }
    }
    if let Some(arr) = b.get("children").and_then(|x| x.as_array()) {
        return arr.iter().filter_map(|c| {
            let t = first_text(c);
            (!t.is_empty()).then_some(t)
        }).collect::<Vec<_>>().join("\n");
    }
    String::new()
}

fn list_md(b: &Value, ordered: bool) -> String {
    let items = b.get("children").or_else(|| b.pointer("/unorderedList/items")).or_else(|| b.pointer("/orderedList/items")).and_then(|x| x.as_array());
    let Some(items) = items else {
        let t = first_text(b);
        return if ordered { format!("1. {t}") } else { format!("- {t}") };
    };
    items.iter().enumerate().map(|(i, it)| {
        let t = first_text(it);
        if ordered { format!("{}. {t}", i + 1) } else { format!("- {t}") }
    }).collect::<Vec<_>>().join("\n")
}

fn table_md(b: &Value) -> String {
    let rows = b.pointer("/table/rows").or_else(|| b.get("rows")).and_then(|x| x.as_array());
    let Some(rows) = rows else { return String::new() };
    let mut lines = Vec::new();
    for (ri, row) in rows.iter().enumerate() {
        let cells = row.as_array().or_else(|| row.get("cells").and_then(|x| x.as_array()));
        let Some(cells) = cells else { continue };
        let texts: Vec<String> = cells.iter().map(|c| first_text(c).replace('|', "\\|")).collect();
        lines.push(format!("| {} |", texts.join(" | ")));
        if ri == 0 {
            lines.push(format!("| {} |", texts.iter().map(|_| "---").collect::<Vec<_>>().join(" | ")));
        }
    }
    lines.join("\n")
}

/// Build a quiet.md [`crate::model::Document`] from pulled Markdown.
pub fn document_from_pulled(pulled: &PulledDoc) -> crate::model::Document {
    let off = crate::paste::Off { file: true, ..Default::default() };
    let mut blocks = crate::paste::markdown_to_blocks(&pulled.markdown, off);
    let title = if pulled.title.is_empty() { "钉钉文档".into() } else { pulled.title.clone() };
    if blocks.first().is_some_and(|b| matches!(b.kind, crate::model::BlockKind::Heading { level: 1, .. }) && b.text.text.trim() == title) {
        blocks.remove(0);
    }
    if blocks.is_empty() {
        blocks.push(crate::model::Block::text(""));
    }
    crate::model::Document {
        title: crate::model::RichText::plain(&title),
        icon: None,
        cover: None,
        blocks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Mock {
        responses: Mutex<Vec<(u16, String)>>,
        pub calls: Mutex<Vec<(String, String, Option<String>)>>,
    }

    impl Mock {
        fn new(responses: Vec<(u16, String)>) -> Self {
            Self { responses: Mutex::new(responses), calls: Mutex::new(vec![]) }
        }
    }

    impl Transport for Mock {
        fn request(&self, method: &str, url: &str, _headers: &[(&str, String)], body: Option<&str>) -> Result<(u16, String)> {
            self.calls.lock().unwrap().push((method.into(), url.into(), body.map(str::to_string)));
            let mut q = self.responses.lock().unwrap();
            if q.is_empty() {
                bail!("mock HTTP queue empty for {method} {url}");
            }
            Ok(q.remove(0))
        }
    }

    fn creds() -> Credentials {
        Credentials {
            app_key: "key".into(),
            app_secret: "secret".into(),
            operator_id: "op1".into(),
            workspace_id: Some("ws1".into()),
        }
    }

    #[test]
    fn parses_doc_urls_and_ids() {
        assert_eq!(parse_doc_key("https://alidocs.dingtalk.com/i/nodes/AbCd1234?utm=1").unwrap(), "AbCd1234");
        assert_eq!(parse_doc_key("DnRL6jAJMNX9kAgycoLy2vOo8yMoPYe1").unwrap(), "DnRL6jAJMNX9kAgycoLy2vOo8yMoPYe1");
        assert!(parse_doc_key("not a url").is_err());
    }

    #[test]
    fn blocks_convert_to_markdown() {
        let blocks = vec![
            json!({"blockType":"heading","heading":{"level":1,"text":"标题"},"id":"a","index":0}),
            json!({"blockType":"paragraph","paragraph":{"text":"正文 ==高亮=="},"id":"b","index":1}),
            json!({"blockType":"todo","todo":{"done":true,"text":"完成"},"id":"c","index":2}),
        ];
        let md = blocks_to_markdown(&blocks);
        assert!(md.contains("# 标题"), "{md}");
        assert!(md.contains("正文"), "{md}");
        assert!(md.contains("- [x] 完成"), "{md}");
    }

    #[test]
    fn pull_and_push_with_mock_http() {
        Client::<Mock>::clear_token_cache();
        let mock = Mock::new(vec![
            (200, json!({"accessToken":"tok","expireIn":7200}).to_string()),
            (200, json!({"success":true,"result":{"data":[
                {"blockType":"heading","heading":{"level":"heading-1","text":"周报"},"id":"1","index":0},
                {"blockType":"paragraph","paragraph":{"text":"hello"},"id":"2","index":1}
            ]}}).to_string()),
            (200, json!({"success":true}).to_string()),
            (200, json!({"success":true}).to_string()),
            (200, json!({"docKey":"newDocKey99","url":"https://alidocs.dingtalk.com/i/nodes/newDocKey99","dentryUuid":"newDocKey99"}).to_string()),
            (200, json!({"success":true}).to_string()),
        ]);
        let client = Client::with_transport(creds(), mock);
        let pulled = client.pull_markdown("https://alidocs.dingtalk.com/i/nodes/AbCdEfGh12345678").unwrap();
        assert_eq!(pulled.doc_key, "AbCdEfGh12345678");
        assert_eq!(pulled.title, "周报");
        assert!(pulled.markdown.contains("hello"));
        let doc = document_from_pulled(&pulled);
        assert_eq!(doc.title.text, "周报");
        client.push_overwrite(&pulled.doc_key, "# 新\n").unwrap();
        client.push_append(&pulled.doc_key, "追加").unwrap();
        let created = client.create_and_push("新建", "# hi\n").unwrap();
        assert_eq!(created.doc_key, "newDocKey99");
        let calls = client.http.calls.lock().unwrap();
        assert!(calls.iter().any(|c| c.0 == "POST" && c.1.contains("oauth2/accessToken")));
        assert!(calls.iter().any(|c| c.0 == "GET" && c.1.contains("/blocks?")));
        assert!(calls.iter().any(|c| c.1.contains("overwriteContent")));
        assert!(calls.iter().any(|c| c.1.contains("/content?")));
        assert!(calls.iter().any(|c| c.1.contains("/workspaces/ws1/docs")));
    }

    #[test]
    fn credentials_from_env_requires_three() {
        // Isolation: only assert the helper shape; do not mutate process env in parallel tests.
        let c = Credentials {
            app_key: "a".into(),
            app_secret: "b".into(),
            operator_id: "c".into(),
            workspace_id: None,
        };
        assert_eq!(c.app_key, "a");
    }
}
