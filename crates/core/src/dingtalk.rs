//! 钉钉文档 OpenAPI：以**用户 OAuth（个人授权）**为主路径读写文档。
//!
//! - AppKey / AppSecret 仅作 OAuth `clientId` / `clientSecret`（换 token 必需）。
//! - 文档读写使用用户 `accessToken`（及从 `/contact/users/me` 得到的 `unionId`）。
//! - 桌面默认：系统浏览器打开授权页 + `http://127.0.0.1:17865/callback` 本机回调。
//!
//! 文件 / 粘贴导入不依赖本模块。见 `docs/DINGTALK.md`。

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{Read, Write as IoWrite};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const API_HOST: &str = "https://api.dingtalk.com";
const AUTH_HOST: &str = "https://login.dingtalk.com/oauth2/auth";
const DEFAULT_REDIRECT: &str = "http://127.0.0.1:17865/callback";
const DEFAULT_SCOPE: &str = "openid corpid";
const OAUTH_FILE: &str = "dingtalk-oauth.json";

pub const ENV_APP_KEY: &str = "QUIET_MD_DINGTALK_APP_KEY";
pub const ENV_APP_SECRET: &str = "QUIET_MD_DINGTALK_APP_SECRET";
pub const ENV_REDIRECT_URI: &str = "QUIET_MD_DINGTALK_REDIRECT_URI";
pub const ENV_WORKSPACE_ID: &str = "QUIET_MD_DINGTALK_WORKSPACE_ID";
pub const ENV_DOC: &str = "QUIET_MD_DINGTALK_DOC";
/// One-shot: exchange this authCode without opening a browser (tests / manual).
pub const ENV_AUTH_CODE: &str = "QUIET_MD_DINGTALK_AUTH_CODE";

/// OAuth client registration (AppKey/AppSecret). Not used as app accessToken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OAuthApp {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    pub workspace_id: Option<String>,
}

pub fn oauth_app_from_env() -> Option<OAuthApp> {
    let client_id = std::env::var(ENV_APP_KEY).ok()?.trim().to_string();
    let client_secret = std::env::var(ENV_APP_SECRET).ok()?.trim().to_string();
    if client_id.is_empty() || client_secret.is_empty() {
        return None;
    }
    let redirect_uri = std::env::var(ENV_REDIRECT_URI)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_REDIRECT.into());
    let workspace_id = std::env::var(ENV_WORKSPACE_ID).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    Some(OAuthApp { client_id, client_secret, redirect_uri, workspace_id })
}

pub fn doc_from_env() -> Option<String> {
    std::env::var(ENV_DOC).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

pub fn oauth_store_path() -> PathBuf {
    crate::store::Store::root_dir().join(OAUTH_FILE)
}

/// Persisted user session after consent.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserSession {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds when `access_token` should be treated as expired.
    pub expires_at: u64,
    pub union_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nick: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corp_id: Option<String>,
}

