//! 视频下载命令：DASH 音视频下载、ffmpeg 合流、封面/字幕/弹幕联动。

use crate::cli::{CodecArg, DanmakuDownloadArgs, DownloadArgs, SubtitleArgs};
use crate::config::AppConfig;
use crate::output::Output;
use anyhow::{Context, Result, bail};
use futures::StreamExt;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::file::download_to;
use super::video::{
    self, Dash, MediaId, ViewData, convert_danmaku_xml_to_ass, pick_audio, pick_video,
    qn_from_label, qn_label,
};
use super::ApiClient;

#[derive(Clone)]
struct Settings {
    qn: u32,
    codec: u32,
    output_dir: PathBuf,
    template: String,
    with_cover: bool,
    with_subtitle: bool,
    with_danmaku: bool,
    danmaku_ass: bool,
    overwrite: bool,
    no_merge: bool,
    audio_only: bool,
    ffmpeg: String,
}

#[derive(Clone)]
struct Task {
    /// 用于展示的名字
    label: String,
    bvid: Option<String>,
    ep_id: Option<u64>,
    cid: u64,
    page: u32,
    part: String,
    title: String,
    pic: String,
    aid: u64,
}

pub async fn run_info(id: &str, cfg: &AppConfig, out: Output) -> Result<()> {
    let client = ApiClient::load(cfg).await?;
    let media = video::resolve_media(&client, id).await?;
    match &media {
        MediaId::Video(_) => {
            let view = video::view(&client, &media).await?;
            let value = serde_json::to_value(&view)?;
            out.emit(value.clone(), || human_video(&view));
        }
        _ => {
            let season = video::pgc_season(&client, &media).await?;
            let value = serde_json::to_value(&season)?;
            out.emit(value.clone(), || {
                let mut text = format!("{}\n", season.title);
                for episode in &season.episodes {
                    text.push_str(&format!(
                        "  ep{:<8} {} {}\n",
                        episode.id,
                        episode.long_title,
                        episode.bvid
                    ));
                }
                text
            });
        }
    }
    Ok(())
}

pub async fn run_cover(id: &str, output: Option<PathBuf>, cfg: &AppConfig, out: Output) -> Result<()> {
    let client = ApiClient::load(cfg).await?;
    let media = video::resolve_media(&client, id).await?;
    let (title, url) = match &media {
        MediaId::Video(_) => {
            let view = video::view(&client, &media).await?;
            (view.title.clone(), view.pic.clone())
        }
        MediaId::Episode(ep_id) => {
            let season = video::pgc_season(&client, &media).await?;
            let episode = season
                .episodes
                .iter()
                .find(|episode| episode.id == *ep_id)
                .with_context(|| format!("找不到 ep{ep_id}"))?;
            (
                format!("{}-{}", season.title, episode.long_title),
                if episode.cover.is_empty() {
                    season.cover.clone()
                } else {
                    episode.cover.clone()
                },
            )
        }
        MediaId::Season(_) => {
            let season = video::pgc_season(&client, &media).await?;
            (season.title.clone(), season.cover.clone())
        }
    };
    if url.is_empty() {
        bail!("该视频没有封面");
    }
    let path = output.unwrap_or_else(|| {
        cfg.download_dir
            .join(format!("{}.jpg", sanitize_component(&title)))
    });
    let size = video::download_cover(&client, &url, &path, true).await?;
    let path_text = path.display().to_string();
    out.emit(json!({"path": path, "bytes": size}), || {
        format!("封面已保存: {path_text} ({size} 字节)")
    });
    Ok(())
}

