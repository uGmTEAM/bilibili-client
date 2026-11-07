//! 投稿：新建、编辑、追加分 P、模板与封面。

use crate::cli::{
    SubmitArg, TemplateAction, UploadAction, UploadAppendArgs, UploadArgs, UploadEditArgs,
};
use crate::config::AppConfig;
use crate::output::Output;
use anyhow::{Context, Result, bail};
use biliup::client::StatelessClient;
use biliup::error::Kind;
use biliup::uploader::bilibili::{BiliBili, Studio, Vid, Video};
use biliup::uploader::line::{self, Line, Probe};
use biliup::uploader::season::EpisodeAdd;
use biliup::uploader::util::SubmitOption;
use biliup::uploader::VideoFile;
use futures::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::header::HeaderMap;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

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
        cfg.proxy.as_deref(),
        !args.no_auto_line_retry,
        args.line_retry_max.max(1),
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
        cfg.proxy.as_deref(),
        !args.no_auto_line_retry,
        args.line_retry_max.max(1),
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
    let naive = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M"))
        .context("定时发布时间格式应为 Unix 时间戳、RFC3339 或 \"YYYY-MM-DD HH:MM:SS\"")?;
    use chrono::TimeZone;
    let local = chrono::Local
        .from_local_datetime(&naive)
        .single()
        .context("定时发布时间在当前时区不唯一（夏令时切换）")?;
    Ok(local.timestamp() as u32)
}

async fn upload_files(
    bili: &BiliBili,
    mid: u64,
    files: &[PathBuf],
    line_name: Option<&str>,
    limit: usize,
    proxy: Option<&str>,
    auto_line_retry: bool,
    max_line_retry: usize,
) -> Result<UploadBatch> {
    let candidate_lines = build_line_pool(line_name, bili).await;
    if candidate_lines.is_empty() {
        bail!("没有可用的上传线路（probe 测速全部失败，且未指定 --line）");
    }
    tracing::info!(
        line = candidate_lines[0].query.as_str(),
        pool = candidate_lines.len(),
        "首选上传线路"
    );

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
        let total = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
        let bar = ProgressBar::new(total);
        let _ = bar.set_style(
            ProgressStyle::with_template(
                "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})",
            )
            .unwrap_or_else(|_| ProgressStyle::default_bar()),
        );

        let mut last_error: Option<Kind> = None;
        let mut risk_hit = false;

        for (attempt, line) in candidate_lines.iter().enumerate() {
            let client = StatelessClient::new(HeaderMap::new(), proxy);
            tracing::info!(
                file = %path.display(),
                line = line.query.as_str(),
                attempt = attempt + 1,
                "尝试上传"
            );

            // 每次重试都重新打开文件，避免复用读到一半的句柄
            let video_file = match VideoFile::new(path) {
                Ok(video_file) => video_file,
                Err(error) => {
                    bar.finish_and_clear();
                    return Err(anyhow::anyhow!(
                        "读取视频文件失败: {}: {error}",
                        path.display()
                    ));
                }
            };

            match try_upload_one(bili, video_file, line, client, limit.max(1), bar.clone()).await {
                Ok(video) => {
                    bar.finish_and_clear();
                    tracing::info!(file = %path.display(), name = %video.filename, "分 P 上传完成");
                    checkpoint.videos.push(video);
                    checkpoint.uploaded.push(key.clone());
                    save_checkpoint(&checkpoint_path, &checkpoint);
                    last_error = None;
                    break;
                }
                Err(kind) => {
                    let is_risk = is_risk_control(&kind);
                    if is_risk {
                        risk_hit = true;
                        if !auto_line_retry || attempt + 1 >= candidate_lines.len().min(max_line_retry) {
                            last_error = Some(kind);
                            break;
                        }
                        eprintln!(
                            "⚠️  线路 {} 触发风控（412/限流），稍后切换下一条线路重试...",
                            line.query
                        );
                        last_error = Some(kind);
                        let backoff = Duration::from_secs(30 * (1 << attempt.min(3)));
                        tokio::time::sleep(backoff).await;
                        continue;
                    }
                    // 非风控错误直接返回，不换线路
                    bar.finish_and_clear();
                    return Err(anyhow::anyhow!("上传失败: {kind}"));
                }
            }
        }

        if let Some(kind) = last_error {
            bar.finish_and_clear();
            if risk_hit {
                return Err(enrich_risk_error(kind, proxy, auto_line_retry));
            }
            return Err(anyhow::anyhow!("上传失败: {kind}"));
        }
    }

    Ok(UploadBatch {
        videos: checkpoint.videos,
        checkpoint: checkpoint_path,
    })
}

