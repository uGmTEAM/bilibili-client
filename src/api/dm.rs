//! 私信：会话列表、历史消息、发送、回复、自动回复与自动消息识别。

use crate::cli::{MessageAction, MessageWatchArgs};
use crate::config::AppConfig;
use crate::output::Output;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;

use super::ApiClient;

const VC: &str = "https://api.vc.bilibili.com";

pub async fn run(action: MessageAction, cfg: &AppConfig, out: Output) -> Result<()> {
    let client = ApiClient::load(cfg).await?;
    client.require_login()?;
    match action {
        MessageAction::List => list(&client, out).await,
        MessageAction::History { mid, count } => history(&client, mid, count, out).await,
        MessageAction::Send { mid, message } => send(&client, mid, &message, out).await,
        MessageAction::Reply {
            mid,
            message,
            force,
        } => reply(&client, mid, &message, force, out).await,
        MessageAction::AutoReply {
            mid,
            text,
            template,
            dry_run,
            force,
        } => auto_reply(&client, mid, text, template, dry_run, force, cfg, out).await,
        MessageAction::Watch(args) => watch(&client, args, cfg, out).await,
        MessageAction::Read { mid } => read(&client, mid, out).await,
        MessageAction::Unread => unread(&client, out).await,
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Session {
    #[serde(default, deserialize_with = "de_u64")]
    pub talker_id: u64,
    #[serde(
        default,
        alias = "unread_count",
        alias = "unread",
        deserialize_with = "de_u64"
    )]
    pub unread_num: u64,
    #[serde(default, deserialize_with = "de_u64")]
    pub system_msg_type: u64,
    #[serde(default, deserialize_with = "de_u64")]
    pub session_ts: u64,
    #[serde(default)]
    pub last_msg: Option<Value>,
    #[serde(default, deserialize_with = "de_u64")]
    pub is_follow: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Message {
    #[serde(default, deserialize_with = "de_u64")]
    pub sender_uid: u64,
    #[serde(default, deserialize_with = "de_u64")]
    pub receiver_id: u64,
    #[serde(default, deserialize_with = "de_u64")]
    pub msg_type: u64,
    #[serde(default)]
    pub content: String,
    #[serde(default, deserialize_with = "de_u64")]
    pub timestamp: u64,
    #[serde(default, deserialize_with = "de_u64")]
    pub msg_seqno: u64,
    #[serde(default, deserialize_with = "de_u64")]
    pub msg_key: u64,
    #[serde(default, deserialize_with = "de_u64")]
    pub msg_status: u64,
    #[serde(default, deserialize_with = "de_u64")]
    pub sys_msg_type: u64,
}

impl Message {
    pub fn text(&self) -> String {
        extract_text(&self.content)
    }

    pub fn key(&self) -> String {
        if self.msg_key != 0 {
            self.msg_key.to_string()
        } else {
            format!("{}:{}", self.timestamp, self.msg_seqno)
        }
    }
}

async fn list(client: &ApiClient, out: Output) -> Result<()> {
    let sessions = fetch_sessions(client).await?;
    let mut rows = Vec::new();
    for session in &sessions {
        let name = user_name(client, session.talker_id).await.unwrap_or_default();
        let last = last_message_text(session);
        rows.push(json!({
            "talker_id": session.talker_id,
            "name": name,
            "unread": session.unread_num,
            "system": session.system_msg_type != 0,
            "is_follow": session.is_follow,
            "last_message": last,
            "session_ts": session.session_ts,
        }));
    }
    out.emit(json!({"sessions": &rows}), || {
        let mut text = String::new();
        for row in &rows {
            text.push_str(&format!(
                "{}  {}  未读 {}  {}  {}\n",
                row["talker_id"].as_u64().unwrap_or(0),
                row["name"].as_str().unwrap_or("-"),
                row["unread"].as_u64().unwrap_or(0),
                if row["system"].as_bool().unwrap_or(false) {
                    "[系统]"
                } else {
                    ""
                },
                row["last_message"].as_str().unwrap_or("")
            ));
        }
        if text.is_empty() {
            "没有会话".to_string()
        } else {
            text.trim_end().to_string()
        }
    });
    Ok(())
}

async fn history(client: &ApiClient, mid: u64, count: u32, out: Output) -> Result<()> {
    let messages = fetch_messages(client, mid, count).await?;
    let me = client.mid.unwrap_or(0);
    let rows: Vec<Value> = messages
        .iter()
        .map(|message| {
            json!({
                "sender_uid": message.sender_uid,
                "direction": if message.sender_uid == me { "out" } else { "in" },
                "type": message.msg_type,
                "text": message.text(),
                "timestamp": message.timestamp,
                "msg_key": message.msg_key,
                "automated": is_automated(message, None).map(|reason| reason.to_string()),
            })
        })
        .collect();
    out.emit(json!({"talker_id": mid, "messages": &rows}), || {
        let mut text = String::new();
        for row in &rows {
            let direction = if row["direction"] == "out" { "我 -> 对方" } else { "对方 -> 我" };
            let flag = row["automated"]
                .as_str()
                .map(|reason| format!(" [自动消息: {reason}]"))
                .unwrap_or_default();
            text.push_str(&format!(
                "{} {} {}{}\n",
                format_time(row["timestamp"].as_u64().unwrap_or(0)),
                direction,
                row["text"].as_str().unwrap_or(""),
                flag
            ));
        }
        if text.is_empty() {
            "没有消息".to_string()
        } else {
            text.trim_end().to_string()
        }
    });
    Ok(())
}

async fn send(client: &ApiClient, mid: u64, message: &str, out: Output) -> Result<()> {
    if message.trim().is_empty() {
        bail!("消息内容不能为空");
    }
    let data = send_text(client, mid, message).await?;
    let msg_key = data.get("msg_key").cloned().unwrap_or(Value::Null);
    out.emit(json!({"mid": mid, "sent": true, "msg_key": msg_key}), || {
        format!("已发送私信给 {mid}")
    });
    Ok(())
}

async fn reply(
    client: &ApiClient,
    mid: u64,
    message: &str,
    force: bool,
    out: Output,
) -> Result<()> {
    let latest = latest_incoming(client, mid).await?;
    let Some(latest) = latest else {
        bail!("对方没有给你发过消息，无法回复（可直接用 message send）");
    };
    if !force {
        if let Some(reason) = is_automated(&latest, None) {
            out.emit(
                json!({
                    "mid": mid,
                    "skipped": true,
                    "reason": reason,
                    "message": latest.text(),
                }),
                || format!("检测到自动消息，已跳过回复：{reason}"),
            );
            return Ok(());
        }
    }
    let data = send_text(client, mid, message).await?;
    update_state(mid, &latest.key());
    let _ = mark_read_latest(client, mid).await;
    let msg_key = data.get("msg_key").cloned().unwrap_or(Value::Null);
    out.emit(
        json!({"mid": mid, "replied": true, "msg_key": msg_key, "to_message": latest.text()}),
        || format!("已回复 {mid}"),
    );
    Ok(())
}

async fn auto_reply(
    client: &ApiClient,
    mid: u64,
    text: Option<String>,
    template: Option<PathBuf>,
    dry_run: bool,
    force: bool,
    cfg: &AppConfig,
    out: Output,
) -> Result<()> {
    let latest = latest_incoming(client, mid).await?;
    let Some(latest) = latest else {
        bail!("对方没有给你发过消息");
    };
    if !force {
        if let Some(reason) = auto_skip_reason(&latest, None) {
            out.emit(
                json!({"mid": mid, "skipped": true, "reason": reason}),
                || format!("检测到自动消息，不回复：{reason}"),
            );
            return Ok(());
        }
    }
    if state_replied(mid, &latest.key()) {
        out.emit(
            json!({"mid": mid, "skipped": true, "reason": "already_replied"}),
            || "这条消息已经回复过，跳过".to_string(),
        );
        return Ok(());
    }
    let template = read_template(text, template, cfg)?;
    let name = user_name(client, mid).await.unwrap_or_default();
    let reply = render_template(&template, mid, &name, &latest.text());
    if dry_run {
        out.emit(
            json!({"mid": mid, "dry_run": true, "reply": reply, "to_message": latest.text()}),
            || format!("[dry-run] 将回复 {mid}：{reply}"),
        );
        return Ok(());
    }
    let data = send_text(client, mid, &reply).await?;
    update_state(mid, &latest.key());
    let _ = mark_read_latest(client, mid).await;
    out.emit(
        json!({"mid": mid, "replied": true, "reply": reply, "msg_key": data.get("msg_key")}),
        || format!("已自动回复 {mid}：{reply}"),
    );
    Ok(())
}

async fn watch(
    client: &ApiClient,
    args: MessageWatchArgs,
    cfg: &AppConfig,
    out: Output,
) -> Result<()> {
    let template = read_template(args.text, args.template, cfg)?;
    let interval = args.interval.unwrap_or(cfg.dm_poll_seconds).max(5);
    loop {
        let sessions = fetch_sessions(client).await?;
        let mut handled = 0usize;
        for session in sessions.iter().filter(|session| session.unread_num > 0) {
            let mid = session.talker_id;
            let Some(latest) = latest_incoming(client, mid).await? else {
                continue;
            };
            if let Some(reason) = auto_skip_reason(&latest, Some(session)) {
                out.info(format!("跳过 {mid} 的自动消息：{reason}"));
                let _ = mark_read_latest(client, mid).await;
                continue;
            }
            if state_replied(mid, &latest.key()) {
                out.info(format!("跳过 {mid}：这条消息已回复过"));
                let _ = mark_read_latest(client, mid).await;
                continue;
            }
            let name = user_name(client, mid).await.unwrap_or_default();
            let reply = render_template(&template, mid, &name, &latest.text());
            if args.dry_run {
                out.emit(
                    json!({"event": "dry_run", "mid": mid, "reply": reply}),
                    || format!("[dry-run] {mid} <- {reply}"),
                );
                continue;
            }
            match send_text(client, mid, &reply).await {
                Ok(_) => {
                    update_state(mid, &latest.key());
                    let _ = mark_read_latest(client, mid).await;
                    handled += 1;
                    out.emit(
                        json!({"event": "replied", "mid": mid, "name": name, "reply": reply}),
                        || format!("已回复 {name} ({mid})：{reply}"),
                    );
                }
                Err(error) => {
                    out.emit(
                        json!({"event": "error", "mid": mid, "error": format!("{error:#}")}),
                        || format!("回复 {mid} 失败: {error:#}"),
                    );
                }
            }
        }
        if args.once {
            out.emit(json!({"event": "done", "handled": handled}), || {
                format!("本轮结束，回复 {handled} 条")
            });
            return Ok(());
        }
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(interval)) => {}
            _ = tokio::signal::ctrl_c() => {
                out.info("收到 Ctrl+C，停止监听");
                return Ok(());
            }
        }
    }
}

