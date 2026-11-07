//! 投稿：新建、编辑、追加分 P、模板与封面。

use crate::cli::{
    SubmitArg, TemplateAction, UploadAction, UploadAppendArgs, UploadArgs, UploadEditArgs,
};
use crate::config::AppConfig;
use crate::output::Output;
use anyhow::{Context, Result, bail};
use biliup::client::StatelessClient;
use biliup::uploader::bilibili::{BiliBili, Studio, Vid, Video};
use biliup::uploader::line::{self, Line, Probe};
use biliup::uploader::season::EpisodeAdd;
use biliup::uploader::util::SubmitOption;
use biliup::uploader::VideoFile;
use futures::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use super::ApiClient;

pub async fn run(action: UploadAction, cfg: &AppConfig, out: Output) -> Result<()> {
    match action {
        UploadAction::New(args) => upload_new(args, cfg, out).await,
        UploadAction::Edit(args) => edit(args, cfg, out).await,
        UploadAction::Append(args) => append(args, cfg, out).await,
        UploadAction::Template { action } => template(action, out),
    }
}

async fn upload_new(args: UploadArgs, cfg: &AppConfig, out: Output) -> Result<()> {
    if args.files.is_empty() {
        bail!("至少需要一个视频文件");
    }
    let client = ApiClient::load(cfg).await?;
    client.require_login()?;
    let bili = client.bili()?;

    let mut studio = build_studio(&args, cfg).await?;
    if !studio.cover.is_empty() {
        let cover = upload_cover(&client, bili, &studio.cover).await?;
        studio.cover = cover;
    }

    let mut batch = upload_files(
        bili,
        client.mid.unwrap_or(0),
        &args.files,
        args.line.as_deref().or(cfg.line.as_deref()),
        args.limit.unwrap_or(cfg.limit),
    )
    .await?;
    studio.videos = std::mem::take(&mut batch.videos);
    let submit = args.submit.unwrap_or(cfg.submit);
    let response = submit_studio(bili, &studio, submit, cfg.proxy.as_deref()).await?;
    let payload = serde_json::to_value(&response)?;
    batch.finish();

    if let Some(section_id) = args.section_id {
        let aid = payload
            .pointer("/data/aid")
            .and_then(Value::as_u64)
            .context("投稿响应里没有 aid")?;
        let view = bili
            .archive_view(aid)
            .await
            .map_err(|error| anyhow::anyhow!("{error:#}"))?;
        bili.add_to_season(section_id, &[EpisodeAdd::from(&view)])
            .await
            .map_err(|error| anyhow::anyhow!("加入合集失败: {error:#}"))?;
    }

    out.emit(payload.clone(), || {
        let bvid = payload
            .pointer("/data/bvid")
            .and_then(Value::as_str)
            .unwrap_or("-");
        let aid = payload
            .pointer("/data/aid")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        format!("投稿成功: {bvid} (av{aid})")
    });
    Ok(())
}

async fn edit(args: UploadEditArgs, cfg: &AppConfig, out: Output) -> Result<()> {
    let client = ApiClient::load(cfg).await?;
    client.require_login()?;
    let bili = client.bili()?;
    let vid = normalize_vid(&args.vid)?;
    let mut studio = bili
        .studio_data(&vid, cfg.proxy.as_deref())
        .await
        .map_err(|error| anyhow::anyhow!("{error:#}"))?;
    let mut map = serde_json::to_value(&studio)?
        .as_object()
        .cloned()
        .unwrap_or_default();
    let mut cover_overridden = false;
    if let Some(path) = &args.template {
        let template = read_template(path)?;
        cover_overridden = template.contains_key("cover");
        map.extend(template);
    }
    apply_edit(&mut map, &args)?;
    if args.cover.is_some() {
        cover_overridden = true;
    }
    if cover_overridden {
        if let Some(cover) = map
            .get("cover")
            .and_then(Value::as_str)
            .filter(|cover| !cover.is_empty())
            .map(str::to_string)
        {
            map.insert(
                "cover".to_string(),
                Value::String(upload_cover(&client, bili, &cover).await?),
            );
        }
    }
    studio = serde_json::from_value(Value::Object(map))
        .context("编辑参数无法转换成投稿结构（请检查字段名）")?;

    let submit = args.submit.unwrap_or(cfg.submit);
    let response = submit_edit(bili, &studio, submit, cfg.proxy.as_deref()).await?;
    out.emit(serde_json::to_value(&response)?, || "稿件已更新".to_string());
    Ok(())
}

