//! 合集（小节）管理。

use crate::cli::{SeasonAction};
use crate::config::AppConfig;
use crate::output::Output;
use anyhow::Result;
use biliup::uploader::bilibili::Vid;
use biliup::uploader::season::{EpisodeAdd, EpisodeSort};
use serde_json::json;
use std::str::FromStr;

use super::ApiClient;

pub async fn run(action: SeasonAction, cfg: &AppConfig, out: Output) -> Result<()> {
    let client = ApiClient::load(cfg).await?;
    client.require_login()?;
    let bili = client.bili()?;

    match action {
        SeasonAction::List { page, size } => {
            let seasons = bili
                .seasons(page, size)
                .await
                .map_err(|error| anyhow::anyhow!("{error:#}"))?;
            out.emit(serde_json::to_value(&seasons)?, || {
                let mut text = String::new();
                for entry in &seasons.seasons {
                    text.push_str(&format!("合集 {} {}\n", entry.season.id, entry.season.title));
                    for section in &entry.sections.sections {
                        text.push_str(&format!(
                            "  小节 {} {} (season={})\n",
                            section.id,
                            section.title,
                            section.season_id.unwrap_or(entry.season.id)
                        ));
                    }
                }
                text.trim_end().to_string()
            });
        }
        SeasonAction::Sections { section_id, sort } => {
            let detail = bili
                .season_section(section_id, Some(sort.as_str()))
                .await
                .map_err(|error| anyhow::anyhow!("{error:#}"))?;
            out.emit(serde_json::to_value(&detail)?, || {
                let mut text = format!("小节 {} {}\n", detail.section.id, detail.section.title);
                for episode in &detail.episodes {
                    text.push_str(&format!(
                        "  episode={} av{} {}\n",
                        episode.id, episode.aid, episode.title
                    ));
                }
                text.trim_end().to_string()
            });
        }
        SeasonAction::Add { section_id, vids } => {
            let mut episodes = Vec::new();
            for vid in &vids {
                let vid = parse_vid(vid)?;
                let view = bili
                    .archive_view_by_vid(&vid)
                    .await
                    .map_err(|error| anyhow::anyhow!("{error:#}"))?;
                episodes.push(EpisodeAdd::from(&view));
            }
            bili.add_to_season(section_id, &episodes)
                .await
                .map_err(|error| anyhow::anyhow!("{error:#}"))?;
            out.emit(json!({"section_id": section_id, "added": episodes.len()}), || {
                format!("已把 {} 个稿件加入小节 {section_id}", episodes.len())
            });
        }
        SeasonAction::Remove { episode_id } => {
            bili.remove_from_season(episode_id)
                .await
                .map_err(|error| anyhow::anyhow!("{error:#}"))?;
            out.emit(json!({"removed": episode_id}), || {
                format!("已移出 episode {episode_id}")
            });
        }
        SeasonAction::Sort {
            section_id,
            season_id,
            section_title,
            mut episode_ids,
            reverse,
        } => {
            if episode_ids.is_empty() {
                anyhow::bail!("至少需要一个 episode id");
            }
            if reverse {
                episode_ids.reverse();
            }
            let sorts: Vec<EpisodeSort> = episode_ids
                .iter()
                .enumerate()
                .map(|(index, id)| EpisodeSort {
                    id: *id,
                    sort: (index + 1) as u32,
                })
                .collect();
            bili.sort_season_episodes(section_id, season_id, &sorts, &section_title)
                .await
                .map_err(|error| anyhow::anyhow!("{error:#}"))?;
            out.emit(
                json!({"section_id": section_id, "season_id": season_id, "sorted": sorts.len()}),
                || format!("已重排 {} 个稿件", sorts.len()),
            );
        }
    }
    Ok(())
}

fn parse_vid(input: &str) -> Result<Vid> {
    Vid::from_str(input.trim()).map_err(|_| anyhow::anyhow!("无法识别的稿件 ID: {input}"))
}
