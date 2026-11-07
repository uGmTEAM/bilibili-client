//! 登录、登出、账号信息与大会员状态。

use crate::cli::{LoginArgs, VipAction};
use crate::config::AppConfig;
use crate::output::Output;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

use super::client::{ApiClient, build_http};

const QR_GENERATE: &str = "https://passport.bilibili.com/x/passport-login/web/qrcode/generate";
const QR_POLL: &str = "https://passport.bilibili.com/x/passport-login/web/qrcode/poll";
const COOKIE_REFRESH: &str = "https://passport.bilibili.com/x/passport-login/web/cookie/refresh";
const COOKIE_CONFIRM: &str = "https://passport.bilibili.com/x/passport-login/web/confirm/refresh";

/// 登录态里的长效凭证，用于免扫码续期。
#[derive(Debug, Default, Clone)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: u64,
}

pub async fn login(args: &LoginArgs, cfg: &AppConfig, out: Output) -> Result<()> {
    let save_path = args.save.clone().unwrap_or_else(|| cfg.cookie.clone());
    if args.refresh {
        return refresh_login(cfg, &save_path, out).await;
    }
    let (cookies, tokens) = if let Some(path) = &args.cookie_file {
        (import_cookie_file(path)?, Tokens::default())
    } else if let Some(sessdata) = &args.sessdata {
        let jct = args
            .bili_jct
            .as_deref()
            .context("使用 --sessdata 时必须同时提供 --bili-jct")?;
        let mut map = BTreeMap::new();
        map.insert("SESSDATA".to_string(), sessdata.clone());
        map.insert("bili_jct".to_string(), jct.to_string());
        if let Some(uid) = &args.dede_user_id {
            map.insert("DedeUserID".to_string(), uid.clone());
        }
        (map, Tokens::default())
    } else {
        web_qrcode_login(cfg, out).await?
    };

    let mid = cookies
        .get("DedeUserID")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    save_login(&save_path, &cookies, mid, &tokens)?;
    out.info(format!("登录信息已保存: {}", save_path.display()));
    out.emit(
        json!({
            "saved": save_path,
            "mid": mid,
            "cookies": cookies.keys().collect::<Vec<_>>(),
            "refreshable": !tokens.refresh_token.is_empty(),
        }),
        || {
            let hint = if tokens.refresh_token.is_empty() {
                "（未获取到 refresh_token，过期后需重新扫码）"
            } else {
                "（可用 `bilic login --refresh` 免扫码续期）"
            };
            format!("已保存 {} 项 Cookie，mid={mid}{hint}", cookies.len())
        },
    );
    Ok(())
}

/// 用保存的 refresh_token 续期，避免过期后重新扫码。
async fn refresh_login(cfg: &AppConfig, save_path: &Path, out: Output) -> Result<()> {
    let (cookies, tokens) = load_login_file(save_path)?;
    if tokens.refresh_token.is_empty() {
        bail!(
            "{} 里没有 refresh_token，无法续期；请重新运行 `bilic login` 扫码",
            save_path.display()
        );
    }
    let csrf = cookies
        .get("bili_jct")
        .cloned()
        .context("登录信息缺少 bili_jct，无法续期，请重新扫码")?;
    let http = build_http(cfg, &cfg.user_agent)?;
    let referer = "https://www.bilibili.com/";

    let response = http
        .post(COOKIE_REFRESH)
        .form(&[("csrf", csrf.as_str()), ("refresh_token", tokens.refresh_token.as_str())])
        .header(reqwest::header::REFERER, referer)
        .send()
        .await
        .context("请求续期接口失败")?;
    let mut updated = cookies.clone();
    merge_set_cookie(&mut updated, response.headers());
    let body: Value = response.json().await.context("解析续期结果失败")?;
    if body.get("code").and_then(Value::as_i64).unwrap_or(-1) != 0 {
        let message = body.get("message").and_then(Value::as_str).unwrap_or("unknown");
        bail!("续期失败: {message}（refresh_token 可能已失效，请重新扫码）");
    }
    let data = body.get("data").cloned().unwrap_or(Value::Null);
    let refreshed = Tokens {
        access_token: data
            .get("access_token")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        refresh_token: data
            .get("refresh_token")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| tokens.refresh_token.clone()),
        expires_in: data.get("expires_in").and_then(Value::as_u64).unwrap_or(0),
    };
    if !updated.contains_key("SESSDATA") {
        bail!("续期响应里没有新的 SESSDATA，请重新扫码");
    }
    let new_csrf = updated
        .get("bili_jct")
        .cloned()
        .unwrap_or_else(|| csrf.clone());
    // 确认续期，使旧 refresh_token 失效
    let _ = http
        .post(COOKIE_CONFIRM)
        .form(&[
            ("csrf", new_csrf.as_str()),
            ("refresh_token", refreshed.refresh_token.as_str()),
        ])
        .header(reqwest::header::REFERER, referer)
        .send()
        .await;

    let mid = updated
        .get("DedeUserID")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    save_login(save_path, &updated, mid, &refreshed)?;
    out.emit(
        json!({"refreshed": true, "saved": save_path, "mid": mid}),
        || format!("登录态已续期并保存: {}", save_path.display()),
    );
    Ok(())
}

