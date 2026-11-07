//! 直播：房间信息、录制（含弹幕）与直播弹幕发送。

use crate::cli::{LiveAction, LiveDanmakuArgs, LiveRecordArgs};
use crate::config::AppConfig;
use crate::output::Output;
use anyhow::{Context, Result, bail};
use biliup::client::StatelessClient;
use biliup::downloader::httpflv::Connection;
use biliup::downloader::live::{
    LiveCredentials, LiveOptions, LiveRequest, LiveStatus, LiveStream, builtin_plugins,
};
use biliup::downloader::util::{LifecycleFile, Segmentable};
use biliup::downloader::{hls, httpflv};
use futures::{SinkExt, StreamExt};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

use super::ApiClient;

pub async fn run(action: LiveAction, cfg: &AppConfig, out: Output) -> Result<()> {
    let client = ApiClient::load(cfg).await?;
    match action {
        LiveAction::Info { room } => info(&client, &room, out).await,
        LiveAction::Record(args) => record(args, &client, cfg, out).await,
        LiveAction::Danmaku(args) => danmaku_only(args, &client, cfg, out).await,
        LiveAction::Send {
            room,
            message,
            color,
        } => send(&client, &room, &message, &color, out).await,
    }
}

async fn info(client: &ApiClient, room: &str, out: Output) -> Result<()> {
    let stream = fetch_stream(client, room).await?;
    let value = serde_json::to_value(&stream)?;
    out.emit(value, || {
        format!(
            "{} - {}\n平台: {}\n主播: {}\n开播: {}\n拉流: {}\n封面: {}",
            stream.title,
            stream.name,
            stream.platform,
            stream.name,
            stream.date,
            stream.raw_stream_url,
            stream.live_cover_url
        )
    });
    Ok(())
}

async fn record(args: LiveRecordArgs, client: &ApiClient, cfg: &AppConfig, out: Output) -> Result<()> {
    let stream = fetch_stream(client, &args.room).await?;
    let room_id = room_id(&args.room)?;
    let output_dir = args.output.unwrap_or_else(|| cfg.download_dir.clone());
    std::fs::create_dir_all(&output_dir)?;

    let template = args
        .name
        .unwrap_or_else(|| "{title}-{platform}-%Y%m%d-%H%M%S".to_string())
        .replace("{title}", &sanitize(&stream.title))
        .replace("{platform}", &sanitize(&stream.platform))
        .replace("{room}", &room_id.to_string());
    let base = output_dir.join(sanitize(&template));

    let ext = if stream.suffix.contains("m3u8") || stream.suffix.contains("ts") || stream.raw_stream_url.contains(".m3u8") {
        "ts"
    } else {
        "flv"
    };

    out.info(format!(
        "开始录制: {} ({})\n输出: {}",
        stream.title,
        stream.name,
        base.with_extension(ext).display()
    ));

    let danmaku_path = base.with_extension("xml");
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let danmaku_task = if args.danmaku {
        let client = client.clone();
        let path = danmaku_path.clone();
        Some(tokio::spawn(async move {
            record_danmaku(client, room_id, &path, None, Some(stop_rx)).await
        }))
    } else {
        None
    };

    let duration = match &args.duration {
        Some(text) => Some(humantime::parse_duration(text).context("无法解析录制时长")?),
        None => None,
    };

    let record_base = base.to_string_lossy().to_string();
    let headers = stream_headers(&stream, client);
    let url = stream.raw_stream_url.clone();
    let recording = record_stream(&record_base, ext, &url, headers);
    let result = match duration {
        Some(duration) => {
            tokio::select! {
                result = recording => result,
                _ = tokio::time::sleep(duration) => {
                    out.info("达到录制时长，停止录制");
                    Ok(())
                }
                _ = tokio::signal::ctrl_c() => {
                    out.info("收到 Ctrl+C，停止录制");
                    Ok(())
                }
            }
        }
        None => {
            tokio::select! {
                result = recording => result,
                _ = tokio::signal::ctrl_c() => {
                    out.info("收到 Ctrl+C，停止录制");
                    Ok(())
                }
            }
        }
    };

    // 被中途打断时 LifecycleFile 可能来不及改名，这里收尾。
    finalize_part(&base.with_extension(ext));

    let _ = stop_tx.send(());
    if let Some(task) = danmaku_task {
        let _ = task.await;
    }

    result?;
    let final_path = base.with_extension(ext);
    let final_text = final_path.display().to_string();
    let title = stream.title.clone();
    out.emit(
        json!({
            "output": final_path,
            "danmaku": args.danmaku.then_some(danmaku_path),
            "title": title,
        }),
        || format!("录制结束: {final_text}"),
    );
    Ok(())
}