async fn read(client: &ApiClient, mid: u64, out: Output) -> Result<()> {
    mark_read_latest(client, mid).await?;
    out.emit(json!({"mid": mid, "read": true}), || {
        format!("已把 {mid} 的会话标记为已读")
    });
    Ok(())
}

async fn mark_read_latest(client: &ApiClient, mid: u64) -> Result<()> {
    let messages = fetch_messages(client, mid, 20).await?;
    let seqno = messages.first().map(|message| message.msg_seqno).unwrap_or(0);
    mark_read(client, mid, seqno).await
}

async fn unread(client: &ApiClient, out: Output) -> Result<()> {
    let sessions = fetch_sessions(client).await?;
    let unread: u64 = sessions.iter().map(|session| session.unread_num).sum();
    let rows: Vec<Value> = sessions
        .iter()
        .filter(|session| session.unread_num > 0)
        .map(|session| json!({"mid": session.talker_id, "unread": session.unread_num}))
        .collect();
    out.emit(json!({"unread": unread, "sessions": rows}), || {
        format!("未读私信 {unread} 条")
    });
    Ok(())
}

pub async fn fetch_sessions(client: &ApiClient) -> Result<Vec<Session>> {
    let params = vec![
        ("session_type".to_string(), "1".to_string()),
        ("group_id".to_string(), "0".to_string()),
        ("sort_rule".to_string(), "2".to_string()),
        ("build".to_string(), "0".to_string()),
        ("mobi_app".to_string(), "web".to_string()),
    ];
    let data = dm_get(client, "/session_svr/v1/session_svr/get_sessions", params)
        .await
        .context("获取私信会话失败")?;
    let mut sessions: Vec<Session> =
        serde_json::from_value(data.get("session_list").cloned().unwrap_or(json!([])))
            .context("解析私信会话失败")?;
    sessions.sort_by(|a, b| b.session_ts.cmp(&a.session_ts));
    Ok(sessions)
}