async fn append(args: UploadAppendArgs, cfg: &AppConfig, out: Output) -> Result<()> {
    let client = ApiClient::load(cfg).await?;
    client.require_login()?;
    let bili = client.bili()?;
    let vid = normalize_vid(&args.vid)?;
    let mut batch = upload_files(
        bili,
        client.mid.unwrap_or(0),
        &args.files,
        args.line.as_deref().or(cfg.line.as_deref()),
        args.limit.unwrap_or(cfg.limit),
    )
    .await?;
    let mut studio = bili
        .studio_data(&vid, cfg.proxy.as_deref())
        .await
        .map_err(|error| anyhow::anyhow!("{error:#}"))?;
    studio.videos.append(&mut batch.videos);
    let submit = args.submit.unwrap_or(cfg.submit);
    let response = submit_edit(bili, &studio, submit, cfg.proxy.as_deref()).await?;
    batch.finish();
    out.emit(serde_json::to_value(&response)?, || {
        format!("已追加 {} 个分 P", args.files.len())
    });
    Ok(())
}

fn template(action: TemplateAction, out: Output) -> Result<()> {
    match action {
        TemplateAction::Init { path } => {
            if path.exists() {
                bail!("文件已存在: {}", path.display());
            }
            std::fs::write(&path, EXAMPLE_TEMPLATE)?;
            out.emit(json!({"path": path}), || {
                format!("已生成投稿模板: {}", path.display())
            });
        }
        TemplateAction::Show { path } => {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("读取模板失败: {}", path.display()))?;
            out.emit(json!({"path": path, "content": text}), || text);
        }
        TemplateAction::Validate { path } => {
            let map = read_template(&path)?;
            if !map.contains_key("title") {
                bail!("模板缺少 title 字段");
            }
            let _: Studio = serde_json::from_value(Value::Object(map))
                .context("模板字段无法转换成投稿结构")?;
            out.emit(json!({"valid": true, "path": path}), || {
                format!("模板有效: {}", path.display())
            });
        }
    }
    Ok(())
}

const EXAMPLE_TEMPLATE: &str = r#"# bilibili_client 投稿模板
# CLI 参数优先于模板。未知字段会原样提交给 B 站接口。
title = "视频标题"
tid = 171                 # 分区 ID（171 = 单机游戏）
copyright = 1             # 1 自制，2 转载
source = ""               # 转载来源
cover = ""                # 本地封面文件路径
desc = "视频简介"
dynamic = ""              # 粉丝动态
tag = "标签1,标签2"
# dtime = 1760000000      # 定时发布（Unix 时间戳，需大于当前 4 小时）
# no_reprint = 0
# is_only_self = 0
# dolby = 0
# hires = 0
# charging_pay = 0
# up_selection_reply = false
# up_close_reply = false
# up_close_danmu = false
"#;

fn read_template(path: &Path) -> Result<Map<String, Value>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("读取模板失败: {}", path.display()))?;
    let value: toml::Value = toml::from_str(&text).context("模板不是合法 TOML")?;
    let value = serde_json::to_value(value)?;
    value
        .as_object()
        .cloned()
        .context("模板顶层必须是表（table）")
}