pub async fn run_subtitle(args: SubtitleArgs, cfg: &AppConfig, out: Output) -> Result<()> {
    let client = ApiClient::load(cfg).await?;
    let media = video::resolve_media(&client, &args.id).await?;
    let MediaId::Video(_) = &media else {
        bail!("字幕下载目前只支持普通视频");
    };
    let view = video::view(&client, &media).await?;
    let page = view
        .pages
        .iter()
        .find(|page| page.page == args.page as i32)
        .with_context(|| format!("找不到第 {} P", args.page))?;
    let tracks = video::subtitle_tracks(&client, &view.bvid, page.cid).await?;
    if tracks.is_empty() {
        bail!("该视频没有可用字幕（可能未登录或 UP 主未上传）");
    }
    let track = match &args.lang {
        Some(lang) => tracks
            .iter()
            .find(|track| &track.lan == lang || track.lan_doc.contains(lang))
            .with_context(|| format!("没有语言为 {lang} 的字幕"))?,
        None => &tracks[0],
    };
    let items = video::fetch_subtitle(&client, track).await?;
    let output_dir = args.output.unwrap_or_else(|| cfg.download_dir.clone());
    let base = sanitize_component(&view.title);
    let path = if args.raw {
        let path = output_dir.join(format!("{base}.{}.json", track.lan));
        std::fs::create_dir_all(&output_dir)?;
        std::fs::write(&path, serde_json::to_vec_pretty(&items)?)?;
        path
    } else {
        let path = output_dir.join(format!("{base}.{}.srt", track.lan));
        std::fs::create_dir_all(&output_dir)?;
        std::fs::write(&path, video::subtitle_to_srt(&items))?;
        path
    };
    let path_text = path.display().to_string();
    out.emit(json!({"path": path, "language": track.lan, "count": items.len()}), || {
        format!("字幕已保存: {path_text}（{} 条）", items.len())
    });
    Ok(())
}

pub async fn run_danmaku_download(
    args: DanmakuDownloadArgs,
    cfg: &AppConfig,
    out: Output,
) -> Result<()> {
    let client = ApiClient::load(cfg).await?;
    let media = video::resolve_media(&client, &args.id).await?;
    let MediaId::Video(_) = &media else {
        bail!("弹幕下载目前只支持普通视频");
    };
    let view = video::view(&client, &media).await?;
    let pages: Vec<_> = if args.all_pages {
        view.pages.clone()
    } else {
        view.pages
            .iter()
            .filter(|page| page.page == args.page as i32)
            .cloned()
            .collect()
    };
    if pages.is_empty() {
        bail!("找不到指定分 P");
    }
    let output_dir = args.output.unwrap_or_else(|| cfg.download_dir.clone());
    std::fs::create_dir_all(&output_dir)?;
    let mut files = Vec::new();
    for page in pages {
        let xml = video::danmaku_xml(&client, page.cid).await?;
        let base = if view.pages.len() > 1 {
            format!("{} P{} {}", sanitize_component(&view.title), page.page, sanitize_component(&page.part))
        } else {
            sanitize_component(&view.title)
        };
        let xml_path = output_dir.join(format!("{base}.xml"));
        std::fs::write(&xml_path, &xml)?;
        let mut entry = json!({"xml": xml_path, "cid": page.cid});
        if args.ass {
            let ass_path = output_dir.join(format!("{base}.ass"));
            convert_danmaku_xml_to_ass(&xml, &ass_path)?;
            entry["ass"] = json!(ass_path);
        }
        files.push(entry);
    }
    out.emit(json!({"files": &files}), || {
        files
            .iter()
            .flat_map(|file| {
                ["xml", "ass"]
                    .into_iter()
                    .filter_map(|key| file.get(key).and_then(|path| path.as_str()))
                    .map(|path| format!("已保存: {path}"))
            })
            .collect::<Vec<_>>()
            .join("\n")
    });
    Ok(())
}