pub async fn fetch_messages(client: &ApiClient, mid: u64, count: u32) -> Result<Vec<Message>> {
    let params = vec![
        ("talker_id".to_string(), mid.to_string()),
        ("session_type".to_string(), "1".to_string()),
        ("size".to_string(), count.to_string()),
        ("build".to_string(), "0".to_string()),
        ("mobi_app".to_string(), "web".to_string()),
    ];
    let data = dm_get(client, "/svr_sync/v1/svr_sync/fetch_session_msgs", params)
        .await
        .with_context(|| format!("获取与 {mid} 的私信失败"))?;
    let mut messages: Vec<Message> =
        serde_json::from_value(data.get("messages").cloned().unwrap_or(json!([])))
            .context("解析私信消息失败")?;
    messages.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    Ok(messages)
}

/// 私信接口偶尔要求 WBI 签名：先按普通请求，失败后自动重试签名版本。
async fn dm_get(client: &ApiClient, path: &str, params: Vec<(String, String)>) -> Result<Value> {
    let url = format!("{VC}{path}");
    let query: Vec<(&str, String)> = params
        .iter()
        .map(|(key, value)| (key.as_str(), value.clone()))
        .collect();
    let first_result = client.get_value(&url, &query).await;
    drop(query);
    match first_result {
        Ok(value) => Ok(value),
        Err(first) => client
            .get_value_signed(&url, params)
            .await
            .with_context(|| format!("普通请求失败（{first}），WBI 重试也失败")),
    }
}

