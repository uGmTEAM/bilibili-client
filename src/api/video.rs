//! 视频信息、播放地址、字幕、弹幕与番剧。

use anyhow::{Context, Result, bail};
use biliup::uploader::bilibili::Vid;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::str::FromStr;

use super::ApiClient;
use super::file::download_to;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaId {
    Video(Vid),
    /// 番剧单集
    Episode(u64),
    /// 番剧季度（整季）
    Season(u64),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Owner {
    pub mid: i64,
    pub name: String,
    #[serde(default)]
    pub face: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Page {
    pub cid: u64,
    pub page: i32,
    #[serde(default)]
    pub part: String,
    #[serde(default)]
    pub duration: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ViewData {
    pub bvid: String,
    pub aid: u64,
    #[serde(default)]
    pub videos: u32,
    pub title: String,
    #[serde(default)]
    pub pic: String,
    #[serde(default)]
    pub desc: String,
    #[serde(default)]
    pub pubdate: i64,
    #[serde(default)]
    pub duration: u64,
    #[serde(default)]
    pub tname: String,
    #[serde(default)]
    pub copyright: u8,
    pub owner: Owner,
    #[serde(default)]
    pub stat: Value,
    #[serde(default)]
    pub pages: Vec<Page>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PgcEpisode {
    pub id: u64,
    #[serde(default)]
    pub aid: u64,
    #[serde(default)]
    pub bvid: String,
    #[serde(default)]
    pub cid: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub long_title: String,
    #[serde(default)]
    pub duration: u64,
    #[serde(default)]
    pub cover: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PgcSeason {
    #[serde(default)]
    pub season_id: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub cover: String,
    #[serde(default)]
    pub episodes: Vec<PgcEpisode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "DashStreamWire")]
pub struct DashStream {
    pub id: u32,
    pub base_url: String,
    pub backup_url: Vec<String>,
    pub bandwidth: u64,
    pub codecid: u32,
    pub width: u32,
    pub height: u32,
    pub codecs: String,
    pub mime_type: String,
}

/// B 站播放接口会同时返回 camelCase 与 snake_case 字段，先分别接收再合并，
/// 避免 serde 把两者都当成同一个字段而报 duplicate field。
#[derive(Debug, Clone, Deserialize)]
struct DashStreamWire {
    #[serde(default)]
    id: u32,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default, rename = "baseUrl")]
    base_url_camel: Option<String>,
    #[serde(default)]
    backup_url: Option<Vec<String>>,
    #[serde(default, rename = "backupUrl")]
    backup_url_camel: Option<Vec<String>>,
    #[serde(default)]
    bandwidth: u64,
    #[serde(default)]
    codecid: u32,
    #[serde(default)]
    width: u32,
    #[serde(default)]
    height: u32,
    #[serde(default)]
    codecs: String,
    #[serde(default)]
    mime_type: Option<String>,
    #[serde(default, rename = "mimeType")]
    mime_type_camel: Option<String>,
}

impl From<DashStreamWire> for DashStream {
    fn from(wire: DashStreamWire) -> Self {
        let base_url = wire
            .base_url
            .filter(|value| !value.is_empty())
            .or(wire.base_url_camel)
            .unwrap_or_default();
        let mut backup_url = wire.backup_url.unwrap_or_default();
        if let Some(camel) = wire.backup_url_camel {
            for url in camel {
                if !backup_url.contains(&url) {
                    backup_url.push(url);
                }
            }
        }
        let mime_type = wire
            .mime_type
            .filter(|value| !value.is_empty())
            .or(wire.mime_type_camel)
            .unwrap_or_default();
        DashStream {
            id: wire.id,
            base_url,
            backup_url,
            bandwidth: wire.bandwidth,
            codecid: wire.codecid,
            width: wire.width,
            height: wire.height,
            codecs: wire.codecs,
            mime_type,
        }
    }
}

impl DashStream {
    pub fn urls(&self) -> Vec<String> {
        let mut urls = Vec::new();
        if !self.base_url.is_empty() {
            urls.push(normalize_url(&self.base_url));
        }
        urls.extend(self.backup_url.iter().map(|url| normalize_url(url)));
        urls
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Dash {
    #[serde(default)]
    pub duration: u64,
    #[serde(default)]
    pub video: Vec<DashStream>,
    #[serde(default)]
    pub audio: Option<Vec<DashStream>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Durl {
    pub url: String,
    #[serde(default)]
    pub length: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct PlayUrl {
    #[serde(default)]
    pub quality: u32,
    #[serde(default)]
    pub dash: Option<Dash>,
    #[serde(default)]
    pub durl: Option<Vec<Durl>>,
    #[serde(default)]
    pub accept_quality: Vec<u32>,
    #[serde(default)]
    pub accept_description: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SubtitleTrack {
    pub lan: String,
    pub lan_doc: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtitleItem {
    pub from: f64,
    pub to: f64,
    pub content: String,
}

pub async fn resolve_media(client: &ApiClient, input: &str) -> Result<MediaId> {
    let mut text = input.trim().to_string();
    if text.contains("b23.tv") || text.contains("bili2233.cn") {
        let response = client.get_raw(&text).await?;
        text = response.url().to_string();
    }
    parse_media(&text)
}

fn parse_media(input: &str) -> Result<MediaId> {
    let text = input.trim();

    if let Some(index) = text.find("/bangumi/play/ep") {
        return Ok(MediaId::Episode(parse_tail_number(&text[index + 16..])?));
    }
    if let Some(index) = text.find("/bangumi/play/ss") {
        return Ok(MediaId::Season(parse_tail_number(&text[index + 16..])?));
    }
    if let Some(index) = text.find("/video/") {
        let rest = &text[index + 7..];
        let token = rest
            .split(['/', '?', '#'])
            .next()
            .unwrap_or(rest)
            .trim();
        return parse_vid(token);
    }

    let lower = text.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("ep") {
        if rest.chars().all(|c| c.is_ascii_digit()) && !rest.is_empty() {
            return Ok(MediaId::Episode(rest.parse()?));
        }
    }
    if let Some(rest) = lower.strip_prefix("ss") {
        if rest.chars().all(|c| c.is_ascii_digit()) && !rest.is_empty() {
            return Ok(MediaId::Season(rest.parse()?));
        }
    }
    parse_vid(text)
}

fn parse_vid(token: &str) -> Result<MediaId> {
    Vid::from_str(token)
        .map(MediaId::Video)
        .map_err(|_| anyhow::anyhow!("无法识别的视频 ID: {token}"))
}

fn parse_tail_number(tail: &str) -> Result<u64> {
    let digits: String = tail
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        bail!("链接里没有有效的集数/季数");
    }
    Ok(digits.parse()?)
}

pub async fn view(client: &ApiClient, id: &MediaId) -> Result<ViewData> {
    let MediaId::Video(vid) = id else {
        bail!("不是普通视频");
    };
    let query = match vid {
        Vid::Aid(aid) => vec![("aid", aid.to_string())],
        Vid::Bvid(bvid) => vec![("bvid", bvid.clone())],
    };
    let value = client
        .get_value("https://api.bilibili.com/x/web-interface/view", &query)
        .await?;
    Ok(serde_json::from_value(value).context("解析视频信息失败")?)
}

pub async fn pgc_season(client: &ApiClient, id: &MediaId) -> Result<PgcSeason> {
    let query = match id {
        MediaId::Episode(ep) => vec![("ep_id", ep.to_string())],
        MediaId::Season(ss) => vec![("season_id", ss.to_string())],
        MediaId::Video(_) => bail!("不是番剧"),
    };
    let envelope = client
        .get_envelope("https://api.bilibili.com/pgc/view/web/season", &query)
        .await?;
    let data = envelope
        .get("result")
        .or_else(|| envelope.get("data"))
        .context("番剧接口没有返回 result")?
        .clone();
    let season: PgcSeason = serde_json::from_value(data).context("解析番剧信息失败")?;
    if season.episodes.is_empty() {
        bail!("没有找到可播放的剧集");
    }
    Ok(season)
}

pub fn qn_from_label(label: &str) -> u32 {
    match label.trim().to_ascii_lowercase().as_str() {
        "8k" => 127,
        "dolby" | "dolbyvision" => 126,
        "hdr" => 125,
        "4k" | "2160p" => 120,
        "1080p60" | "1080p60fps" => 116,
        "1080p+" | "1080p_plus" => 112,
        "1080p" | "fhd" => 80,
        "720p60" => 74,
        "720p" | "hd" => 64,
        "480p" | "sd" => 32,
        "360p" | "ld" => 16,
        other => other.parse::<u32>().unwrap_or(80),
    }
}

pub fn qn_label(qn: u32) -> &'static str {
    match qn {
        127 => "8K",
        126 => "杜比视界",
        125 => "HDR",
        120 => "4K",
        116 => "1080P60",
        112 => "1080P+",
        80 => "1080P",
        74 => "720P60",
        64 => "720P",
        32 => "480P",
        16 => "360P",
        _ => "未知",
    }
}

pub async fn playurl(client: &ApiClient, bvid: &str, cid: u64, qn: u32) -> Result<PlayUrl> {
    let params = vec![
        ("bvid".to_string(), bvid.to_string()),
        ("cid".to_string(), cid.to_string()),
        ("qn".to_string(), qn.to_string()),
        ("fnval".to_string(), "4048".to_string()),
        ("fnver".to_string(), "0".to_string()),
        ("fourk".to_string(), "1".to_string()),
        ("platform".to_string(), "pc".to_string()),
        ("high_quality".to_string(), "1".to_string()),
    ];
    let data = match client
        .get_value_signed("https://api.bilibili.com/x/player/wbi/playurl", params.clone())
        .await
    {
        Ok(data) => data,
        Err(signed_error) => {
            let query: Vec<(&str, String)> = params
                .iter()
                .map(|(key, value)| (key.as_str(), value.clone()))
                .collect();
            client
                .get_value("https://api.bilibili.com/x/player/playurl", &query)
                .await
                .with_context(|| format!("获取播放地址失败（WBI 接口：{signed_error}）"))?
        }
    };
    serde_json::from_value(data).context("解析播放地址失败")
}

pub async fn playurl_pgc(client: &ApiClient, ep_id: u64, cid: u64, qn: u32) -> Result<PlayUrl> {
    let params = vec![
        ("ep_id".to_string(), ep_id.to_string()),
        ("cid".to_string(), cid.to_string()),
        ("qn".to_string(), qn.to_string()),
        ("fnval".to_string(), "4048".to_string()),
        ("fnver".to_string(), "0".to_string()),
        ("fourk".to_string(), "1".to_string()),
    ];
    let envelope = match client
        .get_envelope(
            "https://api.bilibili.com/pgc/player/web/playurl",
            &params
                .iter()
                .map(|(key, value)| (key.as_str(), value.clone()))
                .collect::<Vec<_>>(),
        )
        .await
    {
        Ok(value) => value,
        Err(_) => Value::Null,
    };
    let mut data = envelope
        .get("result")
        .or_else(|| envelope.get("data"))
        .cloned()
        .unwrap_or(Value::Null);
    if data.is_null() {
        let signed = client
            .get_envelope_signed("https://api.bilibili.com/pgc/player/web/playurl", params)
            .await
            .context("获取番剧播放地址失败（可能需要大会员）")?;
        data = signed
            .get("result")
            .or_else(|| signed.get("data"))
            .cloned()
            .unwrap_or(Value::Null);
    }
    serde_json::from_value(data).context("解析番剧播放地址失败")
}

pub fn pick_video(dash: &Dash, qn: u32, codec: u32) -> Option<DashStream> {
    let by_codec: Vec<&DashStream> = dash
        .video
        .iter()
        .filter(|stream| codec == 0 || stream.codecid == codec)
        .collect();
    let pool: Vec<&DashStream> = if by_codec.is_empty() {
        dash.video.iter().collect()
    } else {
        by_codec
    };
    if pool.is_empty() {
        return None;
    }
    if let Some(exact) = pool.iter().find(|stream| stream.id == qn) {
        return Some((*exact).clone());
    }
    if let Some(lower) = pool
        .iter()
        .filter(|stream| stream.id < qn)
        .max_by_key(|stream| stream.id)
    {
        return Some((*lower).clone());
    }
    pool.iter()
        .max_by_key(|stream| stream.id)
        .map(|stream| (*stream).clone())
}

pub fn pick_audio(dash: &Dash) -> Option<DashStream> {
    let audio = dash.audio.as_ref()?;
    let rank = |id: u32| match id {
        30251 => 5, // Hi-Res
        30250 => 4, // 杜比
        30280 => 3, // 192K
        30232 => 2, // 132K
        30216 => 1, // 64K
        _ => 0,
    };
    audio
        .iter()
        .max_by_key(|stream| (rank(stream.id), stream.bandwidth))
        .cloned()
}

pub async fn subtitle_tracks(client: &ApiClient, bvid: &str, cid: u64) -> Result<Vec<SubtitleTrack>> {
    let params = vec![
        ("bvid".to_string(), bvid.to_string()),
        ("cid".to_string(), cid.to_string()),
    ];
    let data = client
        .get_value_signed("https://api.bilibili.com/x/player/wbi/v2", params)
        .await?;
    let mut tracks = Vec::new();
    if let Some(items) = data.pointer("/subtitle/subtitles").and_then(Value::as_array) {
        for item in items {
            let url = item
                .get("subtitle_url")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if url.is_empty() {
                continue;
            }
            tracks.push(SubtitleTrack {
                lan: item
                    .get("lan")
                    .and_then(Value::as_str)
                    .unwrap_or("-")
                    .to_string(),
                lan_doc: item
                    .get("lan_doc")
                    .and_then(Value::as_str)
                    .unwrap_or("-")
                    .to_string(),
                url: normalize_url(&url),
            });
        }
    }
    Ok(tracks)
}

pub async fn fetch_subtitle(client: &ApiClient, track: &SubtitleTrack) -> Result<Vec<SubtitleItem>> {
    let response = client.get_raw(&track.url).await?;
    let raw: Value = response.json().await.context("解析字幕响应失败")?;
    let body = raw.get("body").cloned().unwrap_or(raw);
    serde_json::from_value(body).context("解析字幕内容失败")
}

pub fn subtitle_to_srt(items: &[SubtitleItem]) -> String {
    let mut output = String::new();
    for (index, item) in items.iter().enumerate() {
        output.push_str(&format!("{}\n", index + 1));
        output.push_str(&format!(
            "{} --> {}\n{}\n\n",
            srt_time(item.from),
            srt_time(item.to),
            item.content.replace('\n', " ")
        ));
    }
    output
}

fn srt_time(seconds: f64) -> String {
    let total_ms = (seconds.max(0.0) * 1000.0).round() as u64;
    let hours = total_ms / 3_600_000;
    let minutes = (total_ms % 3_600_000) / 60_000;
    let secs = (total_ms % 60_000) / 1000;
    let millis = total_ms % 1000;
    format!("{hours:02}:{minutes:02}:{secs:02},{millis:03}")
}

pub async fn danmaku_xml(client: &ApiClient, cid: u64) -> Result<String> {
    let url = format!("https://api.bilibili.com/x/v1/dm/list.so?oid={cid}");
    let response = client.get_raw(&url).await?;
    let bytes = response.bytes().await?;
    decode_danmaku(&bytes)
}

fn decode_danmaku(bytes: &[u8]) -> Result<String> {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return Ok(String::from_utf8_lossy(&bytes[3..]).to_string());
    }
    if bytes.first() == Some(&b'<') {
        return Ok(String::from_utf8_lossy(bytes).to_string());
    }
    use std::io::Read;
    for decoder in ["zlib", "deflate", "gzip"] {
        let mut output = Vec::new();
        let result = match decoder {
            "zlib" => flate2::read::ZlibDecoder::new(bytes).read_to_end(&mut output),
            "deflate" => flate2::read::DeflateDecoder::new(bytes).read_to_end(&mut output),
            _ => flate2::read::GzDecoder::new(bytes).read_to_end(&mut output),
        };
        if result.is_ok() {
            if let Ok(text) = String::from_utf8(output) {
                if text.contains("<d ") || text.contains("<?xml") {
                    return Ok(text);
                }
            }
        }
    }
    bail!("无法解压弹幕数据（{} 字节）", bytes.len())
}

pub fn convert_danmaku_xml_to_ass(xml: &str, output: &Path) -> Result<()> {
    let temporary = tempfile::NamedTempFile::new()?;
    std::fs::write(temporary.path(), xml.as_bytes())?;
    if output.exists() {
        std::fs::remove_file(output)?;
    }
    danmaku_factory::convert_xml_to_ass(
        temporary.path(),
        output,
        &danmaku_factory::ConvertOptions::default(),
    )
    .map_err(|error| anyhow::anyhow!("弹幕转 ASS 失败: {error}"))?;
    Ok(())
}

pub async fn download_cover(
    client: &ApiClient,
    url: &str,
    output: &Path,
    overwrite: bool,
) -> Result<u64> {
    let media = client.media_client()?;
    download_to(
        &media,
        &client.media_headers(),
        &normalize_url(url),
        output,
        overwrite,
        false,
    )
    .await
}

fn normalize_url(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("//") {
        format!("https://{rest}")
    } else {
        url.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{MediaId, parse_media, qn_from_label};
    use biliup::uploader::bilibili::Vid;

    #[test]
    fn parses_bvid_av_url_and_bangumi_ids() {
        assert_eq!(
            parse_media("BV1xx411c7mD").unwrap(),
            MediaId::Video(Vid::Bvid("BV1xx411c7mD".into()))
        );
        assert_eq!(
            parse_media("av123").unwrap(),
            MediaId::Video(Vid::Aid(123))
        );
        assert_eq!(
            parse_media("https://www.bilibili.com/video/BV1xx411c7mD?p=2").unwrap(),
            MediaId::Video(Vid::Bvid("BV1xx411c7mD".into()))
        );
        assert_eq!(
            parse_media("https://www.bilibili.com/bangumi/play/ep123456").unwrap(),
            MediaId::Episode(123456)
        );
        assert_eq!(parse_media("ss789").unwrap(), MediaId::Season(789));
    }

    #[test]
    fn quality_labels_map_to_bilibili_qn() {
        assert_eq!(qn_from_label("1080p"), 80);
        assert_eq!(qn_from_label("4k"), 120);
        assert_eq!(qn_from_label("80"), 80);
        assert_eq!(qn_from_label("unknown"), 80);
    }

    #[test]
    fn dash_streams_merge_camel_and_snake_case_fields() {
        let stream: super::DashStream = serde_json::from_value(serde_json::json!({
            "id": 80,
            "baseUrl": "https://camel/video.m4s",
            "base_url": "https://snake/video.m4s",
            "backupUrl": ["https://camel/backup.m4s"],
            "backup_url": ["https://snake/backup.m4s"],
            "codecid": 7,
            "codecs": "avc1.64001f",
            "mimeType": "video/mp4",
            "mime_type": "video/mp4"
        }))
        .unwrap();
        assert_eq!(stream.base_url, "https://snake/video.m4s");
        assert_eq!(
            stream.backup_url,
            vec![
                "https://snake/backup.m4s".to_string(),
                "https://camel/backup.m4s".to_string()
            ]
        );
        assert_eq!(stream.mime_type, "video/mp4");
    }
}