pub async fn run_download(args: DownloadArgs, cfg: &AppConfig, out: Output) -> Result<()> {
    let client = ApiClient::load(cfg).await?;
    let qn = qn_from_label(&args.quality);
    let codec = match args.codec {
        CodecArg::Avc => 7,
        CodecArg::Hevc => 12,
        CodecArg::Av1 => 13,
    };
    let settings = Arc::new(Settings {
        qn,
        codec,
        output_dir: args.output.clone().unwrap_or_else(|| cfg.download_dir.clone()),
        template: args.name.clone().unwrap_or_else(|| cfg.filename.clone()),
        with_cover: args.with_cover,
        with_subtitle: args.with_subtitle,
        with_danmaku: args.with_danmaku,
        danmaku_ass: args.danmaku_ass,
        overwrite: args.overwrite,
        no_merge: args.no_merge,
        audio_only: args.audio_only,
        ffmpeg: cfg.ffmpeg.clone(),
    });

    let mut tasks = Vec::new();
    for id in &args.ids {
        let media = video::resolve_media(&client, id).await?;
        match &media {
            MediaId::Video(_) => {
                let view = video::view(&client, &media).await?;
                let pages = select_pages(&view, &args)?;
                for page in pages {
                    tasks.push(Task {
                        label: format!("{} P{}", view.title, page.page),
                        bvid: Some(view.bvid.clone()),
                        ep_id: None,
                        cid: page.cid,
                        page: page.page as u32,
                        part: page.part.clone(),
                        title: view.title.clone(),
                        pic: view.pic.clone(),
                        aid: view.aid,
                    });
                }
            }
            MediaId::Episode(_) | MediaId::Season(_) => {
                let season = video::pgc_season(&client, &media).await?;
                let episodes = match &media {
                    MediaId::Episode(ep) => season
                        .episodes
                        .iter()
                        .filter(|episode| episode.id == *ep)
                        .cloned()
                        .collect::<Vec<_>>(),
                    _ => season.episodes.clone(),
                };
                for (index, episode) in episodes.into_iter().enumerate() {
                    tasks.push(Task {
                        label: format!("{} {}", season.title, episode.long_title),
                        bvid: (!episode.bvid.is_empty()).then_some(episode.bvid.clone()),
                        ep_id: Some(episode.id),
                        cid: episode.cid,
                        page: (index + 1) as u32,
                        part: episode.long_title.clone(),
                        title: season.title.clone(),
                        pic: if episode.cover.is_empty() {
                            season.cover.clone()
                        } else {
                            episode.cover.clone()
                        },
                        aid: episode.aid,
                    });
                }
            }
        }
    }

    let jobs = args.jobs.max(1);
    let results = futures::stream::iter(tasks.into_iter().map(|task| {
        let client = client.clone();
        let settings = settings.clone();
        async move { download_one(&client, &settings, task).await }
    }))
    .buffer_unordered(jobs)
    .collect::<Vec<_>>()
    .await;

    let mut files = Vec::new();
    for result in results {
        files.push(result?);
    }
    out.emit(json!({"files": &files}), || {
        let mut text = String::new();
        for file in &files {
            if let Some(path) = file.get("output").and_then(Value::as_str) {
                text.push_str(&format!("完成: {path}\n"));
            }
        }
        text.trim_end().to_string()
    });
    Ok(())
}

fn select_pages<'a>(view: &'a ViewData, args: &DownloadArgs) -> Result<Vec<&'a video::Page>> {
    if args.all_pages {
        return Ok(view.pages.iter().collect());
    }
    let page_number = args.page.unwrap_or(1);
    let page = view
        .pages
        .iter()
        .find(|page| page.page == page_number as i32)
        .with_context(|| format!("找不到第 {page_number} P（共 {} P）", view.pages.len()))?;
    Ok(vec![page])
}