fn last_message_text(session: &Session) -> String {
    let Some(value) = &session.last_msg else {
        return String::new();
    };
    match value {
        Value::String(text) => extract_text(text),
        other => extract_text(&other.to_string()),
    }
}

/// 从私信富文本里抽出可读文字：普通文本、系统通知（title/text）、@ 与表情数组。
/// B 站会把 JSON 反复套在字符串里（会话 last_msg.content、系统通知正文等），这里最多解四层。
fn extract_text(raw: &str) -> String {
    let mut current = raw.to_string();
    for _ in 0..4 {
        let Ok(value) = serde_json::from_str::<Value>(&current) else {
            return current;
        };
        let next = match &value {
            Value::String(inner) => inner.clone(),
            _ => match text_of(&value) {
                Some(text) if !text.is_empty() => text,
                _ => {
                    if value.get("url").is_some() {
                        return "[非文本] 图片/分享".to_string();
                    }
                    return current;
                }
            },
        };
        if next == current {
            return current;
        }
        current = next;
    }
    current
}

fn text_of(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().filter_map(text_of).collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.concat())
            }
        }
        Value::Object(map) => {
            for key in ["content", "text", "title", "desc"] {
                if let Some(text) = map.get(key).and_then(Value::as_str) {
                    if !text.trim().is_empty() {
                        return Some(text.to_string());
                    }
                }
            }
            if let Some(attach) = map
                .get("attach_msg")
                .and_then(|value| value.get("content"))
                .and_then(Value::as_str)
            {
                if !attach.trim().is_empty() {
                    return Some(attach.to_string());
                }
            }
            if map.contains_key("bvid") || map.contains_key("rid") {
                return Some("[视频/分享]".to_string());
            }
            None
        }
        _ => None,
    }
}

pub async fn latest_incoming(client: &ApiClient, mid: u64) -> Result<Option<Message>> {
    let me = client.mid.unwrap_or(0);
    let messages = fetch_messages(client, mid, 20).await?;
    Ok(messages.into_iter().find(|message| message.sender_uid != me))
}

async fn send_text(client: &ApiClient, mid: u64, message: &str) -> Result<Value> {
    let me = client.require_mid()?;
    let csrf = client.csrf()?.to_string();
    let content = serde_json::to_string(&json!({ "content": message }))?;
    let timestamp = chrono::Utc::now().timestamp_millis().to_string();
    let dev_id = dev_id(me);
    let form = vec![
        ("msg[sender_uid]".to_string(), me.to_string()),
        ("msg[receiver_id]".to_string(), mid.to_string()),
        ("msg[receiver_type]".to_string(), "1".to_string()),
        ("msg[msg_type]".to_string(), "1".to_string()),
        ("msg[msg_status]".to_string(), "0".to_string()),
        ("msg[content]".to_string(), content),
        ("msg[timestamp]".to_string(), timestamp),
        ("msg[dev_id]".to_string(), dev_id),
        ("csrf".to_string(), csrf.clone()),
        ("csrf_token".to_string(), csrf),
    ];
    client
        .post_form(&format!("{VC}/web_im/v1/web_im/send_msg"), &form)
        .await
        .context("发送私信失败")
}

