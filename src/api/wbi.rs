//! WBI 签名：B 站部分 Web 接口要求 w_rid / wts。

use anyhow::{Context, Result, bail};
use serde_json::Value;

/// WBI mixin key 的重排表（B 站前端固定常量）。
const MIXIN_KEY_ENC_TAB: [usize; 32] = [
    46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 36, 27, 43, 20, 41, 24, 49, 6, 26,
    40, 28, 30, 1, 21, 44, 17, 29,
];

#[derive(Debug, Clone)]
pub struct WbiKeys {
    mixin_key: String,
}

impl WbiKeys {
    pub async fn fetch(http: &reqwest::Client) -> Result<Self> {
        let value: Value = http
            .get("https://api.bilibili.com/x/web-interface/nav")
            .header(reqwest::header::REFERER, "https://www.bilibili.com/")
            .header(
                reqwest::header::USER_AGENT,
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
            )
            .send()
            .await
            .context("请求 WBI 密钥失败")?
            .json()
            .await
            .context("解析 WBI 密钥失败")?;

        let img = value["data"]["wbi_img"]["img_url"]
            .as_str()
            .context("nav 响应缺少 wbi_img")?;
        let sub = value["data"]["wbi_img"]["sub_url"]
            .as_str()
            .context("nav 响应缺少 sub_url")?;

        let img_key = file_stem(img)?;
        let sub_key = file_stem(sub)?;
        let raw = format!("{img_key}{sub_key}");
        if raw.len() < 64 {
            bail!("WBI 密钥长度异常: {}", raw.len());
        }
        let mixin_key = MIXIN_KEY_ENC_TAB
            .iter()
            .map(|&index| raw.as_bytes()[index] as char)
            .collect::<String>();
        Ok(Self { mixin_key })
    }

    /// 生成 `k=v&...&wts=...&w_rid=...`，参数会按 key 排序并过滤 `!'()*`。
    pub fn sign(&self, mut params: Vec<(String, String)>) -> String {
        params.sort_by(|a, b| a.0.cmp(&b.0));
        let query = params
            .iter()
            .map(|(key, value)| {
                let cleaned: String = value
                    .chars()
                    .filter(|c| !matches!(c, '!' | '\'' | '(' | ')' | '*'))
                    .collect();
                format!(
                    "{}={}",
                    url::form_urlencoded::byte_serialize(key.as_bytes()).collect::<String>(),
                    url::form_urlencoded::byte_serialize(cleaned.as_bytes()).collect::<String>()
                )
            })
            .collect::<Vec<_>>()
            .join("&");
        let sign_source = format!("{query}{}", self.mixin_key);
        let w_rid = format!("{:x}", md5::compute(sign_source));
        format!("{query}&w_rid={w_rid}")
    }
}

fn file_stem(url: &str) -> Result<&str> {
    url.rsplit('/')
        .next()
        .and_then(|name| name.split('.').next())
        .context("WBI 密钥 URL 格式异常")
}