impl UserSession {
    pub fn access_valid(&self) -> bool {
        now_secs() + 60 < self.expires_at
    }
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn load_session(path: &Path) -> Option<UserSession> {
    let b = std::fs::read(path).ok()?;
    serde_json::from_slice(&b).ok()
}

pub fn save_session(path: &Path, session: &UserSession) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(session)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn clear_session(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

pub fn is_connected(path: &Path) -> bool {
    load_session(path).is_some_and(|s| !s.refresh_token.is_empty() && !s.union_id.is_empty())
}

pub fn parse_doc_key(input: &str) -> Result<String> {
    let s = input.trim();
    if s.is_empty() {
        bail!("文档标识为空");
    }
    if let Some(rest) = s
        .strip_prefix("https://alidocs.dingtalk.com/i/nodes/")
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

pub fn authorize_url(app: &OAuthApp, state: &str) -> String {
    format!(
        "{AUTH_HOST}?redirect_uri={}&response_type=code&client_id={}&scope={}&state={}&prompt=consent",
        urlencoding_simple(&app.redirect_uri),
        urlencoding_simple(&app.client_id),
        urlencoding_simple(DEFAULT_SCOPE),
        urlencoding_simple(state),
    )
}

/// HTTP transport used by [`Client`]. Tests inject a mock.
pub trait Transport: Send + Sync {
    fn request(&self, method: &str, url: &str, headers: &[(&str, String)], body: Option<&str>) -> Result<(u16, String)>;
}

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
        let out = cmd.output().context("启动 curl 失败（钉钉 OAuth 需要系统 curl）")?;
        if !out.status.success() && out.stdout.is_empty() {
            bail!("curl 调用失败: {}", String::from_utf8_lossy(&out.stderr));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let (body_s, code_s) = text.rsplit_once('\n').unwrap_or((text.as_ref(), "0"));
        let code: u16 = code_s.trim().parse().unwrap_or(0);
        Ok((code, body_s.to_string()))
    }
}

pub struct Client<T: Transport = CurlTransport> {
    pub app: OAuthApp,
    pub session: Mutex<UserSession>,
    pub session_path: PathBuf,
    pub http: T,
}

impl Client<CurlTransport> {
    pub fn open(app: OAuthApp, session: UserSession, session_path: PathBuf) -> Self {
        Self { app, session: Mutex::new(session), session_path, http: CurlTransport }
    }

    /// Load saved session (refreshing access token if needed).
    pub fn from_store(app: OAuthApp) -> Result<Self> {
        let path = oauth_store_path();
        let session = load_session(&path).ok_or_else(|| anyhow!("尚未连接钉钉：请先「连接钉钉账号」"))?;
        let client = Self::open(app, session, path);
        client.ensure_access_token()?;
        Ok(client)
    }
}

impl<T: Transport> Client<T> {
    pub fn with_transport(app: OAuthApp, session: UserSession, session_path: PathBuf, http: T) -> Self {
        Self { app, session: Mutex::new(session), session_path, http }
    }

    fn api_user(&self, method: &str, path_and_query: &str, body: Option<Value>) -> Result<Value> {
        let token = self.ensure_access_token()?;
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

    /// Ensure a fresh user accessToken (refresh if needed); returns it.
    pub fn ensure_access_token(&self) -> Result<String> {
        {
            let s = self.session.lock().unwrap();
            if s.access_valid() {
                return Ok(s.access_token.clone());
            }
        }
        self.refresh_user_token()?;
        Ok(self.session.lock().unwrap().access_token.clone())
    }

    pub fn union_id(&self) -> String {
        self.session.lock().unwrap().union_id.clone()
    }

    pub fn nick(&self) -> Option<String> {
        self.session.lock().unwrap().nick.clone()
    }

    fn exchange_token(&self, grant_type: &str, code: Option<&str>, refresh: Option<&str>) -> Result<UserSession> {
        let url = format!("{API_HOST}/v1.0/oauth2/userAccessToken");
        let mut body = json!({
            "clientId": self.app.client_id,
            "clientSecret": self.app.client_secret,
            "grantType": grant_type,
        });
        if let Some(c) = code {
            body["code"] = json!(c);
        }
        if let Some(r) = refresh {
            body["refreshToken"] = json!(r);
        }
        let (status, text) = self.http.request("POST", &url, &[], Some(&body.to_string()))?;
        if !(200..300).contains(&status) {
            bail!("换取用户 token 失败 HTTP {status}: {text}");
        }
        let v: Value = serde_json::from_str(&text).context("解析 userAccessToken")?;
        let access_token = v.get("accessToken").and_then(|x| x.as_str()).ok_or_else(|| anyhow!("无 accessToken"))?.to_string();
        let refresh_token = v
            .get("refreshToken")
            .and_then(|x| x.as_str())
            .map(str::to_string)
            .or_else(|| refresh.map(str::to_string))
            .unwrap_or_default();
        let expire = v.get("expireIn").and_then(|x| x.as_u64()).unwrap_or(7200);
        let corp_id = v.get("corpId").and_then(|x| x.as_str()).map(str::to_string);
        // Fetch profile with the new token.
        let (st2, profile_text) = self.http.request(
            "GET",
            &format!("{API_HOST}/v1.0/contact/users/me"),
            &[("x-acs-dingtalk-access-token", access_token.clone())],
            None,
        )?;
        if !(200..300).contains(&st2) {
            bail!("获取用户信息失败 HTTP {st2}: {profile_text}");
        }
        let p: Value = serde_json::from_str(&profile_text).context("解析用户信息")?;
        let union_id = p.get("unionId").and_then(|x| x.as_str()).ok_or_else(|| anyhow!("用户信息无 unionId"))?.to_string();
        let nick = p.get("nick").and_then(|x| x.as_str()).map(str::to_string);
        Ok(UserSession {
            access_token,
            refresh_token,
            expires_at: now_secs() + expire.saturating_sub(120).max(60),
            union_id,
            nick,
            corp_id,
        })
    }

    pub fn refresh_user_token(&self) -> Result<()> {
        let refresh = self.session.lock().unwrap().refresh_token.clone();
        if refresh.is_empty() {
            bail!("无 refreshToken，请重新连接钉钉账号");
        }
        let next = self.exchange_token("refresh_token", None, Some(&refresh))?;
        save_session(&self.session_path, &next)?;
        *self.session.lock().unwrap() = next;
        Ok(())
    }

    /// Complete OAuth with an authCode (from loopback callback or `ENV_AUTH_CODE`).
    pub fn finish_auth_code(app: &OAuthApp, code: &str, http: &impl Transport, path: &Path) -> Result<UserSession> {
        let url = format!("{API_HOST}/v1.0/oauth2/userAccessToken");
        let body = json!({
            "clientId": app.client_id,
            "clientSecret": app.client_secret,
            "code": code,
            "grantType": "authorization_code",
        });
        let (status, text) = http.request("POST", &url, &[], Some(&body.to_string()))?;
        if !(200..300).contains(&status) {
            bail!("授权码换 token 失败 HTTP {status}: {text}");
        }
        let v: Value = serde_json::from_str(&text).context("解析 userAccessToken")?;
        let access_token = v.get("accessToken").and_then(|x| x.as_str()).ok_or_else(|| anyhow!("无 accessToken"))?.to_string();
        let refresh_token = v.get("refreshToken").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let expire = v.get("expireIn").and_then(|x| x.as_u64()).unwrap_or(7200);
        let corp_id = v.get("corpId").and_then(|x| x.as_str()).map(str::to_string);
        let (st2, profile_text) = http.request(
            "GET",
            &format!("{API_HOST}/v1.0/contact/users/me"),
            &[("x-acs-dingtalk-access-token", access_token.clone())],
            None,
        )?;
        if !(200..300).contains(&st2) {
            bail!("获取用户信息失败 HTTP {st2}: {profile_text}");
        }
        let p: Value = serde_json::from_str(&profile_text).context("解析用户信息")?;
        let union_id = p.get("unionId").and_then(|x| x.as_str()).ok_or_else(|| anyhow!("用户信息无 unionId"))?.to_string();
        let nick = p.get("nick").and_then(|x| x.as_str()).map(str::to_string);
        let session = UserSession {
            access_token,
            refresh_token,
            expires_at: now_secs() + expire.saturating_sub(120).max(60),
            union_id,
            nick,
            corp_id,
        };
        save_session(path, &session)?;
        Ok(session)
    }

    /// Pull document as Markdown via Query block elements (user token + unionId).
    pub fn pull_markdown(&self, doc: &str) -> Result<PulledDoc> {
        let doc_key = parse_doc_key(doc)?;
        let op = urlencoding_simple(&self.union_id());
        let mut start = 0i64;
        let page = 100i64;
        let mut blocks = Vec::new();
        loop {
            let end = start + page;
            let path = format!(
                "/v1.0/doc/suites/documents/{doc_key}/blocks?operatorId={op}&startIndex={start}&endIndex={end}"
            );
            let v = self.api_user("GET", &path, None)?;
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

    /// User-delegated overwrite: `POST /v2.0/doc/me/suites/documents/{docKey}/overwriteContent`.
    pub fn push_overwrite(&self, doc: &str, markdown: &str) -> Result<()> {
        let doc_key = parse_doc_key(doc)?;
        if markdown.chars().count() > 50_000 {
            bail!("Markdown 超过钉钉覆写上限 50000 字符");
        }
        let path = format!("/v2.0/doc/me/suites/documents/{doc_key}/overwriteContent");
        let body = json!({ "dataType": "markdown", "content": markdown });
        let _ = self.api_user("POST", &path, Some(body))?;
        Ok(())
    }

    /// Append Markdown (v1 insert with operatorId = user unionId).
    pub fn push_append(&self, doc: &str, markdown: &str) -> Result<()> {
        let doc_key = parse_doc_key(doc)?;
        let op = urlencoding_simple(&self.union_id());
        let path = format!("/v1.0/doc/suites/documents/{doc_key}/content?operatorId={op}");
        let body = json!({ "content": { "type": "markdown", "content": markdown } });
        let _ = self.api_user("POST", &path, Some(body))?;
        Ok(())
    }

    pub fn create_and_push(&self, name: &str, markdown: &str) -> Result<CreatedDoc> {
        let workspace = self.app.workspace_id.as_deref().ok_or_else(|| anyhow!("创建文档需要设置 {ENV_WORKSPACE_ID}"))?;
        let path = format!("/v1.0/doc/workspaces/{workspace}/docs");
        let body = json!({
            "name": name,
            "docType": "DOC",
            "operatorId": self.union_id(),
        });
        let v = self.api_user("POST", &path, Some(body))?;
        let doc_key = v
            .get("docKey")
            .or_else(|| v.get("dentryUuid"))
            .or_else(|| v.get("nodeId"))
            .and_then(|x| x.as_str())
            .ok_or_else(|| anyhow!("创建文档响应缺少 docKey: {v}"))?
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

/// Open system browser to `url`.
pub fn open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let st = std::process::Command::new("open").arg(url).status().context("open 浏览器失败")?;
        if !st.success() {
            bail!("open 返回失败");
        }
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    {
        let st = std::process::Command::new("cmd").args(["/C", "start", "", url]).status().context("start 浏览器失败")?;
        if !st.success() {
            bail!("start 返回失败");
        }
        return Ok(());
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        for bin in ["xdg-open", "gio"] {
            if std::process::Command::new(bin).arg(url).status().map(|s| s.success()).unwrap_or(false) {
                return Ok(());
            }
        }
        bail!("无法打开浏览器，请手动访问: {url}");
    }
}

/// Bind the redirect loopback, open the browser, wait for `authCode`.
///
/// `redirect_uri` must match the console registration (default
/// `http://127.0.0.1:17865/callback`).
pub fn interactive_login(app: &OAuthApp) -> Result<UserSession> {
    if let Ok(code) = std::env::var(ENV_AUTH_CODE) {
        let code = code.trim();
        if !code.is_empty() {
            return Client::<CurlTransport>::finish_auth_code(app, code, &CurlTransport, &oauth_store_path());
        }
    }
    let redirect = parse_redirect_uri(&app.redirect_uri)?;
    let listener = TcpListener::bind((redirect.host.as_str(), redirect.port)).with_context(|| {
        format!("无法监听回调 {}:{}（请确认控制台 redirect URI 一致，且端口空闲）", redirect.host, redirect.port)
    })?;
    listener.set_nonblocking(true)?;
    let state = format!("q{}", now_secs());
    let auth = authorize_url(app, &state);
    open_browser(&auth)?;
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        if Instant::now() > deadline {
            bail!("等待钉钉授权超时（5 分钟）");
        }
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = stream.set_nonblocking(false);
                if let Some(code) = handle_oauth_callback(stream, &state)? {
                    return Client::<CurlTransport>::finish_auth_code(app, &code, &CurlTransport, &oauth_store_path());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => return Err(e.into()),
        }
    }
}

struct RedirectUri {
    host: String,
    port: u16,
}

fn parse_redirect_uri(s: &str) -> Result<RedirectUri> {
    let s = s.trim();
    let rest = s.strip_prefix("http://").or_else(|| s.strip_prefix("https://")).ok_or_else(|| anyhow!("redirect_uri 需为 http(s) URL"))?;
    let (hostport, _) = rest.split_once('/').map(|(h, p)| (h, format!("/{p}"))).unwrap_or((rest, "/".into()));
    let (host, port) = if let Some((h, p)) = hostport.split_once(':') {
        (h.to_string(), p.parse().unwrap_or(80))
    } else {
        (hostport.to_string(), if s.starts_with("https://") { 443 } else { 80 })
    };
    if host != "127.0.0.1" && host != "localhost" {
        bail!("桌面回调仅支持 127.0.0.1 / localhost，当前: {host}");
    }
    Ok(RedirectUri { host, port })
}

fn handle_oauth_callback(mut stream: TcpStream, expect_state: &str) -> Result<Option<String>> {
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf).unwrap_or(0);
    let req = String::from_utf8_lossy(&buf[..n]);
    let line = req.lines().next().unwrap_or("");
    let path = line.split_whitespace().nth(1).unwrap_or("");
    let q = path.split_once('?').map(|(_, q)| q).unwrap_or("");
    let mut auth_code = None;
    let mut state = None;
    let mut error = None;
    for part in q.split('&') {
        if let Some((k, v)) = part.split_once('=') {
            match k {
                "authCode" | "code" => auth_code = Some(url_decode(v)),
                "state" => state = Some(url_decode(v)),
                "error" => error = Some(url_decode(v)),
                _ => {}
            }
        }
    }
    let denied = error.is_some();
    let (ok, body) = if let Some(err) = error {
        (false, format!("<!doctype html><meta charset=utf-8><title>钉钉授权失败</title><p>授权失败：{err}</p><p>可以关闭此窗口。</p>"))
    } else if auth_code.is_some() && state.as_deref() == Some(expect_state) {
        (true, "<!doctype html><meta charset=utf-8><title>钉钉授权成功</title><p>已连接钉钉账号，可以关闭此窗口回到 quiet.md。</p>".into())
    } else if auth_code.is_some() {
        (false, "<!doctype html><meta charset=utf-8><title>钉钉授权</title><p>state 不匹配，请重试连接。</p>".into())
    } else {
        let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return Ok(None);
    };
    let status = if ok { "200 OK" } else { "400 Bad Request" };
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes());
    if ok {
        Ok(auth_code)
    } else if denied {
        bail!("钉钉授权被拒绝或失败");
    } else {
        Ok(None)
    }
}

fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

fn urlencoding_simple(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push_str("%20"),
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
            let level = b
                .pointer("/heading/level")
                .and_then(|x| x.as_u64())
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
            if text.is_empty() {
                ":::".into()
            } else {
                format!("::: {text}:::")
            }
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
    s?.strip_prefix("heading-")?.parse().ok()
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
        return arr
            .iter()
            .filter_map(|c| {
                let t = first_text(c);
                (!t.is_empty()).then_some(t)
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    String::new()
}

fn list_md(b: &Value, ordered: bool) -> String {
    let items = b
        .get("children")
        .or_else(|| b.pointer("/unorderedList/items"))
        .or_else(|| b.pointer("/orderedList/items"))
        .and_then(|x| x.as_array());
    let Some(items) = items else {
        let t = first_text(b);
        return if ordered { format!("1. {t}") } else { format!("- {t}") };
    };
    items
        .iter()
        .enumerate()
        .map(|(i, it)| {
            let t = first_text(it);
            if ordered {
                format!("{}. {t}", i + 1)
            } else {
                format!("- {t}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn table_md(b: &Value) -> String {
    let rows = b.pointer("/table/rows").or_else(|| b.get("rows")).and_then(|x| x.as_array());
    let Some(rows) = rows else {
        return String::new();
    };
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

pub fn document_from_pulled(pulled: &PulledDoc) -> crate::model::Document {
    let off = crate::paste::Off { file: true, ..Default::default() };
    let mut blocks = crate::paste::markdown_to_blocks(&pulled.markdown, off);
    let title = if pulled.title.is_empty() { "钉钉文档".into() } else { pulled.title.clone() };
    if blocks
        .first()
        .is_some_and(|b| matches!(b.kind, crate::model::BlockKind::Heading { level: 1, .. }) && b.text.text.trim() == title)
    {
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

    fn app() -> OAuthApp {
        OAuthApp {
            client_id: "dingclient".into(),
            client_secret: "secret".into(),
            redirect_uri: DEFAULT_REDIRECT.into(),
            workspace_id: Some("ws1".into()),
        }
    }

    fn session() -> UserSession {
        UserSession {
            access_token: "user-tok".into(),
            refresh_token: "ref-tok".into(),
            expires_at: now_secs() + 3600,
            union_id: "union-me".into(),
            nick: Some("测试".into()),
            corp_id: Some("corp1".into()),
        }
    }

    #[test]
    fn authorize_url_contains_client_and_consent() {
        let u = authorize_url(&app(), "st1");
        assert!(u.contains("login.dingtalk.com/oauth2/auth"));
        assert!(u.contains("client_id=dingclient"));
        assert!(u.contains("prompt=consent"));
        assert!(u.contains("state=st1"));
        assert!(u.contains("openid%20corpid"), "{u}");
    }

    #[test]
    fn parses_doc_urls_and_ids() {
        assert_eq!(parse_doc_key("https://alidocs.dingtalk.com/i/nodes/AbCd1234?utm=1").unwrap(), "AbCd1234");
        assert_eq!(parse_doc_key("DnRL6jAJMNX9kAgycoLy2vOo8yMoPYe1").unwrap(), "DnRL6jAJMNX9kAgycoLy2vOo8yMoPYe1");
    }

    #[test]
    fn session_round_trip_file() {
        let dir = std::env::temp_dir().join(format!("fdl-dt-oauth-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(OAUTH_FILE);
        let s = session();
        save_session(&path, &s).unwrap();
        let loaded = load_session(&path).unwrap();
        assert_eq!(loaded.union_id, "union-me");
        assert!(is_connected(&path));
        clear_session(&path).unwrap();
        assert!(!is_connected(&path));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn finish_auth_code_and_doc_ops() {
        let dir = std::env::temp_dir().join(format!("fdl-dt-oauth2-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(OAUTH_FILE);
        let mock = Mock::new(vec![
            (200, json!({"accessToken":"uat","refreshToken":"urt","expireIn":7200,"corpId":"c1"}).to_string()),
            (200, json!({"nick":"Ada","unionId":"uAda","openId":"o1"}).to_string()),
        ]);
        let sess = Client::<Mock>::finish_auth_code(&app(), "auth-xyz", &mock, &path).unwrap();
        assert_eq!(sess.union_id, "uAda");
        assert_eq!(sess.nick.as_deref(), Some("Ada"));

        let mock2 = Mock::new(vec![
            (
                200,
                json!({"success":true,"result":{"data":[
                {"blockType":"heading","heading":{"level":"heading-1","text":"周报"},"id":"1","index":0},
                {"blockType":"paragraph","paragraph":{"text":"hello"},"id":"2","index":1}
            ]}})
                .to_string(),
            ),
            (200, json!({"success":true}).to_string()),
            (200, json!({"success":true}).to_string()),
            (200, json!({"docKey":"newDocKey99","url":"https://alidocs.dingtalk.com/i/nodes/newDocKey99"}).to_string()),
            (200, json!({"success":true}).to_string()),
        ]);
        let client = Client::with_transport(app(), sess, path.clone(), mock2);
        let pulled = client.pull_markdown("https://alidocs.dingtalk.com/i/nodes/AbCdEfGh12345678").unwrap();
        assert_eq!(pulled.title, "周报");
        client.push_overwrite(&pulled.doc_key, "# 新\n").unwrap();
        client.push_append(&pulled.doc_key, "追加").unwrap();
        let created = client.create_and_push("新建", "# hi\n").unwrap();
        assert_eq!(created.doc_key, "newDocKey99");
        let calls = client.http.calls.lock().unwrap();
        assert!(calls.iter().any(|c| c.1.contains("/v2.0/doc/me/suites/documents/") && c.1.contains("overwriteContent")));
        assert!(calls.iter().any(|c| c.1.contains("operatorId=uAda")));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn blocks_convert_to_markdown() {
        let blocks = vec![
            json!({"blockType":"heading","heading":{"level":1,"text":"标题"},"id":"a","index":0}),
            json!({"blockType":"todo","todo":{"done":true,"text":"完成"},"id":"c","index":2}),
        ];
        let md = blocks_to_markdown(&blocks);
        assert!(md.contains("# 标题"), "{md}");
        assert!(md.contains("- [x] 完成"), "{md}");
    }
}