async fn danmaku_only(args: LiveDanmakuArgs, client: &ApiClient, cfg: &AppConfig, out: Output) -> Result<()> {
    let room_id = room_id(&args.room)?;
    let output_dir = args.output.unwrap_or_else(|| cfg.download_dir.clone());
    std::fs::create_dir_all(&output_dir)?;
    let path = output_dir.join(format!("danmaku-{room_id}-%Y%m%d-%H%M%S.xml"));
    let path = PathBuf::from(biliup::downloader::util::format_filename(
        &path.to_string_lossy(),
    ));
    let duration = match &args.duration {
        Some(text) => Some(humantime::parse_duration(text).context("无法解析录制时长")?),
        None => None,
    };
    out.info(format!("开始录制弹幕: {}", path.display()));
    record_danmaku(client.clone(), room_id, &path, duration, None).await?;
    let path_text = path.display().to_string();
    out.emit(json!({"output": path}), || {
        format!("弹幕已保存: {path_text}")
    });
    Ok(())
}

async fn send(
    client: &ApiClient,
    room: &str,
    message: &str,
    color: &str,
    out: Output,
) -> Result<()> {
    client.require_login()?;
    let room_id = room_id(room)?;
    let color = super::danmaku::parse_color(color)?;
    let csrf = client.csrf()?.to_string();
    let form = vec![
        ("bubble".to_string(), "0".to_string()),
        ("msg".to_string(), message.to_string()),
        ("color".to_string(), color.to_string()),
        ("mode".to_string(), "1".to_string()),
        ("fontsize".to_string(), "25".to_string()),
        ("rnd".to_string(), chrono::Utc::now().timestamp().to_string()),
        ("roomid".to_string(), room_id.to_string()),
        ("csrf".to_string(), csrf.clone()),
        ("csrf_token".to_string(), csrf),
    ];
    let data = client
        .post_form("https://api.live.bilibili.com/msg/send", &form)
        .await?;
    out.emit(json!({"room_id": room_id, "sent": true, "data": data}), || {
        format!("直播弹幕已发送到房间 {room_id}（接口为实验性，若失败请检查登录态）")
    });
    Ok(())
}

async fn fetch_stream(client: &ApiClient, room: &str) -> Result<LiveStream> {
    let room_id = room_id(room)?;
    let url = format!("https://live.bilibili.com/{room_id}");
    let plugins = builtin_plugins();
    let plugin = plugins
        .iter()
        .find(|plugin| plugin.matches(&url))
        .context("没有匹配的直播解析器")?
        .clone();
    let request = LiveRequest {
        client: client.http.clone(),
        url,
        name: room.to_string(),
        options: LiveOptions::default(),
        credentials: LiveCredentials {
            bilibili_cookie: (!client.cookie_header.is_empty()).then(|| client.cookie_header.clone()),
            ..Default::default()
        },
    };
    match plugin
        .check_stream(request)
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?
    {
        LiveStatus::Live { stream } => Ok(*stream),
        LiveStatus::Offline => bail!("直播间 {room_id} 当前未开播"),
    }
}

async fn record_stream(base: &str, ext: &'static str, url: &str, headers: HeaderMap) -> Result<()> {
    let client = StatelessClient::new(headers, None);
    let segmentable = Segmentable::new(None, None);
    if ext == "ts" || url.contains(".m3u8") {
        let file = LifecycleFile::new(base, ext);
        hls::download(url, &client, file, segmentable, None)
            .await
            .map_err(|error| anyhow::anyhow!("HLS 录制失败: {error:#}"))?;
    } else {
        let response = client
            .retryable(url)
            .await
            .context("连接直播流失败")?;
        let mut connection = Connection::new(response);
        connection
            .read_frame(9)
            .await
            .map_err(|error| anyhow::anyhow!("读取 FLV 头失败: {error}"))?;
        let file = LifecycleFile::new(base, ext);
        httpflv::download(connection, file, segmentable, None).await;
    }
    Ok(())
}

fn stream_headers(stream: &LiveStream, client: &ApiClient) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in &stream.stream_headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            headers.insert(name, value);
        }
    }
    headers.insert(
        reqwest::header::USER_AGENT,
        HeaderValue::from_str(&client.user_agent).unwrap_or_else(|_| HeaderValue::from_static("Mozilla/5.0")),
    );
    headers.insert(
        reqwest::header::REFERER,
        HeaderValue::from_static("https://live.bilibili.com/"),
    );
    if let Ok(value) = HeaderValue::from_str(&client.cookie_header) {
        headers.insert(reqwest::header::COOKIE, value);
    }
    headers
}