async fn build_studio(args: &UploadArgs, _cfg: &AppConfig) -> Result<Studio> {
    let mut map = match &args.template {
        Some(path) => read_template(path)?,
        None => Map::new(),
    };

    if let Some(value) = &args.title {
        map.insert("title".into(), json!(value));
    }
    if let Some(value) = &args.desc {
        map.insert("desc".into(), json!(value));
    }
    if let Some(value) = &args.tag {
        map.insert("tag".into(), json!(value));
    }
    if let Some(value) = args.tid {
        map.insert("tid".into(), json!(value));
    }
    if let Some(value) = args.tid_v2 {
        map.insert("tid_v2".into(), json!(value));
    }
    if let Some(value) = &args.cover {
        map.insert("cover".into(), json!(value));
    }
    if let Some(value) = args.copyright {
        map.insert("copyright".into(), json!(value));
    }
    if let Some(value) = &args.source {
        map.insert("source".into(), json!(value));
    }
    if let Some(value) = &args.dynamic {
        map.insert("dynamic".into(), json!(value));
    }
    if let Some(value) = &args.dtime {
        map.insert("dtime".into(), json!(parse_dtime(value)?));
    }
    if args.no_reprint {
        map.insert("no_reprint".into(), json!(1));
    }
    if args.only_self {
        map.insert("is_only_self".into(), json!(1));
    }
    if args.charging_pay {
        map.insert("charging_pay".into(), json!(1));
    }
    if args.dolby {
        map.insert("dolby".into(), json!(1));
    }
    if args.hires {
        map.insert("lossless_music".into(), json!(1));
    }
    if args.open_subtitle {
        map.insert("open_subtitle".into(), json!(true));
    }
    if args.up_selection_reply {
        map.insert("up_selection_reply".into(), json!(true));
    }
    if args.up_close_reply {
        map.insert("up_close_reply".into(), json!(true));
    }
    if args.up_close_danmu {
        map.insert("up_close_danmu".into(), json!(true));
    }
    if args.interactive {
        map.insert("interactive".into(), json!(1));
    }
    if let Some(extra) = &args.extra {
        let extra: Map<String, Value> =
            serde_json::from_str(extra).context("--extra 必须是 JSON 对象")?;
        map.extend(extra);
    }
    if !map.contains_key("title") {
        let title = args
            .files
            .first()
            .and_then(|path| path.file_stem())
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "未命名投稿".to_string());
        map.insert("title".into(), json!(title));
    }
    serde_json::from_value(Value::Object(map)).context("投稿参数无法转换成投稿结构（请检查字段名）")
}

fn apply_edit(map: &mut Map<String, Value>, args: &UploadEditArgs) -> Result<()> {
    if let Some(value) = &args.title {
        map.insert("title".into(), json!(value));
    }
    if let Some(value) = &args.desc {
        map.insert("desc".into(), json!(value));
    }
    if let Some(value) = &args.tag {
        map.insert("tag".into(), json!(value));
    }
    if let Some(value) = args.tid {
        map.insert("tid".into(), json!(value));
    }
    if let Some(value) = &args.cover {
        map.insert("cover".into(), json!(value));
    }
    if let Some(value) = args.copyright {
        map.insert("copyright".into(), json!(value));
    }
    if let Some(value) = &args.source {
        map.insert("source".into(), json!(value));
    }
    if let Some(value) = &args.dynamic {
        map.insert("dynamic".into(), json!(value));
    }
    if let Some(value) = &args.dtime {
        map.insert("dtime".into(), json!(parse_dtime(value)?));
    }
    if let Some(extra) = &args.extra {
        let extra: Map<String, Value> =
            serde_json::from_str(extra).context("--extra 必须是 JSON 对象")?;
        map.extend(extra);
    }
    Ok(())
}

fn parse_dtime(value: &str) -> Result<u32> {
    if let Ok(timestamp) = value.parse::<u32>() {
        return Ok(timestamp);
    }
    if let Ok(time) = chrono::DateTime::parse_from_rfc3339(value) {
        return Ok(time.timestamp() as u32);
    }
    let local = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M"))
        .context("定时发布时间格式应为 Unix 时间戳、RFC3339 或 \"YYYY-MM-DD HH:MM:SS\"")?;
    Ok(local.and_utc().timestamp() as u32)
}

