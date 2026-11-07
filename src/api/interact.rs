//! 点赞、投币、收藏与关注。

use crate::cli::InteractAction;
use crate::config::AppConfig;
use crate::output::Output;
use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::ApiClient;
use super::comment::aid_of;

pub async fn run(action: InteractAction, cfg: &AppConfig, out: Output) -> Result<()> {
    let client = ApiClient::load(cfg).await?;
    match action {
        InteractAction::Triple {
            id,
            coin,
            folder_id,
            no_like,
            no_coin,
            no_fav,
        } => {
            triple(
                &client,
                &id,
                coin,
                folder_id,
                no_like,
                no_coin,
                no_fav,
                out,
            )
            .await
        }
        InteractAction::Follow { mid, undo } => follow(&client, mid, undo, out).await,
    }
}

#[allow(clippy::too_many_arguments)]
async fn triple(
    client: &ApiClient,
    id: &str,
    coin: u8,
    folder_id: Option<u64>,
    no_like: bool,
    no_coin: bool,
    no_fav: bool,
    out: Output,
) -> Result<()> {
    client.require_login()?;
    let aid = aid_of(client, id).await?;
    let csrf = client.csrf()?.to_string();
    let mut result = json!({"aid": aid});
    let mut errors: Vec<String> = Vec::new();

    if !no_like {
        let form = vec![
            ("aid".to_string(), aid.to_string()),
            ("like".to_string(), "1".to_string()),
            ("csrf".to_string(), csrf.clone()),
        ];
        match client
            .post_form("https://api.bilibili.com/x/web-interface/archive/like", &form)
            .await
        {
            Ok(_) => result["like"] = json!(true),
            Err(error) => {
                result["like"] = json!(false);
                errors.push(format!("点赞失败: {error:#}"));
            }
        }
    }

    if !no_coin {
        let multiply = coin.clamp(1, 2).to_string();
        let form = vec![
            ("aid".to_string(), aid.to_string()),
            ("multiply".to_string(), multiply),
            ("select_like".to_string(), "1".to_string()),
            ("csrf".to_string(), csrf.clone()),
        ];
        match client
            .post_form("https://api.bilibili.com/x/web-interface/coin/add", &form)
            .await
        {
            Ok(_) => result["coin"] = json!(coin.clamp(1, 2)),
            Err(error) => {
                result["coin"] = json!(0);
                errors.push(format!("投币失败: {error:#}"));
            }
        }
    }

    if !no_fav {
        let folder = match folder_id {
            Some(folder) => folder,
            None => default_favorite_folder(client).await?,
        };
        let form = vec![
            ("rid".to_string(), aid.to_string()),
            ("type".to_string(), "2".to_string()),
            ("add_media_ids".to_string(), folder.to_string()),
            ("csrf".to_string(), csrf),
        ];
        match client
            .post_form("https://api.bilibili.com/x/v3/fav/resource/deal", &form)
            .await
        {
            Ok(_) => {
                result["favorite"] = json!(true);
                result["folder_id"] = json!(folder);
            }
            Err(error) => {
                result["favorite"] = json!(false);
                errors.push(format!("收藏失败: {error:#}"));
            }
        }
    }

    if !errors.is_empty() {
        result["errors"] = json!(errors);
    }
    out.emit(result.clone(), || {
        let mut text = format!("av{aid}: ");
        if result.get("like").and_then(Value::as_bool) == Some(true) {
            text.push_str("已点赞 ");
        }
        if result.get("coin").and_then(Value::as_u64).unwrap_or(0) > 0 {
            text.push_str("已投币 ");
        }
        if result.get("favorite").and_then(Value::as_bool) == Some(true) {
            text.push_str("已收藏 ");
        }
        for error in errors {
            text.push_str(&format!("\n{error}"));
        }
        text.trim_end().to_string()
    });
    Ok(())
}

async fn default_favorite_folder(client: &ApiClient) -> Result<u64> {
    let mid = client.require_mid()?;
    let data = client
        .get_value(
            "https://api.bilibili.com/x/v3/fav/folder/created/list-all",
            &[("up_mid", mid.to_string())],
        )
        .await?;
    data.get("list")
        .and_then(Value::as_array)
        .and_then(|list| list.first())
        .and_then(|folder| folder.get("id"))
        .and_then(Value::as_u64)
        .context("没有找到收藏夹，请用 --folder-id 指定")
}

async fn follow(client: &ApiClient, mid: u64, undo: bool, out: Output) -> Result<()> {
    client.require_login()?;
    let act = if undo { "2" } else { "1" };
    let form = vec![
        ("fid".to_string(), mid.to_string()),
        ("act".to_string(), act.to_string()),
        ("re_src".to_string(), "11".to_string()),
        ("csrf".to_string(), client.csrf()?.to_string()),
    ];
    client
        .post_form("https://api.bilibili.com/x/relation/modify", &form)
        .await?;
    out.emit(json!({"mid": mid, "following": !undo}), || {
        format!("{}用户 {mid}", if undo { "已取消关注" } else { "已关注" })
    });
    Ok(())
}