fn finalize_part(path: &Path) {
    let part = path.with_extension(format!(
        "{}.part",
        path.extension().and_then(|ext| ext.to_str()).unwrap_or("flv")
    ));
    if part.exists() {
        let _ = std::fs::rename(part, path);
    }
}

fn room_id(input: &str) -> Result<u64> {
    let text = input.trim();
    if let Some(index) = text.find("live.bilibili.com/") {
        let digits: String = text[index + 18..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if !digits.is_empty() {
            return Ok(digits.parse()?);
        }
    }
    if let Some(index) = text.find("room_id=") {
        let digits: String = text[index + 8..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if !digits.is_empty() {
            return Ok(digits.parse()?);
        }
    }
    text.parse::<u64>()
        .with_context(|| format!("无法识别直播间: {input}"))
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect::<String>()
        .trim()
        .to_string()
}

// ---------------------------------------------------------------------------
// 直播弹幕 WebSocket 客户端
// ---------------------------------------------------------------------------

async fn record_danmaku(
    client: ApiClient,
    room_id: u64,
    path: &Path,
    duration: Option<std::time::Duration>,
    mut stop: Option<tokio::sync::oneshot::Receiver<()>>,
) -> Result<()> {
    use tokio::io::AsyncWriteExt;

    let (token, host) = danmu_info(&client, room_id).await?;
    let buvid = buvid(&client).await?;
    let url = format!("wss://{host}/sub");
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut request = url.into_client_request()?;
    request
        .headers_mut()
        .insert("Origin", HeaderValue::from_static("https://live.bilibili.com"));
    request.headers_mut().insert(
        reqwest::header::USER_AGENT,
        HeaderValue::from_str(&client.user_agent)?,
    );
    if !client.cookie_header.is_empty() {
        request.headers_mut().insert(
            reqwest::header::COOKIE,
            HeaderValue::from_str(&client.cookie_header)?,
        );
    }

    let (socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .context("连接直播弹幕服务器失败")?;
    let (mut write, mut read) = socket.split();

    let auth = json!({
        "uid": client.mid.unwrap_or(0),
        "roomid": room_id,
        "protover": 3,
        "platform": "web",
        "type": 2,
        "key": token,
        "buvid": buvid,
    });
    write.send(tungstenite_message(7, auth.to_string().as_bytes())).await?;

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut file = tokio::fs::File::create(path).await?;
    file.write_all(
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><i><chatserver>chat.bilibili.com</chatserver><chatid>{room_id}</chatid><mission>0</mission><maxlimit>1000</maxlimit><state>0</state><real_name>0</real_name><source>k-v</source>\n"
        )
        .as_bytes(),
    )
    .await?;

    let start = std::time::Instant::now();
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(30));
    let deadline = duration.map(|duration| tokio::time::Instant::now() + duration);
    let mut buffer = String::new();
    let mut last_flush = std::time::Instant::now();

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                write.send(tungstenite_message(2, b"")).await?;
            }
            message = read.next() => {
                let Some(message) = message else { break };
                match message? {
                    tokio_tungstenite::tungstenite::Message::Binary(data) => {
                        let mut values = Vec::new();
                        parse_frames(&data, &mut values);
                        for value in values {
                            if let Some(line) = format_danmaku(&value, start) {
                                buffer.push_str(&line);
                            }
                        }
                    }
                    tokio_tungstenite::tungstenite::Message::Close(_) => break,
                    _ => {}
                }
            }
            _ = async {
                match stop.as_mut() {
                    Some(receiver) => { let _ = receiver.await; }
                    None => std::future::pending::<()>().await,
                }
            } => break,
            _ = async {
                match deadline {
                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                    None => std::future::pending::<()>().await,
                }
            } => break,
        }

        if !buffer.is_empty() && last_flush.elapsed() >= std::time::Duration::from_secs(1) {
            file.write_all(buffer.as_bytes()).await?;
            file.flush().await?;
            buffer.clear();
            last_flush = std::time::Instant::now();
        }
    }

    if !buffer.is_empty() {
        file.write_all(buffer.as_bytes()).await?;
    }
    file.write_all(b"</i>\n").await?;
    file.flush().await?;
    Ok(())
}