pub async fn mark_read(client: &ApiClient, mid: u64, ack_seqno: u64) -> Result<()> {
    if ack_seqno == 0 {
        return Ok(());
    }
    let csrf = client.csrf()?.to_string();
    let form = vec![
        ("talker_id".to_string(), mid.to_string()),
        ("session_type".to_string(), "1".to_string()),
        ("ack_seqno".to_string(), ack_seqno.to_string()),
        ("csrf".to_string(), csrf.clone()),
        ("csrf_token".to_string(), csrf),
    ];
    client
        .post_form(&format!("{VC}/session_svr/v1/session_svr/update_ack"), &form)
        .await?;
    Ok(())
}

pub async fn user_name(client: &ApiClient, mid: u64) -> Result<String> {
    let data = client
        .get_value(
            "https://api.bilibili.com/x/web-interface/card",
            &[("mid", mid.to_string())],
        )
        .await?;
    Ok(data
        .pointer("/card/name")
        .and_then(Value::as_str)
        .unwrap_or("-")
        .to_string())
}

/// 判断一条收到的消息是否属于自动/系统消息；返回原因。
pub fn is_automated(message: &Message, session: Option<&Session>) -> Option<String> {
    if message.sys_msg_type != 0 {
        return Some(format!("系统消息（sys_msg_type={}）", message.sys_msg_type));
    }
    if message.sender_uid == 1 {
        return Some("系统账号消息".to_string());
    }
    if let Some(session) = session {
        if session.system_msg_type != 0 {
            return Some("系统会话".to_string());
        }
    }
    let text = message.text().to_lowercase();
    for pattern in default_ignore_patterns() {
        let needle = pattern.to_lowercase();
        if text.contains(&needle) {
            return Some(format!("命中自动消息关键词：{pattern}"));
        }
    }
    None
}

/// 自动回复模式下的跳过原因：非文本消息无法自动回复，其余按自动消息判断。
fn auto_skip_reason(message: &Message, session: Option<&Session>) -> Option<String> {
    if message.msg_type != 1 {
        return Some(format!(
            "非文本消息（type={}），无法自动回复",
            message.msg_type
        ));
    }
    is_automated(message, session)
}

fn default_ignore_patterns() -> &'static [&'static str] {
    &[
        "自动回复",
        "机器人",
        "客服",
        "欢迎关注",
        "感谢关注",
        "感谢你的关注",
        "系统消息",
        "官方助手",
        "小助手",
        "智能助手",
        "自动发送",
        "请勿回复",
        "勿回",
        "本条为系统",
        "已收到你的消息",
        "稍后回复",
        "自动消息",
        "do not reply",
        "auto reply",
    ]
}

fn read_template(
    text: Option<String>,
    template: Option<PathBuf>,
    cfg: &AppConfig,
) -> Result<String> {
    let template = match template {
        Some(path) => std::fs::read_to_string(&path)
            .with_context(|| format!("读取回复模板失败: {}", path.display()))?,
        None => text
            .or_else(|| cfg.dm_auto_reply.clone())
            .filter(|text| !text.trim().is_empty())
            .context("没有回复内容：用 --text 指定，或在配置里设置 dm_auto_reply")?,
    };
    Ok(template)
}

fn render_template(template: &str, mid: u64, name: &str, content: &str) -> String {
    template
        .replace("{mid}", &mid.to_string())
        .replace("{name}", name)
        .replace("{content}", content)
}

fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("bilibili_client")
}

