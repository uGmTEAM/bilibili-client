//! 评论读取、发表、删除、点赞与点踩。

use crate::cli::{CommentAction, FeedKind, SortArg};
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
        CommentAction::Replies {
            id,
            rpid,
            page,
            size,
        } => replies(&client, &id, rpid, page, size, out).await,
        CommentAction::Received { kind, cursor } => received(&client, kind, cursor, out).await,
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

/// 读取某条根评论下的楼中楼（分页）。
async fn replies(
    client: &ApiClient,
    id: &str,
    rpid: u64,
    page: u32,
    size: u32,
    out: Output,
) -> Result<()> {
    let aid = aid_of(client, id).await?;
    let data = client
        .get_value(
            "https://api.bilibili.com/x/v2/reply/reply",
            &[
                ("type", "1".to_string()),
                ("oid", aid.to_string()),
                ("root", rpid.to_string()),
                ("pn", page.to_string()),
                ("ps", size.to_string()),
            ],
        )
        .await?;
    let list = data
        .get("replies")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let total = data
        .pointer("/page/count")
        .and_then(Value::as_u64)
        .unwrap_or(0);

    out.emit(
        json!({"root": rpid, "total": total, "page": page, "size": size, "replies": data}),
        || {
            let mut text = format!("评论 {rpid} 下共 {total} 条回复（第 {page} 页）\n");
            if list.is_empty() {
                text.push_str("（本页没有回复）");
            }
            for reply in &list {
                text.push_str(&format_reply(reply, 0));
            }
            text.trim_end().to_string()
        },
    );
    Ok(())
}

/// 读取「回复我的 / @我的 / 赞我的」消息流（游标翻页）。
async fn received(
    client: &ApiClient,
    kind: FeedKind,
    cursor: Option<u64>,
    out: Output,
) -> Result<()> {
    client.require_login()?;
    let url = match kind {
        FeedKind::Reply => "https://api.bilibili.com/x/msgfeed/reply",
        FeedKind::At => "https://api.bilibili.com/x/msgfeed/at",
        FeedKind::Like => "https://api.bilibili.com/x/msgfeed/like",
    };
    let mut query: Vec<(&str, String)> = vec![
        ("platform", "web".to_string()),
        ("mobi_app", "web".to_string()),
        ("build", "0".to_string()),
    ];
    if let Some(cursor) = cursor {
        query.push(("id", cursor.to_string()));
    }
    let data = client.get_value(url, &query).await?;
    // 「赞我的」把列表放在 latest / total 里，其余类型直接放 items
    let items = data
        .get("items")
        .and_then(Value::as_array)
        .or_else(|| {
            data.pointer("/latest/items")
                .and_then(Value::as_array)
                .filter(|items| !items.is_empty())
        })
        .or_else(|| data.pointer("/total/items").and_then(Value::as_array))
        .cloned()
        .unwrap_or_default();
    let next_cursor = data
        .pointer("/cursor/id")
        .or_else(|| data.pointer("/total/cursor/id"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let is_end = data
        .pointer("/cursor/is_end")
        .or_else(|| data.pointer("/total/cursor/is_end"))
        .and_then(Value::as_bool)
        .unwrap_or(true);

    out.emit(
        json!({
            "kind": format!("{kind:?}").to_lowercase(),
            "next_cursor": next_cursor,
            "is_end": is_end,
            "items": data
        }),
        || {
            if items.is_empty() {
                return "没有新消息".to_string();
            }
            let mut text = String::new();
            for item in &items {
                text.push_str(&format_feed(item, kind));
            }
            text.push_str(&format!(
                "\n本页 {} 条，{}",
                items.len(),
                if is_end {
                    "已到最后一页".to_string()
                } else {
                    format!("下一页加 --cursor {next_cursor}")
                }
            ));
            text
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
    let time = format_time(ctime);
    let message = reply
        .pointer("/content/message")
        .and_then(Value::as_str)
        .unwrap_or("");
    let indent = "  ".repeat(depth);
    format!("{indent}#{rpid} {uname}  {time}  赞{like}\n{indent}  {message}\n")
}

/// 「回复我的」一条消息的文本化。
fn format_feed(item: &Value, kind: FeedKind) -> String {
    let nickname = item
        .pointer("/user/nickname")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            let names: Vec<&str> = item
                .get("users")
                .and_then(Value::as_array)?
                .iter()
                .filter_map(|user| user.get("nickname").and_then(Value::as_str))
                .collect();
            (!names.is_empty()).then(|| names.join("、"))
        })
        .unwrap_or_else(|| "有人".to_string());
    let ctime = item
        .get("reply_time")
        .and_then(Value::as_i64)
        .or_else(|| item.get("at_time").and_then(Value::as_i64))
        .or_else(|| item.get("like_time").and_then(Value::as_i64))
        .or_else(|| item.pointer("/item/ctime").and_then(Value::as_i64))
        .unwrap_or(0);
    let action = match kind {
        FeedKind::Reply => "回复了你",
        FeedKind::At => "@了你",
        FeedKind::Like => "赞了你",
    };
    let content = feed_text(item.pointer("/item/source_content"))
        .or_else(|| feed_text(item.get("reply_content")));
    let mut text = format!("{}  {nickname} {action}", format_time(ctime));
    if let Some(content) = &content {
        text.push_str(&format!("：{content}"));
    }
    let title = item
        .pointer("/item/title")
        .and_then(Value::as_str)
        .filter(|title| !title.is_empty());
    if let Some(title) = title {
        match kind {
            // 回复消息里的 title 是根评论原文，不是视频标题
            FeedKind::Reply => {
                if content.as_deref() != Some(title) {
                    text.push_str(&format!("\n  评论原文：{title}"));
                }
            }
            _ => text.push_str(&format!("  《{title}》")),
        }
    }
    if let Some(uri) = item
        .pointer("/item/uri")
        .and_then(Value::as_str)
        .filter(|uri| !uri.is_empty())
    {
        text.push_str(&format!("\n  {uri}"));
    }
    text.push('\n');
    text
}

/// 消息内容既可能是字符串，也可能是 `{"message": "..."}` 对象。
fn feed_text(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Object(map) => map
            .get("message")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_string),
        _ => None,
    }
}

