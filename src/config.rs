//! 配置加载：命令行 > 配置文件 > 环境变量 > 默认值。

use crate::cli::{Cli, SubmitArg};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const DEFAULT_UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

#[derive(Debug, Deserialize, Default)]
pub struct FileConfig {
    pub cookie: Option<PathBuf>,
    pub proxy: Option<String>,
    pub download_dir: Option<PathBuf>,
    pub ffmpeg: Option<String>,
    pub filename: Option<String>,
    pub limit: Option<usize>,
    pub submit: Option<SubmitArg>,
    pub line: Option<String>,
    pub user_agent: Option<String>,
    /// 私信自动回复的默认文本，支持 {name} {mid} {content}
    pub dm_auto_reply: Option<String>,
    /// 私信监听轮询间隔（秒）
    pub dm_poll_seconds: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub cookie: PathBuf,
    pub proxy: Option<String>,
    pub download_dir: PathBuf,
    pub ffmpeg: String,
    pub filename: String,
    pub limit: usize,
    pub submit: SubmitArg,
    pub line: Option<String>,
    pub user_agent: String,
    pub dm_auto_reply: Option<String>,
    pub dm_poll_seconds: u64,
}

impl AppConfig {
    pub fn load(cli: &Cli) -> Result<Self> {
        let config_path = cli
            .config
            .clone()
            .unwrap_or_else(|| default_config_dir().join("config.toml"));
        let file = if config_path.exists() {
            let text = std::fs::read_to_string(&config_path)
                .with_context(|| format!("读取配置文件失败: {}", config_path.display()))?;
            toml::from_str::<FileConfig>(&text)
                .with_context(|| format!("解析配置文件失败: {}", config_path.display()))?
        } else {
            FileConfig::default()
        };

        let base = if cli.config.is_some() {
            config_path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(default_config_dir)
        } else {
            default_config_dir()
        };

        let cookie = cli
            .cookie
            .clone()
            .or_else(|| std::env::var_os("BILIBILI_CLIENT_COOKIE").map(PathBuf::from))
            .or(file.cookie.map(|p| expand_path(&base, p)))
            .unwrap_or_else(|| base.join("cookies.json"));

        let proxy = cli
            .proxy
            .clone()
            .or_else(|| std::env::var("BILIBILI_CLIENT_PROXY").ok())
            .or(file.proxy);

        Ok(Self {
            cookie,
            proxy,
            download_dir: file.download_dir.unwrap_or_else(|| PathBuf::from(".")),
            ffmpeg: file.ffmpeg.unwrap_or_else(|| "ffmpeg".to_string()),
            filename: file
                .filename
                .unwrap_or_else(|| "{title}/{p} {part}".to_string()),
            limit: file.limit.unwrap_or(3),
            submit: file.submit.unwrap_or_default(),
            line: file.line,
            user_agent: file.user_agent.unwrap_or_else(|| DEFAULT_UA.to_string()),
            dm_auto_reply: file.dm_auto_reply,
            dm_poll_seconds: file.dm_poll_seconds.unwrap_or(60),
        })
    }
}

pub fn default_config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("bilibili_client")
}

fn expand_path(base: &Path, path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix("~/") {
        return dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(rest);
    }
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}
