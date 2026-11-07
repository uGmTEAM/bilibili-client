//! B 站 API 客户端：Cookie、CSRF、WBI 签名与统一响应处理。

use crate::config::AppConfig;
use anyhow::{Context, Result, bail};
use biliup::uploader::bilibili::BiliBili;
use biliup::uploader::credential::{LoginInfo, bilibili_from_info};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::OnceCell;

use super::wbi::WbiKeys;

#[derive(Clone)]
pub struct ApiClient {
    pub http: reqwest::Client,
    pub cookies: BTreeMap<String, String>,
    pub cookie_header: String,
    pub csrf: Option<String>,
    pub mid: Option<u64>,
    /// 上传/合集等复用 biliup 的登录客户端；匿名或 Cookie 不完整时为 None。
    pub bili: Option<Arc<BiliBili>>,
    pub proxy: Option<String>,
    pub user_agent: String,
    wbi: OnceCell<WbiKeys>,
}

impl ApiClient {
    pub async fn load(cfg: &AppConfig) -> Result<Self> {
        let mut cookies = BTreeMap::new();
        let mut bili = None;

        if let Some(path) = existing_cookie_path(&cfg.cookie) {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("读取登录信息失败: {}", path.display()))?;
            if let Ok(info) = serde_json::from_str::<LoginInfo>(&text) {
                cookies = cookie_map_from_login(&info);
                if let Ok(client) = bilibili_from_info(info, cfg.proxy.as_deref()) {
                    bili = Some(Arc::new(client));
                }
            } else if let Ok(map) = serde_json::from_str::<BTreeMap<String, String>>(&text) {
                cookies = map;
            } else if let Some(parsed) = parse_netscape_cookies(&text) {
                cookies = parsed;
            } else {
                bail!(
                    "无法识别登录信息格式: {}（支持 biliup cookies.json、{{SESSDATA,bili_jct}} JSON 或 Netscape cookies.txt）",
                    path.display()
                );
            }
        }

        let cookie_header = cookies
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ");
        let csrf = cookies.get("bili_jct").cloned();
        let mid = cookies
            .get("DedeUserID")
            .and_then(|value| value.parse::<u64>().ok());

        // 简单 Cookie 导入（只有 SESSDATA/bili_jct）也要能投稿和管合集。
        if bili.is_none() && cookies.contains_key("SESSDATA") {
            if let Ok(info) = login_info_from_cookies(&cookies) {
                if let Ok(client) = bilibili_from_info(info, cfg.proxy.as_deref()) {
                    bili = Some(Arc::new(client));
                }
            }
        }

        let http = build_http(cfg, &cfg.user_agent)?;

        Ok(Self {
            http,
            cookies,
            cookie_header,
            csrf,
            mid,
            bili,
            proxy: cfg.proxy.clone(),
            user_agent: cfg.user_agent.clone(),
            wbi: OnceCell::new(),
        })
    }

    pub fn is_login(&self) -> bool {
        self.cookies.contains_key("SESSDATA")
    }

    pub fn require_login(&self) -> Result<()> {
        if !self.is_login() {
            bail!("需要登录：先运行 `bilic login`");
        }
        Ok(())
    }

    pub fn csrf(&self) -> Result<&str> {
        self.csrf
            .as_deref()
            .context("登录信息缺少 bili_jct，无法执行写操作；请重新登录")
    }

    pub fn require_mid(&self) -> Result<u64> {
        self.mid.context("登录信息缺少 DedeUserID，请重新登录")
    }

    fn get(&self, url: &str) -> reqwest::RequestBuilder {
        let mut request = self
            .http
            .get(url)
            .header(reqwest::header::REFERER, "https://www.bilibili.com/")
            .header(reqwest::header::ACCEPT, "application/json, text/plain, */*");
        if !self.cookie_header.is_empty() {
            request = request.header(reqwest::header::COOKIE, &self.cookie_header);
        }
        request
    }

    fn post(&self, url: &str) -> reqwest::RequestBuilder {
        let mut request = self
            .http
            .post(url)
            .header(reqwest::header::REFERER, "https://www.bilibili.com/")
            .header(reqwest::header::ACCEPT, "application/json, text/plain, */*");
        if !self.cookie_header.is_empty() {
            request = request.header(reqwest::header::COOKIE, &self.cookie_header);
        }
        request
    }

    pub async fn get_value(&self, url: &str, query: &[(&str, String)]) -> Result<Value> {
        let response = self.get(url).query(query).send().await?;
        parse_envelope(response).await
    }

    /// 返回完整响应体（兼容使用 `result` 而不是 `data` 的 PGC 接口）。
    pub async fn get_envelope(&self, url: &str, query: &[(&str, String)]) -> Result<Value> {
        let response = self.get(url).query(query).send().await?;
        parse_value(response).await
    }

    /// 下载媒体文件用的客户端：不设整体超时，只设连接超时。
    pub fn media_client(&self) -> Result<reqwest::Client> {
        let mut builder = reqwest::Client::builder()
            .user_agent(&self.user_agent)
            .connect_timeout(std::time::Duration::from_secs(15));
        if let Some(proxy) = self.proxy.as_deref() {
            builder = builder.proxy(
                reqwest::Proxy::all(proxy).with_context(|| format!("无效的代理地址: {proxy}"))?,
            );
        }
        Ok(builder.build()?)
    }

    pub fn media_headers(&self) -> Vec<(reqwest::header::HeaderName, String)> {
        let mut headers = vec![
            (
                reqwest::header::REFERER,
                "https://www.bilibili.com/".to_string(),
            ),
            (
                reqwest::header::USER_AGENT,
                self.user_agent.clone(),
            ),
        ];
        if !self.cookie_header.is_empty() {
            headers.push((reqwest::header::COOKIE, self.cookie_header.clone()));
        }
        headers
    }

    pub async fn get_value_signed(&self, url: &str, params: Vec<(String, String)>) -> Result<Value> {
        let query = self.sign(params).await?;
        let separator = if url.contains('?') { '&' } else { '?' };
        let response = self.get(&format!("{url}{separator}{query}")).send().await?;
        parse_envelope(response).await
    }

    pub async fn get_envelope_signed(
        &self,
        url: &str,
        params: Vec<(String, String)>,
    ) -> Result<Value> {
        let query = self.sign(params).await?;
        let separator = if url.contains('?') { '&' } else { '?' };
        let response = self.get(&format!("{url}{separator}{query}")).send().await?;
        parse_value(response).await
    }

    pub async fn post_form(&self, url: &str, form: &[(String, String)]) -> Result<Value> {
        let response = self.post(url).form(form).send().await?;
        parse_envelope(response).await
    }

    /// 带 Cookie 的原始 GET（用于弹幕 XML、媒体文件等非 JSON 响应）。
    pub async fn get_raw(&self, url: &str) -> Result<reqwest::Response> {
        let response = self.get(url).send().await?;
        if !response.status().is_success() {
            bail!("HTTP {}: {url}", response.status());
        }
        Ok(response)
    }

    pub async fn sign(&self, mut params: Vec<(String, String)>) -> Result<String> {
        let keys = self
            .wbi
            .get_or_try_init(|| async { WbiKeys::fetch(&self.http).await })
            .await?;
        params.push((
            "wts".to_string(),
            chrono::Utc::now().timestamp().to_string(),
        ));
        Ok(keys.sign(params))
    }

    /// 用 biliup 的客户端发请求；没有登录客户端时报错。
    pub fn bili(&self) -> Result<&BiliBili> {
        self.bili
            .as_deref()
            .context("需要登录后才能使用该功能：先运行 `bilic login`")
    }
}

