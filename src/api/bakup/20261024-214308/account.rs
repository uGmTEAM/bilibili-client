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

pub async fn login(args: &LoginArgs, cfg: &AppConfig, out: Output) -> Result<()> {
    let save_path = args.save.clone().unwrap_or_else(|| cfg.cookie.clone());
    let cookies = if let Some(path) = &args.cookie_file {
        import_cookie_file(path)?
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
        map
    } else {
        web_qrcode_login(cfg, out).await?
    };

    let mid = cookies
        .get("DedeUserID")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    save_login(&save_path, &cookies, mid)?;
    out.info(format!("登录信息已保存: {}", save_path.display()));
    out.emit(
        json!({"saved": save_path, "mid": mid, "cookies": cookies.keys().collect::<Vec<_>>()}),
        || format!("已保存 {} 项 Cookie，mid={mid}", cookies.len()),
    );
    Ok(())
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
        bail!("未登录或登录已失效，请运行 `bilic login`");
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

async fn web_qrcode_login(cfg: &AppConfig, out: Output) -> Result<BTreeMap<String, String>> {
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
                out.info("扫码登录成功");
                return Ok(cookies);
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

pub fn save_login(path: &Path, cookies: &BTreeMap<String, String>, mid: u64) -> Result<()> {
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