/// 单次上传尝试（pre_upload + PUT 分片），保留 biliup 的原始错误类型以便识别风控。
async fn try_upload_one(
    bili: &BiliBili,
    video_file: VideoFile,
    line: &Line,
    stateless: StatelessClient,
    limit: usize,
    bar: ProgressBar,
) -> biliup::error::Result<Video> {
    let parcel = line.pre_upload(bili, video_file).await?;
    parcel
        .upload(stateless, limit, |stream| {
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
}

/// 判断 biliup 返回的错误是否属于 B 站 412 风控 / 上传限流：
/// - HTTP 412（UPOS 分片 PUT 被拒绝，body 通常是 HTML 风控页）
/// - preupload 返回 {"code":601,"message":"上传过快"}
/// - HTML 风控页被当作 JSON 解析时报错（Custom 里含 "<html" / "Request blocked"）
fn is_risk_control(kind: &Kind) -> bool {
    match kind {
        Kind::Reqwest(e) => matches!(e.status(), Some(StatusCode::PRECONDITION_FAILED | StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS)),
        Kind::RateLimit { code: 601, .. } => true,
        Kind::Custom(msg) => {
            let lower = msg.to_ascii_lowercase();
            lower.contains("412")
                || lower.contains("precondition failed")
                || lower.contains("request blocked")
                || lower.contains("<html")
                || lower.contains("风控")
                || lower.contains("上传过快")
        }
        _ => false,
    }
}

/// 把原始风控错误翻译成用户可读的提示，告诉用户可以怎么解决。
fn enrich_risk_error(kind: Kind, proxy: Option<&str>, auto_tried: bool) -> anyhow::Error {
    let mut msg = String::from("上传被 B 站风控拦截（412 / 上传过快）");
    if auto_tried {
        msg.push_str("；已尝试自动切换线路，全部被拦");
    }
    if proxy.is_some() {
        msg.push_str("；当前已使用 --proxy，如果这个 IP 也被标记请换代理");
    } else {
        msg.push_str("；建议：① 使用 --proxy 换 IP 再试，② 等待数小时让风控冷却，③ 换账号");
    }
    anyhow::anyhow!("{msg}\n原始错误: {kind}")
}

/// 构造候选上传线路池：首选用户指定的线路（或 probe 测速选最快），
/// 后续按固定顺序枚举其他静态线路，全部返回用于 412 自动换线路。
async fn build_line_pool(line_name: Option<&str>, bili: &BiliBili) -> Vec<Line> {
    let mut pool: Vec<Line> = Vec::new();
    let mut pushed = std::collections::HashSet::new();
    let push = |line: Line, pool: &mut Vec<Line>, pushed: &mut std::collections::HashSet<String>| {
        let key = line.query.clone();
        if pushed.insert(key) {
            pool.push(line);
        }
    };

    // 首选
    let primary = if let Some(name) = line_name {
        if name.eq_ignore_ascii_case("auto") {
            None
        } else {
            choose_line(Some(name))
        }
    } else {
        None
    };

    // probe 测速作为第二个首选（如果用户没指定具体线路）
    let probed = if line_name.is_none() || line_name.is_some_and(|n| n.eq_ignore_ascii_case("auto")) {
        Probe::probe(&bili.client).await.ok()
    } else {
        None
    };

    // 把首选放最前
    if let Some(line) = primary.or(probed) {
        let label = line.query.clone();
        tracing::info!(line = label.as_str(), "probe/用户指定");
        push(line, &mut pool, &mut pushed);
    }

    // 固定顺序填充其余候选：自建 DSA → 百度 → 腾讯 → 阿里云 → 新线路
    let static_lines: Vec<fn() -> Line> = vec![
        line::bldsa, line::cnbldsa, line::andsa, line::atdsa,
        line::bda2, line::cnbd, line::anbd, line::atbd,
        line::tx, line::cntx, line::antx, line::attx,
        line::txa, line::alia, line::estx, line::akbd,
    ];
    for factory in static_lines {
        push(factory(), &mut pool, &mut pushed);
    }
    pool
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
    let result = match option {
        SubmitOption::Web => bili.submit_by_web(studio, proxy).await,
        SubmitOption::BCutAndroid => bili.submit_by_bcut_android(studio, proxy).await,
        _ => bili.submit_by_app(studio, proxy).await,
    };
    result.map_err(|error| translate_submit_error(error, proxy))
}

async fn submit_edit(
    bili: &BiliBili,
    studio: &Studio,
    submit: SubmitArg,
    proxy: Option<&str>,
) -> Result<serde_json::Value> {
    let result = match submit_option(submit) {
        SubmitOption::App => bili.edit_by_app(studio, proxy).await,
        _ => bili.edit_by_web(studio).await,
    };
    result.map_err(|error| translate_submit_error(error, proxy))
}

/// submit 阶段的风控提示（UPOS 已经过了，这里是 member.bilibili.com 接口的风控）。
fn translate_submit_error(kind: Kind, proxy: Option<&str>) -> anyhow::Error {
    if is_risk_control(&kind) {
        let mut msg = String::from("提交稿件时被 B 站风控拦截");
        if proxy.is_some() {
            msg.push_str("（当前已使用 --proxy，可能该代理 IP 也被标记）");
        } else {
            msg.push_str("（member.bilibili.com 接口风控）");
        }
        msg.push_str("；建议使用 --proxy 换 IP 或等待数小时后重试。");
        return anyhow::anyhow!("{msg}\n原始错误: {kind}");
    }
    anyhow::anyhow!("{kind}")
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

#[cfg(test)]
mod tests {
    use super::{is_risk_control, parse_dtime};
    use biliup::error::Kind;
    use chrono::TimeZone;

    #[test]
    fn scheduled_time_is_interpreted_in_local_timezone() {
        let timestamp = parse_dtime("2026-10-25 20:00:00").unwrap() as i64;
        let local = chrono::Local
            .timestamp_opt(timestamp, 0)
            .single()
            .expect("valid local timestamp");
        assert_eq!(local.format("%Y-%m-%d %H:%M:%S").to_string(), "2026-10-25 20:00:00");
    }

    /// 用本地 HTTP 服务返回 412，拿到真实的 `reqwest::Error` 再判断。
    #[tokio::test]
    async fn detects_precondition_failed_as_risk() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                // 先读完请求头，再回包，避免未读数据触发 RST
                let mut buffer = vec![0u8; 4096];
                let mut received = Vec::new();
                while let Ok(n) = socket.read(&mut buffer).await {
                    if n == 0 {
                        return;
                    }
                    received.extend_from_slice(&buffer[..n]);
                    if received.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let _ = socket
                    .write_all(
                        b"HTTP/1.1 412 Precondition Failed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await;
            }
        });
        let error = reqwest::get(format!("http://{address}/"))
            .await
            .unwrap()
            .error_for_status()
            .unwrap_err();
        assert!(is_risk_control(&Kind::Reqwest(error)));
    }

    #[test]
    fn detects_rate_limit_code_601() {
        let kind = Kind::RateLimit {
            code: 601,
            message: "上传过快".into(),
        };
        assert!(is_risk_control(&kind));
    }

    #[test]
    fn detects_html_block_page_in_custom_error() {
        let kind = Kind::Custom(
            "<html><body>Request blocked by risk control</body></html>".to_string(),
        );
        assert!(is_risk_control(&kind));
    }

    #[test]
    fn normal_404_is_not_risk() {
        assert!(!is_risk_control(&Kind::Custom("not found".to_string())));
    }
}