async fn download_one(client: &ApiClient, settings: &Settings, task: Task) -> Result<Value> {
    let play = match (task.bvid.as_deref(), task.ep_id) {
        (Some(bvid), None) => {
            video::playurl(client, bvid, task.cid, settings.qn).await?
        }
        (_, Some(ep_id)) => video::playurl_pgc(client, ep_id, task.cid, settings.qn).await?,
        _ => bail!("缺少播放参数"),
    };

    let base_path = render_path(settings, &task);
    let mut entry = json!({
        "label": task.label,
        "cid": task.cid,
        "page": task.page,
        "video": Value::Null,
        "audio": Value::Null,
        "output": Value::Null,
    });

    if let Some(dash) = play.dash.as_ref() {
        download_dash(client, settings, &task, dash, &base_path, &mut entry).await?;
    } else if let Some(durl) = play.durl.as_ref().filter(|items| !items.is_empty()) {
        download_durl(client, settings, &task, durl, &base_path, &mut entry).await?;
    } else {
        bail!("没有拿到可用的播放地址（可能需要大会员或登录）");
    }

    if settings.with_cover && !task.pic.is_empty() {
        let cover = base_path.with_extension("jpg");
        video::download_cover(client, &task.pic, &cover, settings.overwrite).await?;
        entry["cover"] = json!(cover);
    }
    if settings.with_subtitle {
        if let Some(bvid) = task.bvid.as_deref() {
            match video::subtitle_tracks(client, bvid, task.cid).await {
                Ok(tracks) if !tracks.is_empty() => {
                    let items = video::fetch_subtitle(client, &tracks[0]).await?;
                    let path = base_path.with_extension("srt");
                    std::fs::write(&path, video::subtitle_to_srt(&items))?;
                    entry["subtitle"] = json!(path);
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(%error, "字幕下载失败，跳过"),
            }
        }
    }
    if settings.with_danmaku {
        let xml = video::danmaku_xml(client, task.cid).await?;
        let xml_path = base_path.with_extension("xml");
        std::fs::write(&xml_path, &xml)?;
        entry["danmaku"] = json!(xml_path);
        if settings.danmaku_ass {
            let ass_path = base_path.with_extension("ass");
            convert_danmaku_xml_to_ass(&xml, &ass_path)?;
            entry["danmaku_ass"] = json!(ass_path);
        }
    }
    Ok(entry)
}

async fn download_dash(
    client: &ApiClient,
    settings: &Settings,
    task: &Task,
    dash: &Dash,
    base_path: &Path,
    entry: &mut Value,
) -> Result<()> {
    let media = client.media_client()?;
    let headers = client.media_headers();

    let video_stream = if settings.audio_only {
        None
    } else {
        Some(pick_video(dash, settings.qn, settings.codec).context("没有匹配的视频流")?)
    };
    let audio_stream = pick_audio(dash);

    if let Some(stream) = &video_stream {
        let quality = qn_label(stream.id);
        tracing::info!(
            label = %task.label,
            quality,
            codec = %stream.codecs,
            "开始下载视频"
        );
        let path = base_path.with_extension("video.m4s");
        download_with_backup(&media, &headers, &stream.urls(), &path, settings.overwrite, true).await?;
        entry["video"] = json!(path);
        entry["quality"] = json!(quality);
    }
    if let Some(stream) = &audio_stream {
        let path = base_path.with_extension("audio.m4s");
        download_with_backup(&media, &headers, &stream.urls(), &path, settings.overwrite, true).await?;
        entry["audio"] = json!(path);
    }

    if settings.audio_only {
        let audio = entry
            .get("audio")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .context("没有音频流")?;
        let output = base_path.with_extension("m4a");
        remux(settings, &audio, None, &output).await?;
        let _ = std::fs::remove_file(&audio);
        entry["output"] = json!(output);
        return Ok(());
    }

    let video = entry
        .get("video")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .context("没有视频流")?;
    let audio = entry.get("audio").and_then(Value::as_str).map(PathBuf::from);

    if settings.no_merge {
        entry["output"] = json!(video);
        return Ok(());
    }

    let output = base_path.with_extension("mp4");
    remux(settings, &video, audio.as_deref(), &output).await?;
    let _ = std::fs::remove_file(&video);
    if let Some(audio) = &audio {
        let _ = std::fs::remove_file(audio);
    }
    entry["output"] = json!(output);
    Ok(())
}

async fn download_durl(
    client: &ApiClient,
    settings: &Settings,
    _task: &Task,
    durl: &[video::Durl],
    base_path: &Path,
    entry: &mut Value,
) -> Result<()> {
    let media = client.media_client()?;
    let headers = client.media_headers();
    if durl.len() == 1 {
        let ext = if durl[0].url.contains(".flv") { "flv" } else { "mp4" };
        let output = base_path.with_extension(ext);
        download_to(
            &media,
            &headers,
            &durl[0].url,
            &output,
            settings.overwrite,
            true,
        )
        .await?;
        entry["output"] = json!(output);
        return Ok(());
    }

    let mut parts = Vec::new();
    for (index, part) in durl.iter().enumerate() {
        let path = base_path.with_extension(format!("part{index}.flv"));
        download_to(&media, &headers, &part.url, &path, settings.overwrite, true).await?;
        parts.push(path);
    }
    let list = base_path.with_extension("concat.txt");
    let text = parts
        .iter()
        .map(|path| format!("file '{}'", path.display()))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&list, text)?;
    let output = base_path.with_extension("mp4");
    let status = tokio::process::Command::new(&settings.ffmpeg)
        .arg("-y")
        .arg("-loglevel")
        .arg("error")
        .arg("-f")
        .arg("concat")
        .arg("-safe")
        .arg("0")
        .arg("-i")
        .arg(&list)
        .arg("-c")
        .arg("copy")
        .arg(&output)
        .status()
        .await
        .context("调用 ffmpeg 失败（请确认已安装 ffmpeg）")?;
    if !status.success() {
        bail!("ffmpeg 合流失败");
    }
    for path in parts {
        let _ = std::fs::remove_file(path);
    }
    let _ = std::fs::remove_file(list);
    entry["output"] = json!(output);
    Ok(())
}