/// 从登录文件里读出 cookies 与 token（兼容 biliup LoginInfo 与裸 map）。
fn load_login_file(path: &Path) -> Result<(BTreeMap<String, String>, Tokens)> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("读取登录信息失败: {}（请先运行 `bilic login`）", path.display()))?;
    let value: Value = serde_json::from_str(&text).context("登录信息不是合法 JSON")?;
    if let Some(entries) = value
        .pointer("/cookie_info/cookies")
        .and_then(Value::as_array)
        .or_else(|| value.get("cookies").and_then(Value::as_array))
    {
        let mut cookies = BTreeMap::new();
        for item in entries {
            if let (Some(name), Some(cookie)) = (
                item.get("name").and_then(Value::as_str),
                item.get("value").and_then(Value::as_str),
            ) {
                cookies.insert(name.to_string(), cookie.to_string());
            }
        }
        let token = value.get("token_info").cloned().unwrap_or(Value::Null);
        let tokens = Tokens {
            access_token: token
                .get("access_token")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            refresh_token: token
                .get("refresh_token")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            expires_in: token.get("expires_in").and_then(Value::as_u64).unwrap_or(0),
        };
        return Ok((cookies, tokens));
    }
    let cookies: BTreeMap<String, String> =
        serde_json::from_value(value).context("无法识别登录信息格式")?;
    Ok((cookies, Tokens::default()))
}

/// 把响应 Set-Cookie 合并进 cookie 表，跳过被删除的项。
fn merge_set_cookie(cookies: &mut BTreeMap<String, String>, headers: &reqwest::header::HeaderMap) {
    for value in headers.get_all(reqwest::header::SET_COOKIE) {
        let Ok(text) = value.to_str() else { continue };
        let Some((name, rest)) = text.split_once('=') else {
            continue;
        };
        let cookie = rest.split(';').next().unwrap_or("").trim();
        let expired = text.to_ascii_lowercase().contains("max-age=0");
        if cookie.is_empty() || cookie == "deleted" || expired {
            cookies.remove(name.trim());
        } else {
            cookies.insert(name.trim().to_string(), cookie.to_string());
        }
    }
}

pub async fn logout(cfg: &AppConfig, out: Output) -> Result<()> {
    if cfg.cookie.exists() {
        std::fs::remove_file(&cfg.cookie)
            .with_context(|| format!("删除登录信息失败: {}", cfg.cookie.display()))?;
        out.emit(json!({"removed": cfg.cookie.clone()}), || {
            format!("已删除 {}", cfg.cookie.display())
        });
    } else {
        out.emit(json!({"removed": null}), || "没有本地登录信息".to_string());
    }
    Ok(())
}

pub async fn whoami(cfg: &AppConfig, out: Output) -> Result<()> {
    let client = ApiClient::load(cfg).await?;
    let nav = client
        .get_value("https://api.bilibili.com/x/web-interface/nav", &[])
        .await?;
    let is_login = nav.get("isLogin").and_then(Value::as_bool).unwrap_or(false);
    if !is_login {
        bail!("未登录或登录已失效，请运行 `bilic login` 扫码，或用 `bilic login --refresh` 免扫码续期");
    }
    let result = json!({
        "mid": nav.get("mid"),
        "name": nav.get("uname"),
        "level": nav.get("level_info"),
        "coins": nav.get("money"),
        "vip": {
            "status": nav.get("vipStatus"),
            "type": nav.get("vipType"),
            "due_date": nav.get("vipDueDate"),
            "label": nav.pointer("/vip_label/text"),
        },
        "face": nav.get("face"),
        "sign": nav.get("sign"),
    });
    out.emit(result.clone(), || {
        let name = nav.get("uname").and_then(Value::as_str).unwrap_or("-");
        let mid = nav.get("mid").and_then(Value::as_u64).unwrap_or(0);
        let level = nav
            .pointer("/level_info/current_level")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let vip = if nav.get("vipStatus").and_then(Value::as_u64).unwrap_or(0) == 1 {
            "大会员"
        } else {
            "普通用户"
        };
        format!("{name} (mid={mid})  Lv{level}  {vip}")
    });
    Ok(())
}

