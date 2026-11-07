//! 评论读取、发表、删除、点赞与点踩。

use crate::cli::{CommentAction, SortArg};
use crate::config::AppConfig;
use crate::output::Output;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::ApiClient;
use super::video::{MediaId, self};

pub async fn run(action: CommentAction, cfg: &AppConfig, out: Output) -> Result<()> {
    let client = ApiClient::load(cfg).await?;
    match action {
        CommentAction::List {
            id,
            sort,
            page,
            size,
            replies,
        } => list(&client, &id, sort, page, size, replies, out).await,
        CommentAction::Send { id, message, root } => {
            send(&client, &id, &message, root, out).await
        }
        CommentAction::Delete { id, rpid } => delete(&client, &id, rpid, out).await,
        CommentAction::Like { id, rpid, undo } => like(&client, &id, rpid, undo, out).await,
        CommentAction::Dislike { id, rpid, undo } => dislike(&client, &id, rpid, undo, out).await,
    }
}

async fn list(
    client: &ApiClient,
    id: &str,
    sort: SortArg,
    page: u32,
    size: u32,
    with_replies: bool,
    out: Output,
) -> Result<()> {
    let aid = aid_of(client, id).await?;
    let data = client
        .get_value(
            "https://api.bilibili.com/x/v2/reply",
            &[
                ("type", "1".to_string()),
                ("oid", aid.to_string()),
                ("sort", sort.value().to_string()),
                ("pn", page.to_string()),
                ("ps", size.to_string()),
            ],
        )
        .await?;

    let replies = data
        .get("replies")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let total = data
        .pointer("/page/count")
        .and_then(Value::as_u64)
        .unwrap_or(0);

    out.emit(
        json!({"total": total, "page": page, "size": size, "replies": data}),
        || {
            let mut text = format!("共 {total} 条评论（第 {page} 页）\n");
            for reply in &replies {
                text.push_str(&format_reply(reply, 0));
                if with_replies {
                    if let Some(children) = reply.get("replies").and_then(Value::as_array) {
                        for child in children {
                            text.push_str(&format_reply(child, 1));
                        }
                    }
                }
            }
            text.trim_end().to_string()
        },
    );
    Ok(())
}

async fn send(
    client: &ApiClient,
    id: &str,
    message: &str,
    root: Option<u64>,
    out: Output,
) -> Result<()> {
    client.require_login()?;
    let aid = aid_of(client, id).await?;
    let csrf = client.csrf()?.to_string();
    let mut form = vec![
        ("type".to_string(), "1".to_string()),
        ("oid".to_string(), aid.to_string()),
        ("message".to_string(), message.to_string()),
        ("csrf".to_string(), csrf),
    ];
    if let Some(root) = root {
        form.push(("root".to_string(), root.to_string()));
        form.push(("parent".to_string(), root.to_string()));
    }
    let data = client
        .post_form("https://api.bilibili.com/x/v2/reply/add", &form)
        .await?;
    let rpid = data.get("rpid").and_then(Value::as_u64);
    out.emit(json!({"rpid": rpid, "data": data}), || {
        format!(
            "评论已发送{}",
            rpid.map(|rpid| format!("，rpid={rpid}")).unwrap_or_default()
        )
    });
    Ok(())
}

async fn delete(client: &ApiClient, id: &str, rpid: u64, out: Output) -> Result<()> {
    client.require_login()?;
    let aid = aid_of(client, id).await?;
    let form = vec![
        ("type".to_string(), "1".to_string()),
        ("oid".to_string(), aid.to_string()),
        ("rpid".to_string(), rpid.to_string()),
        ("csrf".to_string(), client.csrf()?.to_string()),
    ];
    client
        .post_form("https://api.bilibili.com/x/v2/reply/del", &form)
        .await?;
    out.emit(json!({"deleted": rpid}), || format!("已删除评论 {rpid}"));
    Ok(())
}

async fn like(
    client: &ApiClient,
    id: &str,
    rpid: u64,
    undo: bool,
    out: Output,
) -> Result<()> {
    client.require_login()?;
    let aid = aid_of(client, id).await?;
    let action = if undo { "0" } else { "1" };
    let form = vec![
        ("type".to_string(), "1".to_string()),
        ("oid".to_string(), aid.to_string()),
        ("rpid".to_string(), rpid.to_string()),
        ("action".to_string(), action.to_string()),
        ("csrf".to_string(), client.csrf()?.to_string()),
    ];
    client
        .post_form("https://api.bilibili.com/x/v2/reply/action", &form)
        .await?;
    out.emit(json!({"rpid": rpid, "liked": !undo}), || {
        format!("{}评论 {rpid}", if undo { "已取消点赞" } else { "已点赞" })
    });
    Ok(())
}

async fn dislike(
    client: &ApiClient,
    id: &str,
    rpid: u64,
    undo: bool,
    out: Output,
) -> Result<()> {
    client.require_login()?;
    let aid = aid_of(client, id).await?;
    let action = if undo { "0" } else { "1" };
    let form = vec![
        ("type".to_string(), "1".to_string()),
        ("oid".to_string(), aid.to_string()),
        ("rpid".to_string(), rpid.to_string()),
        ("action".to_string(), action.to_string()),
        ("csrf".to_string(), client.csrf()?.to_string()),
    ];
    client
        .post_form("https://api.bilibili.com/x/v2/reply/hate", &form)
        .await?;
    out.emit(json!({"rpid": rpid, "disliked": !undo}), || {
        format!("{}评论 {rpid}", if undo { "已取消点踩" } else { "已点踩" })
    });
    Ok(())
}

pub async fn aid_of(client: &ApiClient, id: &str) -> Result<u64> {
    let media = video::resolve_media(client, id).await?;
    match &media {
        MediaId::Video(_) => Ok(video::view(client, &media).await?.aid),
        MediaId::Episode(_) | MediaId::Season(_) => {
            let season = video::pgc_season(client, &media).await?;
            let episode = match &media {
                MediaId::Episode(ep) => season
                    .episodes
                    .iter()
                    .find(|episode| episode.id == *ep)
                    .with_context(|| format!("找不到 ep{ep}"))?,
                _ => season.episodes.first().context("番剧没有剧集")?,
            };
            if episode.aid == 0 {
                bail!("该剧集没有 aid");
            }
            Ok(episode.aid)
        }
    }
}

fn format_reply(reply: &Value, depth: usize) -> String {
    let uname = reply
        .pointer("/member/uname")
        .and_then(Value::as_str)
        .unwrap_or("-");
    let rpid = reply.get("rpid").and_then(Value::as_u64).unwrap_or(0);
    let like = reply.get("like").and_then(Value::as_u64).unwrap_or(0);
    let ctime = reply.get("ctime").and_then(Value::as_i64).unwrap_or(0);
    let time = chrono::DateTime::from_timestamp(ctime, 0)
        .map(|time| time.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "-".to_string());
    let message = reply
        .pointer("/content/message")
        .and_then(Value::as_str)
        .unwrap_or("");
    let indent = "  ".repeat(depth);
    format!("{indent}#{rpid} {uname}  {time}  赞{like}\n{indent}  {message}\n")
}
