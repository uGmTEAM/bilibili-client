//! 子命令分发。

use crate::api;
use crate::cli::{Command, VideoAction};
use crate::config::AppConfig;
use crate::output::Output;
use anyhow::Result;

pub async fn dispatch(command: Command, cfg: &AppConfig, out: Output) -> Result<()> {
    match command {
        Command::Login(args) => api::account::login(&args, cfg, out).await,
        Command::Logout => api::account::logout(cfg, out).await,
        Command::Whoami => api::account::whoami(cfg, out).await,
        Command::Vip { action } => api::account::vip(action, cfg, out).await,
        Command::Video { action } => match action {
            VideoAction::Info { id } => api::download::run_info(&id, cfg, out).await,
            VideoAction::Download(args) => api::download::run_download(args, cfg, out).await,
            VideoAction::Cover { id, output } => {
                api::download::run_cover(&id, output, cfg, out).await
            }
            VideoAction::Subtitle(args) => api::download::run_subtitle(args, cfg, out).await,
            VideoAction::Danmaku(args) => {
                api::download::run_danmaku_download(args, cfg, out).await
            }
        },
        Command::Comment { action } => api::comment::run(action, cfg, out).await,
        Command::Interact { action } => api::interact::run(action, cfg, out).await,
        Command::Upload { action } => api::upload::run(action, cfg, out).await,
        Command::Season { action } => api::season::run(action, cfg, out).await,
        Command::Danmaku { action } => api::danmaku::run(action, cfg, out).await,
        Command::Live { action } => api::live::run(action, cfg, out).await,
        Command::Message { action } => api::dm::run(action, cfg, out).await,
        Command::Completions { .. } => Ok(()),
    }
}