fn format_time(timestamp: i64) -> String {
    chrono::DateTime::from_timestamp(timestamp, 0)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "-".to_string())
}

#[cfg(test)]
mod tests {
    use super::{format_feed, feed_text};
    use crate::cli::FeedKind;
    use serde_json::json;

    #[test]
    fn feed_text_accepts_string_and_object() {
        assert_eq!(feed_text(Some(&json!("hello"))).as_deref(), Some("hello"));
        assert_eq!(
            feed_text(Some(&json!({"message": "hi"}))).as_deref(),
            Some("hi")
        );
        assert_eq!(feed_text(Some(&json!(""))), None);
        assert_eq!(feed_text(None), None);
    }

    #[test]
    fn format_feed_covers_multi_user_like() {
        let item = json!({
            "users": [{"nickname": "甲"}, {"nickname": "乙"}],
            "like_time": 1761550000,
            "item": {
                "ctime": 1726985107,
                "uri": "https://www.bilibili.com/video/BV1xx",
                "title": "视频标题"
            }
        });
        let text = format_feed(&item, FeedKind::Like);
        assert!(text.contains("甲、乙 赞了你"), "{text}");
        assert!(text.contains("《视频标题》"), "{text}");
        assert!(text.contains("BV1xx"), "{text}");
        assert!(!text.contains("1970"), "时间应取 like_time，实际 {text}");
    }

    #[test]
    fn format_feed_uses_reply_time_and_source_content() {
        let item = json!({
            "user": {"nickname": "张三"},
            "reply_time": 1792895630,
            "item": {
                "source_content": "你是凯伊吗",
                "title": "UP主的视频挺有意思",
                "uri": "https://www.bilibili.com/video/BV1GX"
            }
        });
        let text = format_feed(&item, FeedKind::Reply);
        assert!(text.contains("张三 回复了你：你是凯伊吗"), "{text}");
        assert!(text.contains("评论原文：UP主的视频挺有意思"), "{text}");
        assert!(!text.contains("1970"), "时间应取 reply_time，实际 {text}");
    }

    #[test]
    fn format_feed_uses_at_time() {
        let item = json!({
            "user": {"nickname": "甲"},
            "at_time": 1793000000,
            "item": {
                "source_content": "@你 看这个",
                "title": "视频标题",
                "uri": "https://www.bilibili.com/video/BV1at"
            }
        });
        let text = format_feed(&item, FeedKind::At);
        assert!(text.contains("甲 @了你：@你 看这个"), "{text}");
        assert!(!text.contains("1970"), "时间应取 at_time，实际 {text}");
    }
}