pub async fn vip(action: VipAction, cfg: &AppConfig, out: Output) -> Result<()> {
    match action {
        VipAction::Status => {
            let client = ApiClient::load(cfg).await?;
            let nav = client
                .get_value("https://api.bilibili.com/x/web-interface/nav", &[])
                .await?;
            let status = nav.get("vipStatus").and_then(Value::as_u64).unwrap_or(0) == 1;
            let due = nav.get("vipDueDate").and_then(Value::as_i64).unwrap_or(0);
            let due_text = chrono::DateTime::from_timestamp_millis(due)
                .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string());
            let result = json!({
                "is_vip": status,
                "vip_type": nav.get("vipType"),
                "due_date_ms": due,
                "due_date": due_text.clone(),
                "label": nav.pointer("/vip_label/text"),
            });
            out.emit(result, || {
                if status {
                    format!(
                        "大会员有效，到期时间：{}",
                        due_text.unwrap_or_else(|| "-".into())
                    )
                } else {
                    "当前不是大会员".to_string()
                }
            });
            Ok(())
        }
        VipAction::Open => {
            let url = "https://big.bilibili.com/mobile/big-vip";
            open_browser(url);
            out.emit(json!({"url": url}), || {
                format!("已在浏览器打开官方购买页：{url}\n（需自行完成支付，本工具不处理支付）")
            });
            Ok(())
        }
    }
}

async fn web_qrcode_login(
    cfg: &AppConfig,
    out: Output,
) -> Result<(BTreeMap<String, String>, Tokens)> {
    let http = build_http(cfg, &cfg.user_agent)?;
    let generated: Value = http
        .get(QR_GENERATE)
        .query(&[("source", "main-fe-header")])
        .header(reqwest::header::REFERER, "https://www.bilibili.com/")
        .send()
        .await
        .context("获取登录二维码失败")?
        .json()
        .await?;
    let data = generated
        .get("data")
        .context("二维码响应缺少 data")?;
    let url = data.get("url").and_then(Value::as_str).context("缺少二维码 URL")?;
    let key = data
        .get("qrcode_key")
        .and_then(Value::as_str)
        .context("缺少 qrcode_key")?;

    print_qr(url)?;
    out.info("请使用 B 站手机客户端扫码登录（Ctrl+C 取消）");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    let mut scanned = false;
    loop {
        if std::time::Instant::now() > deadline {
            bail!("二维码已超时，请重新执行 `bilic login`");
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let response = http
            .get(QR_POLL)
            .query(&[("qrcode_key", key)])
            .header(reqwest::header::REFERER, "https://www.bilibili.com/")
            .send()
            .await
            .context("轮询二维码状态失败")?;

        let mut cookies = BTreeMap::new();
        for value in response.headers().get_all(reqwest::header::SET_COOKIE) {
            if let Ok(text) = value.to_str() {
                if let Some((name, value)) = text.split_once('=') {
                    let value = value.split(';').next().unwrap_or("").trim();
                    if !value.is_empty() && value != "deleted" {
                        cookies.insert(name.trim().to_string(), value.to_string());
                    }
                }
            }
        }

        let body: Value = response.json().await.context("解析二维码轮询结果失败")?;
        let code = body
            .pointer("/data/code")
            .and_then(Value::as_i64)
            .unwrap_or(-1);
        match code {
            0 => {
                if !cookies.contains_key("SESSDATA") {
                    bail!("登录成功但未拿到 SESSDATA，请改用 --sessdata/--bili-jct 导入");
                }
                let data = body.get("data").cloned().unwrap_or(Value::Null);
                let tokens = Tokens {
                    access_token: data
                        .get("access_token")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    refresh_token: data
                        .get("refresh_token")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    expires_in: data.get("expires_in").and_then(Value::as_u64).unwrap_or(0),
                };
                out.info("扫码登录成功");
                return Ok((cookies, tokens));
            }
            86101 => {}
            86090 => {
                if !scanned {
                    out.info("已扫码，请在手机上确认登录");
                    scanned = true;
                }
            }
            86038 => bail!("二维码已失效，请重新执行 `bilic login`"),
            other => {
                let message = body
                    .pointer("/data/message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                bail!("二维码登录失败: code={other}, {message}");
            }
        }
    }
}

fn print_qr(url: &str) -> Result<()> {
    let code = qrcode::QrCode::new(url.as_bytes()).context("生成二维码失败")?;
    let image = code
        .render::<qrcode::render::unicode::Dense1x2>()
        .dark_color(qrcode::render::unicode::Dense1x2::Light)
        .light_color(qrcode::render::unicode::Dense1x2::Dark)
        .build();
    println!("{image}");
    println!("如果二维码显示不完整，可复制此链接到浏览器打开：\n{url}\n");
    Ok(())
}

fn import_cookie_file(path: &Path) -> Result<BTreeMap<String, String>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("读取 Cookie 文件失败: {}", path.display()))?;
    if let Ok(map) = serde_json::from_str::<BTreeMap<String, String>>(&text) {
        return Ok(map);
    }
    if let Some(map) = super::client::parse_netscape_cookies(&text) {
        return Ok(map);
    }
    // biliup 的 cookies.json（LoginInfo）
    let info: biliup::uploader::credential::LoginInfo =
        serde_json::from_str(&text).context("无法识别 Cookie 文件格式")?;
    Ok(super::client::cookie_map_from_login(&info))
}

