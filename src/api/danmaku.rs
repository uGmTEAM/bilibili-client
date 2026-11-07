//! 视频弹幕：发送（时间点 / 墙钟定时）与撤回。

use crate::cli::{DanmakuAction, DanmakuSendArgs};
use crate::config::AppConfig;
use crate::output::Output;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::ApiClient;
use super::video::{self, MediaId};

pub async fn run(action: DanmakuAction, cfg: &AppConfig, out: Output) -> Result<()> {
    match action {
        DanmakuAction::Send(args) => send(args, cfg, out).await,
        DanmakuAction::Recall { dmid, id, page } => recall(&id, page, dmid, cfg, out).await,
    }
}

async fn send(args: DanmakuSendArgs, cfg: &AppConfig, out: Output) -> Result<()> {
    let client = ApiClient::load(cfg).await?;
    let media = video::resolve_media(&client, &args.id).await?;
    let MediaId::Video(_) = &media else {
        bail!("视频弹幕目前只支持普通视频");
    };
    let view = video::view(&client, &media).await?;
    let page = view
        .pages
        .iter()
        .find(|page| page.page == args.page as i32)
        .with_context(|| format!("找不到第 {} P", args.page))?;

    let progress_ms = parse_progress_ms(&args.at)?;
    let color = parse_color(&args.color)?;
    let mode = args.mode.value();

    if let Some(schedule) = &args.schedule {
        let target = parse_schedule(schedule)?;
        let now = chrono::Local::now();
        if target > now {
            let wait = (target - now).to_std().unwrap_or_default();
            out.info(format!(
                "定时发送：{}（等待 {}）",
                target.format("%Y-%m-%d %H:%M:%S"),
                humantime::format_duration(wait)
            ));
            tokio::time::sleep(wait).await;
        }
    }

    let form = vec![
        ("type".to_string(), "1".to_string()),
        ("oid".to_string(), page.cid.to_string()),
        ("bvid".to_string(), view.bvid.clone()),
        ("msg".to_string(), args.message.clone()),
        ("progress".to_string(), progress_ms.to_string()),
        ("color".to_string(), color.to_string()),
        ("fontsize".to_string(), args.fontsize.to_string()),
        ("pool".to_string(), args.pool.to_string()),
        ("mode".to_string(), mode.to_string()),
        ("plat".to_string(), "1".to_string()),
        (
            "csrf".to_string(),
            client
                .csrf()
                .map(str::to_string)
                .unwrap_or_else(|_| "0".to_string()),
        ),
    ];

    if args.dry_run {
        out.emit(
            json!({"dry_run": true, "form": &form, "cid": page.cid}),
            || {
                form.iter()
                    .map(|(key, value)| format!("{key}={value}"))
                    .collect::<Vec<_>>()
                    .join("&")
            },
        );
        return Ok(());
    }

    client.require_login()?;
    let data = client
        .post_form("https://api.bilibili.com/x/v2/dm/post", &form)
        .await?;
    let dmid = data
        .get("dmid")
        .or_else(|| data.get("dmid_str"))
        .cloned()
        .unwrap_or(Value::Null);
    out.emit(
        json!({"sent": true, "cid": page.cid, "progress_ms": progress_ms, "dmid": dmid, "data": data}),
        || {
            format!(
                "弹幕已发送（cid={}，时间点 {}，颜色 #{:06X}）",
                page.cid,
                format_progress(progress_ms),
                color
            )
        },
    );
    Ok(())
}

async fn recall(id: &str, page: u32, dmid: u64, cfg: &AppConfig, out: Output) -> Result<()> {
    let client = ApiClient::load(cfg).await?;
    client.require_login()?;
    let media = video::resolve_media(&client, id).await?;
    let MediaId::Video(_) = &media else {
        bail!("视频弹幕目前只支持普通视频");
    };
    let view = video::view(&client, &media).await?;
    let page = view
        .pages
        .iter()
        .find(|item| item.page == page as i32)
        .with_context(|| format!("找不到第 {page} P"))?;
    let form = vec![
        ("dmid".to_string(), dmid.to_string()),
        ("oid".to_string(), page.cid.to_string()),
        ("type".to_string(), "1".to_string()),
        ("csrf".to_string(), client.csrf()?.to_string()),
    ];
    client
        .post_form("https://api.bilibili.com/x/v2/dm/retract", &form)
        .await?;
    out.emit(json!({"recalled": dmid}), || format!("已撤回弹幕 {dmid}"));
    Ok(())
}