fn dev_id(mid: u64) -> String {
    let path = cache_dir().join("dm-dev-id.txt");
    if let Ok(value) = std::fs::read_to_string(&path) {
        let value = value.trim();
        if !value.is_empty() {
            return value.to_string();
        }
    }
    let seed = format!(
        "{}-{}-{mid}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    );
    let hex = format!("{:x}", md5::compute(seed));
    let value = format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
    .to_uppercase();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, &value);
    value
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct DmState {
    #[serde(default)]
    replied: BTreeMap<String, String>,
}

fn state_path() -> PathBuf {
    cache_dir().join("dm-state.json")
}

fn load_state() -> DmState {
    std::fs::read_to_string(state_path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn state_replied(mid: u64, key: &str) -> bool {
    load_state()
        .replied
        .get(&mid.to_string())
        .is_some_and(|value| value.as_str() == key)
}

fn update_state(mid: u64, key: &str) {
    let mut state = load_state();
    state.replied.insert(mid.to_string(), key.to_string());
    if let Ok(text) = serde_json::to_vec_pretty(&state) {
        let path = state_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, text);
    }
}

fn format_time(timestamp: u64) -> String {
    // 私信接口的 timestamp 是秒，个别字段是毫秒，这里按数量级判断
    let millis = if timestamp >= 1_000_000_000_000 {
        timestamp as i64
    } else {
        timestamp as i64 * 1000
    };
    chrono::DateTime::from_timestamp_millis(millis)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "-".to_string())
}

fn de_u64<'de, D>(deserializer: D) -> std::result::Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.parse::<u64>().ok()))
        .unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::{Message, Session, auto_skip_reason, extract_text, format_time, is_automated};

    fn message(text: &str) -> Message {
        Message {
            msg_type: 1,
            content: serde_json::json!({ "content": text }).to_string(),
            sender_uid: 100,
            ..Default::default()
        }
    }

    #[test]
    fn detects_automated_messages() {
        assert!(is_automated(&message("【自动回复】你好，我稍后回复"), None).is_some());
        assert!(is_automated(&message("欢迎关注我的频道"), None).is_some());
        assert!(is_automated(&message("普通聊天内容"), None).is_none());
        let mut system = message("hello");
        system.sys_msg_type = 1;
        assert!(is_automated(&system, None).is_some());
        let mut image = message("x");
        image.msg_type = 2;
        assert!(is_automated(&image, None).is_none());
        assert!(auto_skip_reason(&image, None).is_some());
    }

    #[test]
    fn session_accepts_unread_count_alias() {
        let session: Session =
            serde_json::from_value(serde_json::json!({"talker_id": 1, "unread_count": 3})).unwrap();
        assert_eq!(session.unread_num, 3);
    }

    #[test]
    fn extracts_readable_text_from_rich_content() {
        assert_eq!(message("你好").text(), "你好");

        let notice = r#"{"title":"登录操作通知","text":"你的账号在新设备登录成功","jump_text":"查看详情"}"#;
        let mut system = message("x");
        system.content = notice.to_string();
        assert_eq!(system.text(), "你的账号在新设备登录成功");

        let rich = r##"[{"text":"对方主动回复前，最多发送1条消息","color_day":"#9499A0"}]"##;
        let mut tip = message("x");
        tip.content = rich.to_string();
        assert_eq!(tip.text(), "对方主动回复前，最多发送1条消息");

        // 会话列表里 last_message 常见双层编码
        let nested = serde_json::to_string(&r#"{"content":"牛逼"}"#).unwrap();
        assert_eq!(extract_text(&nested), "牛逼");

        // 真实结构：last_msg 对象里再嵌一层 content JSON
        let envelope = "{\"sender_uid\":1,\"content\":\"{\\\"title\\\":\\\"登录操作通知\\\",\\\"text\\\":\\\"你的账号在新设备登录成功\\\"}\"}";
        assert_eq!(extract_text(envelope), "你的账号在新设备登录成功");

        // 空标题的视频分享卡片：退回 attach_msg 或类型占位
        let card = r#"{"title":"","desc":"","bvid":"BV1xx","attach_msg":{"content":"UP主赠言：更新啦"}}"#;
        assert_eq!(extract_text(card), "UP主赠言：更新啦");
        let bare = r#"{"title":"","desc":"","bvid":"BV1xx","attach_msg":null}"#;
        assert_eq!(extract_text(bare), "[视频/分享]");

        // 秒级时间戳不应被当成毫秒
        assert!(format_time(1716429339).starts_with("2024-"));
        assert!(format_time(1_716_429_339_000).starts_with("2024-"));
    }
}