pub fn save_login(
    path: &Path,
    cookies: &BTreeMap<String, String>,
    mid: u64,
    tokens: &Tokens,
) -> Result<()> {
    let cookies: Vec<Value> = cookies
        .iter()
        .map(|(name, value)| json!({"name": name, "value": value}))
        .collect();
    let info = json!({
        "cookie_info": { "cookies": cookies },
        "sso": [],
        "token_info": {
            "access_token": tokens.access_token,
            "expires_in": tokens.expires_in,
            "mid": mid,
            "refresh_token": tokens.refresh_token
        },
        "platform": "Web"
    });
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, serde_json::to_vec_pretty(&info)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600));
    }
    if path.exists() {
        let _ = std::fs::remove_file(path);
    }
    std::fs::rename(&temporary, path)?;
    Ok(())
}

fn open_browser(url: &str) {
    #[cfg(target_os = "linux")]
    let candidates = ["xdg-open", "gio"];
    #[cfg(target_os = "macos")]
    let candidates = ["open"];
    #[cfg(windows)]
    let candidates = ["explorer"];

    for command in candidates {
        if std::process::Command::new(command)
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .is_ok()
        {
            return;
        }
    }
    eprintln!("无法自动打开浏览器，请手动访问: {url}");
}

#[cfg(test)]
mod tests {
    use super::{Tokens, load_login_file, merge_set_cookie};
    use std::collections::BTreeMap;

    #[test]
    fn loads_login_info_with_refresh_token() {
        let dir = std::env::temp_dir().join("bilic-account-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cookies.json");
        let raw = r#"{
            "cookie_info": {"cookies": [
                {"name": "SESSDATA", "value": "abc"},
                {"name": "bili_jct", "value": "def"},
                {"name": "DedeUserID", "value": "123"}
            ]},
            "token_info": {"access_token": "a", "refresh_token": "r", "expires_in": 3600, "mid": 123},
            "platform": "Web"
        }"#;
        std::fs::write(&path, raw).unwrap();
        let (cookies, tokens) = load_login_file(&path).unwrap();
        assert_eq!(cookies.get("SESSDATA").map(String::as_str), Some("abc"));
        assert_eq!(tokens.refresh_token, "r");
        assert_eq!(tokens.expires_in, 3600);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn merges_set_cookie_and_drops_deleted_entries() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.append(
            reqwest::header::SET_COOKIE,
            "SESSDATA=new; Path=/; HttpOnly".parse().unwrap(),
        );
        headers.append(
            reqwest::header::SET_COOKIE,
            "bili_jct=; Max-Age=0; Path=/".parse().unwrap(),
        );
        let mut cookies = BTreeMap::new();
        cookies.insert("SESSDATA".to_string(), "old".to_string());
        cookies.insert("bili_jct".to_string(), "old".to_string());
        merge_set_cookie(&mut cookies, &headers);
        assert_eq!(cookies.get("SESSDATA").map(String::as_str), Some("new"));
        assert!(!cookies.contains_key("bili_jct"));
    }

    #[test]
    fn saves_and_reads_tokens_roundtrip() {
        let dir = std::env::temp_dir().join("bilic-account-roundtrip");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cookies.json");
        let mut cookies = BTreeMap::new();
        cookies.insert("SESSDATA".to_string(), "s".to_string());
        cookies.insert("DedeUserID".to_string(), "9".to_string());
        let tokens = Tokens {
            access_token: "a".to_string(),
            refresh_token: "r".to_string(),
            expires_in: 7200,
        };
        super::save_login(&path, &cookies, 9, &tokens).unwrap();
        let (read_cookies, read_tokens) = load_login_file(&path).unwrap();
        assert_eq!(read_cookies.get("SESSDATA").map(String::as_str), Some("s"));
        assert_eq!(read_tokens.refresh_token, "r");
        let _ = std::fs::remove_file(&path);
    }
}