pub(crate) fn build_http(cfg: &AppConfig, user_agent: &str) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .user_agent(user_agent)
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(60));
    if let Some(proxy) = cfg.proxy.as_deref() {
        builder = builder.proxy(
            reqwest::Proxy::all(proxy).with_context(|| format!("无效的代理地址: {proxy}"))?,
        );
    }
    Ok(builder.build()?)
}

fn existing_cookie_path(path: &Path) -> Option<std::path::PathBuf> {
    path.exists().then(|| path.to_path_buf())
}

fn login_info_from_cookies(cookies: &BTreeMap<String, String>) -> Result<LoginInfo> {
    let mid = cookies
        .get("DedeUserID")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let cookies: Vec<Value> = cookies
        .iter()
        .map(|(name, value)| json!({"name": name, "value": value}))
        .collect();
    let info = json!({
        "cookie_info": { "cookies": cookies },
        "sso": [],
        "token_info": {
            "access_token": "",
            "expires_in": 0,
            "mid": mid,
            "refresh_token": ""
        },
        "platform": "Web"
    });
    Ok(serde_json::from_value(info)?)
}

pub fn cookie_map_from_login(info: &LoginInfo) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    if let Some(cookies) = info.cookie_info.get("cookies").and_then(Value::as_array) {
        for cookie in cookies {
            if let (Some(name), Some(value)) = (
                cookie.get("name").and_then(Value::as_str),
                cookie.get("value").and_then(Value::as_str),
            ) {
                map.insert(name.to_string(), value.to_string());
            }
        }
    }
    map
}

/// 解析 Netscape cookies.txt（浏览器扩展导出格式）。
pub fn parse_netscape_cookies(text: &str) -> Option<BTreeMap<String, String>> {
    let mut map = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() >= 7 && fields[0].contains("bilibili.com") {
            map.insert(fields[5].to_string(), fields[6].to_string());
        }
    }
    (!map.is_empty()).then_some(map)
}

async fn parse_envelope(response: reqwest::Response) -> Result<Value> {
    let value = parse_value(response).await?;
    Ok(value.get("data").cloned().unwrap_or(Value::Null))
}

async fn parse_value(response: reqwest::Response) -> Result<Value> {
    let status = response.status();
    let text = response.text().await?;
    if !status.is_success() {
        bail!("HTTP {status}: {}", truncate(&text));
    }
    let value: Value = serde_json::from_str(&text)
        .with_context(|| format!("响应不是 JSON（HTTP {status}）: {}", truncate(&text)))?;
    let code = value.get("code").and_then(Value::as_i64).unwrap_or(0);
    if code != 0 {
        let message = value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        bail!("B站接口错误 {code}: {message}");
    }
    Ok(value)
}

fn truncate(text: &str) -> String {
    let mut out: String = text.chars().take(300).collect();
    if text.chars().count() > 300 {
        out.push('…');
    }
    out
}