/// 支持 `90`、`90s`、`1m30s`、`00:01:30`、`1:30`。
pub fn parse_progress_ms(text: &str) -> Result<u64> {
    let text = text.trim();
    if text.contains(':') {
        let parts: Vec<&str> = text.split(':').collect();
        let mut seconds = 0f64;
        for (index, part) in parts.iter().rev().enumerate() {
            let value: f64 = part
                .parse()
                .with_context(|| format!("无法解析时间: {text}"))?;
            seconds += value * 60f64.powi(index as i32);
        }
        return Ok((seconds * 1000.0).round() as u64);
    }
    if let Ok(seconds) = text.parse::<f64>() {
        return Ok((seconds * 1000.0).round() as u64);
    }
    let duration = humantime::parse_duration(text)
        .with_context(|| format!("无法解析时间: {text}（支持 90、90s、1m30s、00:01:30）"))?;
    Ok(duration.as_millis() as u64)
}

pub fn parse_color(text: &str) -> Result<u32> {
    let text = text.trim();
    let hex = text.strip_prefix('#').unwrap_or(text);
    let hex = hex.strip_prefix("0x").unwrap_or(hex);
    if let Ok(value) = text.parse::<u32>() {
        return Ok(value.min(0xFFFFFF));
    }
    u32::from_str_radix(hex, 16).with_context(|| format!("无法解析颜色: {text}（支持 #RRGGBB 或十进制）"))
}

fn parse_schedule(text: &str) -> Result<chrono::DateTime<chrono::Local>> {
    use chrono::TimeZone;
    if let Ok(time) = chrono::DateTime::parse_from_rfc3339(text) {
        return Ok(time.with_timezone(&chrono::Local));
    }
    let now = chrono::Local::now();
    for format in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"] {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(text, format) {
            return chrono::Local
                .from_local_datetime(&naive)
                .single()
                .context("本地时间不唯一");
        }
    }
    if let Ok(naive) = chrono::NaiveTime::parse_from_str(text, "%H:%M:%S")
        .or_else(|_| chrono::NaiveTime::parse_from_str(text, "%H:%M"))
    {
        let mut candidate = now
            .date_naive()
            .and_time(naive)
            .and_local_timezone(chrono::Local)
            .single()
            .context("本地时间不唯一")?;
        if candidate <= now {
            candidate += chrono::Duration::days(1);
        }
        return Ok(candidate);
    }
    bail!("无法解析定时时间: {text}（支持 RFC3339、\"YYYY-MM-DD HH:MM[:SS]\" 或 \"HH:MM\"）")
}

fn format_progress(ms: u64) -> String {
    let seconds = ms / 1000;
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let secs = seconds % 60;
    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes:02}:{secs:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_color, parse_progress_ms};

    #[test]
    fn parses_progress_formats() {
        assert_eq!(parse_progress_ms("90").unwrap(), 90_000);
        assert_eq!(parse_progress_ms("90s").unwrap(), 90_000);
        assert_eq!(parse_progress_ms("1m30s").unwrap(), 90_000);
        assert_eq!(parse_progress_ms("00:01:30").unwrap(), 90_000);
        assert_eq!(parse_progress_ms("1:30").unwrap(), 90_000);
        assert!(parse_progress_ms("not-a-time").is_err());
    }

    #[test]
    fn parses_colors() {
        assert_eq!(parse_color("#FF0000").unwrap(), 0xFF0000);
        assert_eq!(parse_color("00ff00").unwrap(), 0x00FF00);
        assert_eq!(parse_color("16777215").unwrap(), 0xFFFFFF);
        assert!(parse_color("zzz").is_err());
    }
}