async fn download_with_backup(
    client: &reqwest::Client,
    headers: &[(reqwest::header::HeaderName, String)],
    urls: &[String],
    path: &Path,
    overwrite: bool,
    progress: bool,
) -> Result<u64> {
    let mut last_error = None;
    for url in urls {
        match download_to(client, headers, url, path, overwrite, progress).await {
            Ok(size) => return Ok(size),
            Err(error) => {
                tracing::warn!(%error, "下载失败，尝试备用地址");
                last_error = Some(error);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("没有可用的下载地址")))
}

async fn remux(settings: &Settings, video: &Path, audio: Option<&Path>, output: &Path) -> Result<()> {
    let mut command = tokio::process::Command::new(&settings.ffmpeg);
    command
        .arg("-y")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(video);
    if let Some(audio) = audio {
        command.arg("-i").arg(audio);
        command
            .arg("-map")
            .arg("0:v:0")
            .arg("-map")
            .arg("1:a:0")
            .arg("-c")
            .arg("copy")
            .arg("-shortest");
    } else {
        command.arg("-c").arg("copy");
    }
    command.arg("-movflags").arg("+faststart").arg(output);
    let result = command
        .output()
        .await
        .context("调用 ffmpeg 失败（请确认已安装 ffmpeg，或用 --no-merge 跳过合流）")?;
    if !result.status.success() {
        bail!(
            "ffmpeg 合流失败: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        );
    }
    Ok(())
}

fn render_path(settings: &Settings, task: &Task) -> PathBuf {
    let quality = qn_label(settings.qn);
    let mut name = settings.template.clone();
    name = name.replace("{title}", &task.title);
    name = name.replace("{bvid}", task.bvid.as_deref().unwrap_or(""));
    name = name.replace("{aid}", &task.aid.to_string());
    name = name.replace("{p}", &task.page.to_string());
    name = name.replace("{part}", &task.part);
    name = name.replace("{cid}", &task.cid.to_string());
    name = name.replace("{quality}", quality);
    name = name.replace("{ext}", "");

    let mut path = settings.output_dir.clone();
    for component in name.split(['/', '\\']) {
        let component = sanitize_component(component);
        if component.is_empty() || component == "." {
            continue;
        }
        if component == ".." {
            continue;
        }
        path.push(component);
    }
    if path == settings.output_dir {
        path.push(sanitize_component(&task.title));
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    path
}

fn sanitize_component(value: &str) -> String {
    let mut text: String = value
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    text = text.trim().trim_matches('.').to_string();
    if text.chars().count() > 120 {
        text = text.chars().take(120).collect();
    }
    text
}

fn human_video(view: &ViewData) -> String {
    let mut text = String::new();
    text.push_str(&format!("{} ({})\n", view.title, view.bvid));
    text.push_str(&format!(
        "UP主: {}  时长: {}  分P: {}\n",
        view.owner.name,
        format_duration(view.duration),
        view.videos
    ));
    text.push_str(&format!("分区: {}  发布: {}\n", view.tname, view.pubdate));
    text.push_str(&format!(
        "播放 {}  弹幕 {}  点赞 {}\n",
        view.stat.get("view").and_then(Value::as_u64).unwrap_or(0),
        view.stat.get("danmaku").and_then(Value::as_u64).unwrap_or(0),
        view.stat.get("like").and_then(Value::as_u64).unwrap_or(0)
    ));
    text.push_str(&format!("封面: {}\n", view.pic));
    if view.videos > 1 {
        for page in &view.pages {
            text.push_str(&format!("  P{} {} (cid={})\n", page.page, page.part, page.cid));
        }
    }
    text
}

pub fn format_duration(seconds: u64) -> String {
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let secs = seconds % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes}:{secs:02}")
    }
}