async fn upload_files(
    bili: &BiliBili,
    mid: u64,
    files: &[PathBuf],
    line_name: Option<&str>,
    limit: usize,
) -> Result<UploadBatch> {
    let line = choose_line(line_name)
        .unwrap_or_else(|| Line::default());
    let line = if line_name.is_some_and(|name| name.eq_ignore_ascii_case("auto")) || line_name.is_none()
    {
        Probe::probe(&bili.client)
            .await
            .unwrap_or_else(|_| line)
    } else {
        line
    };

    let checkpoint_path = checkpoint_path(mid, files)?;
    let mut checkpoint = load_checkpoint(&checkpoint_path, mid, files)
        .unwrap_or_else(|| UploadCheckpoint {
            manifest: manifest(mid, files),
            uploaded: Vec::new(),
            videos: Vec::new(),
        });
    let uploaded: HashSet<String> = checkpoint.uploaded.iter().cloned().collect();

    for path in files {
        let key = path.to_string_lossy().to_string();
        if uploaded.contains(&key) {
            tracing::info!(file = %path.display(), "断点续传：跳过已上传的分 P");
            continue;
        }
        let video_file = VideoFile::new(path)
            .with_context(|| format!("读取视频文件失败: {}", path.display()))?;
        let total = video_file.total_size;
        let bar = ProgressBar::new(total);
        let _ = bar.set_style(
            ProgressStyle::with_template(
                "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})",
            )
            .unwrap_or_else(|_| ProgressStyle::default_bar()),
        );
        let parcel = line
            .pre_upload(bili, video_file)
            .await
            .map_err(|error| anyhow::anyhow!("申请上传失败: {error:#}"))?;
        let video = parcel
            .upload(StatelessClient::default(), limit.max(1), |stream| {
                let bar = bar.clone();
                stream.map(
                    move |chunk: std::io::Result<bytes::Bytes>|
                          -> biliup::error::Result<(bytes::Bytes, usize)> {
                        let chunk = chunk?;
                        let length = chunk.len();
                        bar.inc(length as u64);
                        Ok((chunk, length))
                    },
                )
            })
            .await
            .map_err(|error| anyhow::anyhow!("上传失败: {error:#}"))?;
        bar.finish_and_clear();
        tracing::info!(file = %path.display(), name = %video.filename, "分 P 上传完成");
        checkpoint.videos.push(video);
        checkpoint.uploaded.push(key);
        save_checkpoint(&checkpoint_path, &checkpoint);
    }

    Ok(UploadBatch {
        videos: checkpoint.videos,
        checkpoint: checkpoint_path,
    })
}

struct UploadBatch {
    videos: Vec<Video>,
    checkpoint: PathBuf,
}