async fn danmu_info(client: &ApiClient, room_id: u64) -> Result<(String, String)> {
    let url = "https://api.live.bilibili.com/xlive/web-room/v1/index/getDanmuInfo";
    let params = vec![
        ("id".to_string(), room_id.to_string()),
        ("type".to_string(), "0".to_string()),
    ];
    let data = match client.get_value(url, &[("id", room_id.to_string()), ("type", "0".to_string())]).await {
        Ok(data) => data,
        Err(_) => client.get_value_signed(url, params).await?,
    };
    let token = data
        .get("token")
        .and_then(Value::as_str)
        .context("弹幕服务器没有返回 token")?
        .to_string();
    let host = data
        .get("host_list")
        .and_then(Value::as_array)
        .and_then(|hosts| hosts.first())
        .and_then(|host| {
            let host_name = host.get("host")?.as_str()?;
            let port = host.get("wss_port").and_then(Value::as_u64).unwrap_or(443);
            Some(format!("{host_name}:{port}"))
        })
        .context("弹幕服务器列表为空")?;
    Ok((token, host))
}

async fn buvid(client: &ApiClient) -> Result<String> {
    if let Some(value) = client.cookies.get("buvid3") {
        return Ok(value.clone());
    }
    let data = client
        .get_value("https://api.bilibili.com/x/frontend/finger/spi", &[])
        .await?;
    data.get("b_3")
        .and_then(Value::as_str)
        .map(str::to_string)
        .context("无法获取 buvid3")
}

fn tungstenite_message(op: u32, body: &[u8]) -> tokio_tungstenite::tungstenite::Message {
    let total = 16 + body.len();
    let mut packet = Vec::with_capacity(total);
    packet.extend_from_slice(&(total as u32).to_be_bytes());
    packet.extend_from_slice(&16u16.to_be_bytes());
    packet.extend_from_slice(&1u16.to_be_bytes());
    packet.extend_from_slice(&op.to_be_bytes());
    packet.extend_from_slice(&1u32.to_be_bytes());
    packet.extend_from_slice(body);
    tokio_tungstenite::tungstenite::Message::Binary(packet.into())
}

fn parse_frames(data: &[u8], out: &mut Vec<Value>) {
    let mut offset = 0usize;
    while offset + 16 <= data.len() {
        let packet_len = u32::from_be_bytes(data[offset..offset + 4].try_into().unwrap_or([0; 4])) as usize;
        let header_len = u16::from_be_bytes(data[offset + 4..offset + 6].try_into().unwrap_or([0; 2])) as usize;
        let version = u16::from_be_bytes(data[offset + 6..offset + 8].try_into().unwrap_or([0; 2]));
        let op = u32::from_be_bytes(data[offset + 8..offset + 12].try_into().unwrap_or([0; 4]));
        if packet_len < header_len || offset + packet_len > data.len() {
            break;
        }
        let body = &data[offset + header_len..offset + packet_len];
        match version {
            2 | 3 => {
                if let Ok(decoded) = decompress(body, version) {
                    parse_frames(&decoded, out);
                }
            }
            _ if op == 5 => {
                if let Ok(value) = serde_json::from_slice::<Value>(body) {
                    out.push(value);
                }
            }
            _ => {}
        }
        offset += packet_len;
    }
}

fn decompress(data: &[u8], version: u16) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::new();
    if version == 3 {
        brotli::Decompressor::new(data, 4096).read_to_end(&mut out)?;
    } else {
        flate2::read::ZlibDecoder::new(data).read_to_end(&mut out)?;
    }
    Ok(out)
}

fn format_danmaku(value: &Value, start: std::time::Instant) -> Option<String> {
    let command = value.get("cmd")?.as_str()?;
    if !command.starts_with("DANMU_MSG") {
        return None;
    }
    let info = value.get("info")?.as_array()?;
    let text = info.get(1)?.as_str()?;
    let meta = info.first()?.as_array()?;
    let mode = meta.get(1).and_then(Value::as_u64).unwrap_or(1);
    let fontsize = meta.get(2).and_then(Value::as_u64).unwrap_or(25);
    let color = meta.get(3).and_then(Value::as_u64).unwrap_or(16777215);
    let timestamp = meta.get(4).and_then(Value::as_u64).unwrap_or(0) / 1000;
    let user = info.get(2)?.as_array()?;
    let uid = user.first().and_then(Value::as_u64).unwrap_or(0);
    let seconds = start.elapsed().as_secs_f64();
    Some(format!(
        "<d p=\"{:.3},{},{},{},{},0,{},{}\">{}</d>\n",
        seconds,
        mode,
        fontsize,
        color,
        timestamp,
        uid,
        0,
        xml_escape(text)
    ))
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
