//! 通用文件下载（支持断点续传与进度条）。

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, HeaderName, RANGE};
use std::path::Path;
use tokio::io::AsyncWriteExt;

/// 下载 `url` 到 `path`，支持 `.part` 断点续传。
/// 返回最终文件字节数；`progress` 为 false 时不显示进度条。
pub async fn download_to(
    client: &reqwest::Client,
    headers: &[(HeaderName, String)],
    url: &str,
    path: &Path,
    overwrite: bool,
    progress: bool,
) -> Result<u64> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }
    if path.exists() && !overwrite {
        return Ok(std::fs::metadata(path)?.len());
    }

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("输出路径没有文件名")?;
    let part = path.with_file_name(format!("{file_name}.part"));
    let existing = if part.exists() {
        std::fs::metadata(&part)?.len()
    } else {
        0
    };

    let mut request = client.get(url);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    if existing > 0 {
        request = request.header(RANGE, format!("bytes={existing}-"));
    }
    let response = request.send().await?;
    let status = response.status();
    if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
        tokio::fs::rename(&part, path).await.ok();
        return Ok(existing);
    }
    if !status.is_success() {
        bail!("下载失败 HTTP {status}: {url}");
    }

    let appending = status == reqwest::StatusCode::PARTIAL_CONTENT && existing > 0;
    if !appending && existing > 0 {
        let _ = tokio::fs::remove_file(&part).await;
    }
    if !appending && path.exists() && overwrite {
        let _ = tokio::fs::remove_file(path).await;
    }

    let total = response
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.rsplit('/').next())
        .and_then(|value| value.parse::<u64>().ok())
        .or_else(|| {
            response
                .headers()
                .get(CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .map(|length| length + if appending { existing } else { 0 })
        });

    let bar = if progress {
        let bar = match total {
            Some(total) => {
                let bar = ProgressBar::new(total);
                let _ = bar.set_style(
                    ProgressStyle::with_template(
                        "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})",
                    )
                    .unwrap_or_else(|_| ProgressStyle::default_bar()),
                );
                bar
            }
            None => ProgressBar::new_spinner(),
        };
        if appending {
            bar.set_position(existing);
        }
        bar
    } else {
        ProgressBar::hidden()
    };

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(appending)
        .truncate(!appending)
        .open(&part)
        .await
        .with_context(|| format!("创建文件失败: {}", part.display()))?;

    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        bar.inc(chunk.len() as u64);
    }
    file.flush().await?;
    drop(file);
    bar.finish_and_clear();

    tokio::fs::rename(&part, path)
        .await
        .with_context(|| format!("写入文件失败: {}", path.display()))?;
    Ok(std::fs::metadata(path)?.len())
}