impl UploadBatch {
    fn finish(self) {
        let _ = std::fs::remove_file(self.checkpoint);
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct UploadCheckpoint {
    manifest: String,
    uploaded: Vec<String>,
    videos: Vec<Video>,
}

fn checkpoint_path(mid: u64, files: &[PathBuf]) -> Result<PathBuf> {
    let manifest = manifest(mid, files);
    let directory = dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("bilibili_client");
    std::fs::create_dir_all(&directory)?;
    Ok(directory.join(format!("upload-{manifest}.json")))
}

fn manifest(mid: u64, files: &[PathBuf]) -> String {
    let mut text = format!("mid={mid};");
    for file in files {
        let metadata = std::fs::metadata(file);
        let canonical = file.canonicalize().unwrap_or_else(|_| file.clone());
        let length = metadata.as_ref().map(|meta| meta.len()).unwrap_or(0);
        let modified = metadata
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        text.push_str(&format!(
            "{}|{length}|{modified};",
            canonical.to_string_lossy()
        ));
    }
    format!("{:x}", md5::compute(text))
}

fn load_checkpoint(path: &Path, mid: u64, files: &[PathBuf]) -> Option<UploadCheckpoint> {
    let text = std::fs::read_to_string(path).ok()?;
    let checkpoint: UploadCheckpoint = serde_json::from_str(&text).ok()?;
    (checkpoint.manifest == manifest(mid, files)
        && checkpoint.uploaded.len() == checkpoint.videos.len())
    .then_some(checkpoint)
}

fn save_checkpoint(path: &Path, checkpoint: &UploadCheckpoint) {
    if let Ok(text) = serde_json::to_vec_pretty(checkpoint) {
        let temporary = path.with_extension("tmp");
        if std::fs::write(&temporary, text).is_ok() {
            if path.exists() {
                let _ = std::fs::remove_file(path);
            }
            let _ = std::fs::rename(&temporary, path);
        }
    }
}

fn choose_line(name: Option<&str>) -> Option<Line> {
    let name = name?.trim().to_ascii_lowercase();
    Some(match name.as_str() {
        "auto" | "" => return None,
        "bldsa" => line::bldsa(),
        "cnbldsa" => line::cnbldsa(),
        "andsa" => line::andsa(),
        "atdsa" => line::atdsa(),
        "bda2" => line::bda2(),
        "cnbd" => line::cnbd(),
        "anbd" => line::anbd(),
        "atbd" => line::atbd(),
        "tx" => line::tx(),
        "cntx" => line::cntx(),
        "antx" => line::antx(),
        "attx" => line::attx(),
        "txa" => line::txa(),
        "alia" => line::alia(),
        "estx" => line::estx(),
        "akbd" => line::akbd(),
        other => {
            tracing::warn!(line = other, "未知上传线路，改为自动测速");
            return None;
        }
    })
}

async fn upload_cover(client: &ApiClient, bili: &BiliBili, cover: &str) -> Result<String> {
    let bytes = if cover.starts_with("http://") || cover.starts_with("https://") {
        let response = client.get_raw(cover).await?;
        response.bytes().await?.to_vec()
    } else {
        std::fs::read(cover).with_context(|| format!("读取封面失败: {cover}"))?
    };
    bili.cover_up(&bytes)
        .await
        .map_err(|error| anyhow::anyhow!("上传封面失败: {error:#}"))
}

fn submit_option(value: SubmitArg) -> SubmitOption {
    match value {
        SubmitArg::App => SubmitOption::App,
        SubmitArg::Web => SubmitOption::Web,
        SubmitArg::Bcut => SubmitOption::BCutAndroid,
    }
}

async fn submit_studio(
    bili: &BiliBili,
    studio: &Studio,
    submit: SubmitArg,
    proxy: Option<&str>,
) -> Result<biliup::uploader::bilibili::ResponseData> {
    let option = submit_option(submit);
    match option {
        SubmitOption::Web => bili.submit_by_web(studio, proxy).await,
        SubmitOption::BCutAndroid => bili.submit_by_bcut_android(studio, proxy).await,
        _ => bili.submit_by_app(studio, proxy).await,
    }
    .map_err(|error| anyhow::anyhow!("投稿失败: {error:#}"))
}

async fn submit_edit(
    bili: &BiliBili,
    studio: &Studio,
    submit: SubmitArg,
    proxy: Option<&str>,
) -> Result<serde_json::Value> {
    match submit_option(submit) {
        SubmitOption::App => bili.edit_by_app(studio, proxy).await,
        _ => bili.edit_by_web(studio).await,
    }
    .map_err(|error| anyhow::anyhow!("更新稿件失败: {error:#}"))
}

fn normalize_vid(input: &str) -> Result<Vid> {
    let text = input.trim();
    if let Ok(vid) = Vid::from_str(text) {
        return Ok(vid);
    }
    if let Some(index) = text.find("/video/") {
        let token = text[index + 7..]
            .split(['/', '?', '#'])
            .next()
            .unwrap_or("");
        return Vid::from_str(token).map_err(|_| anyhow::anyhow!("无法识别的稿件 ID: {input}"));
    }
    bail!("无法识别的稿件 ID: {input}")
}
