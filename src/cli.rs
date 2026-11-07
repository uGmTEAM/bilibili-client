//! 命令行参数定义。所有子命令集中在这里，便于 `--help` 与补全保持一致。

use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "bilic",
    version,
    about = "Bilibili command line client (Linux)",
    propagate_version = true
)]
pub struct Cli {
    /// 以 JSON 输出结果（便于脚本处理）
    #[arg(long, global = true)]
    pub json: bool,

    /// 配置文件路径（默认 ~/.config/bilibili_client/config.toml）
    #[arg(short = 'c', long, global = true)]
    pub config: Option<PathBuf>,

    /// 登录信息文件（默认与配置文件同目录的 cookies.json）
    #[arg(long, global = true)]
    pub cookie: Option<PathBuf>,

    /// HTTP 代理，如 http://127.0.0.1:7890
    #[arg(long, global = true)]
    pub proxy: Option<String>,

    /// 日志级别：-v 为 info，-vv 为 debug
    #[arg(short = 'v', long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// 登录 B 站（默认扫码）
    Login(LoginArgs),
    /// 删除本地登录信息
    Logout,
    /// 查看当前登录账号
    Whoami,
    /// 大会员信息（仅查询，不提供自动购买）
    Vip {
        #[command(subcommand)]
        action: VipAction,
    },
    /// 视频信息、下载、封面、字幕、弹幕
    Video {
        #[command(subcommand)]
        action: VideoAction,
    },
    /// 评论读取与互动
    Comment {
        #[command(subcommand)]
        action: CommentAction,
    },
    /// 点赞 / 投币 / 收藏 / 关注
    Interact {
        #[command(subcommand)]
        action: InteractAction,
    },
    /// 投稿（上传视频）
    Upload {
        #[command(subcommand)]
        action: UploadAction,
    },
    /// 合集（小节）管理
    Season {
        #[command(subcommand)]
        action: SeasonAction,
    },
    /// 视频弹幕发送、定时发送、撤回
    Danmaku {
        #[command(subcommand)]
        action: DanmakuAction,
    },
    /// 直播信息、录制、直播弹幕
    Live {
        #[command(subcommand)]
        action: LiveAction,
    },
    /// 私信：会话、历史、发送、回复、自动回复
    Message {
        #[command(subcommand)]
        action: MessageAction,
    },
    /// 生成 shell 补全脚本
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

#[derive(Args, Debug)]
pub struct LoginArgs {
    /// 使用终端二维码扫码登录（默认方式）
    #[arg(long, conflicts_with_all = ["sessdata", "cookie_file"])]
    pub qrcode: bool,

    /// 直接使用浏览器 Cookie 中的 SESSDATA
    #[arg(long)]
    pub sessdata: Option<String>,

    /// 浏览器 Cookie 中的 bili_jct（CSRF Token）
    #[arg(long)]
    pub bili_jct: Option<String>,

    /// 浏览器 Cookie 中的 DedeUserID（可选，用于显示账号）
    #[arg(long)]
    pub dede_user_id: Option<String>,

    /// 从 JSON 文件导入 Cookie（支持 biliup 登录信息或 {SESSDATA, bili_jct} 形式）
    #[arg(long)]
    pub cookie_file: Option<PathBuf>,

    /// 登录信息保存路径
    #[arg(long)]
    pub save: Option<PathBuf>,

    /// 用已保存的 refresh_token 续期登录态（无需重新扫码）
    #[arg(long, conflicts_with_all = ["sessdata", "bili_jct", "cookie_file"])]
    pub refresh: bool,
}

#[derive(Subcommand, Debug)]
pub enum VipAction {
    /// 查询大会员状态、类型与到期时间
    Status,
    /// 打开官方大会员购买页（不做自动支付）
    Open,
}

#[derive(Subcommand, Debug)]
pub enum VideoAction {
    /// 视频详情（BV/av/URL/EP/ss）
    Info { id: String },
    /// 下载视频（DASH 音视频，需 ffmpeg 合流）
    Download(DownloadArgs),
    /// 只下载封面
    Cover {
        id: String,
        /// 输出文件，默认 ./{title}.jpg
        #[arg(short = 'o', long)]
        output: Option<PathBuf>,
    },
    /// 下载字幕（CC / AI 字幕）
    Subtitle(SubtitleArgs),
    /// 下载弹幕 XML（可转 ASS）
    Danmaku(DanmakuDownloadArgs),
}

#[derive(Args, Debug)]
pub struct DownloadArgs {
    /// 视频 ID 或链接，可给多个
    #[arg(required = true)]
    pub ids: Vec<String>,

    /// 画质：8k/4k/1080p60/1080p/720p/480p/360p，或直接给 qn 数字
    #[arg(short = 'q', long, default_value = "1080p")]
    pub quality: String,

    /// 视频编码
    #[arg(long, value_enum, default_value = "avc")]
    pub codec: CodecArg,

    /// 只下载第几 P（从 1 开始）
    #[arg(short = 'p', long)]
    pub page: Option<u32>,

    /// 下载全部分 P
    #[arg(long)]
    pub all_pages: bool,

    /// 输出目录
    #[arg(short = 'o', long)]
    pub output: Option<PathBuf>,

    /// 文件名模板，支持 {title} {bvid} {p} {part} {cid} {quality} {ext}
    #[arg(long)]
    pub name: Option<String>,

    /// 同时下载封面
    #[arg(long)]
    pub with_cover: bool,

    /// 同时下载字幕
    #[arg(long)]
    pub with_subtitle: bool,

    /// 同时下载弹幕 XML
    #[arg(long)]
    pub with_danmaku: bool,

    /// 把弹幕转换成 ASS
    #[arg(long)]
    pub danmaku_ass: bool,

    /// 同时下载的 P 数
    #[arg(long, default_value_t = 1)]
    pub jobs: usize,

    /// 覆盖已存在的文件
    #[arg(long)]
    pub overwrite: bool,

    /// 只保留分离的音视频文件，不调用 ffmpeg 合流
    #[arg(long)]
    pub no_merge: bool,

    /// 只下载音频
    #[arg(long)]
    pub audio_only: bool,
}

#[derive(ValueEnum, Clone, Copy, Debug, Default)]
pub enum CodecArg {
    #[default]
    Avc,
    Hevc,
    Av1,
}

#[derive(Args, Debug)]
pub struct SubtitleArgs {
    pub id: String,
    /// 第几 P
    #[arg(short = 'p', long, default_value_t = 1)]
    pub page: u32,
    /// 语言代码，如 zh-CN、ai-zh
    #[arg(long)]
    pub lang: Option<String>,
    /// 输出目录
    #[arg(short = 'o', long)]
    pub output: Option<PathBuf>,
    /// 输出为 JSON 原文而不是 SRT
    #[arg(long)]
    pub raw: bool,
}

#[derive(Args, Debug)]
pub struct DanmakuDownloadArgs {
    pub id: String,
    #[arg(short = 'p', long, default_value_t = 1)]
    pub page: u32,
    #[arg(long)]
    pub all_pages: bool,
    #[arg(short = 'o', long)]
    pub output: Option<PathBuf>,
    /// 同时生成 ASS
    #[arg(long)]
    pub ass: bool,
}

#[derive(Subcommand, Debug)]
pub enum CommentAction {
    /// 读取评论
    List {
        id: String,
        /// 排序：time 或 hot
        #[arg(long, default_value = "time")]
        sort: SortArg,
        #[arg(long, default_value_t = 1)]
        page: u32,
        #[arg(long, default_value_t = 20)]
        size: u32,
        /// 同时展开楼中楼
        #[arg(long)]
        replies: bool,
    },
    /// 读取某条评论下的楼中楼
    Replies {
        id: String,
        /// 根评论 rpid
        #[arg(long)]
        rpid: u64,
        #[arg(long, default_value_t = 1)]
        page: u32,
        #[arg(long, default_value_t = 20)]
        size: u32,
    },
    /// 读取「回复我的」消息（需要登录）
    Received {
        /// 消息类型
        #[arg(long, value_enum, default_value = "reply")]
        kind: FeedKind,
        /// 翻页游标，取上次输出的 next_cursor
        #[arg(long)]
        cursor: Option<u64>,
    },
    /// 发表评论；带 --root 时为回复
    Send {
        id: String,
        message: String,
        /// 要回复的评论 rpid
        #[arg(long)]
        root: Option<u64>,
    },
    /// 删除自己的评论
    Delete {
        id: String,
        #[arg(long)]
        rpid: u64,
    },
    /// 给评论点赞
    Like {
        id: String,
        #[arg(long)]
        rpid: u64,
        /// 取消点赞
        #[arg(long)]
        undo: bool,
    },
    /// 给评论点踩
    Dislike {
        id: String,
        #[arg(long)]
        rpid: u64,
        /// 取消点踩
        #[arg(long)]
        undo: bool,
    },
}

#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum SortArg {
    Time,
    Hot,
}

/// 「回复我的」消息类型
#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum FeedKind {
    Reply,
    At,
    Like,
}

impl SortArg {
    pub fn value(self) -> u8 {
        match self {
            SortArg::Time => 0,
            SortArg::Hot => 2,
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum InteractAction {
    /// 一键三连（点赞 + 投币 + 收藏）
    Triple {
        id: String,
        /// 投币数量 1 或 2
        #[arg(long, default_value_t = 1)]
        coin: u8,
        /// 收藏夹 ID；不填时用默认收藏夹
        #[arg(long)]
        folder_id: Option<u64>,
        /// 不点赞
        #[arg(long)]
        no_like: bool,
        /// 不投币
        #[arg(long)]
        no_coin: bool,
        /// 不收藏
        #[arg(long)]
        no_fav: bool,
    },
    /// 关注 / 取关用户
    Follow {
        /// 用户 mid
        mid: u64,
        /// 取消关注
        #[arg(long)]
        undo: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum UploadAction {
    /// 新建投稿
    New(UploadArgs),
    /// 编辑已投稿稿件（仅修改传入的字段）
    Edit(UploadEditArgs),
    /// 给已有稿件追加分 P
    Append(UploadAppendArgs),
    /// 投稿模板
    Template {
        #[command(subcommand)]
        action: TemplateAction,
    },
}

#[derive(Subcommand, Debug)]
pub enum TemplateAction {
    /// 生成示例模板
    Init {
        /// 输出路径
        #[arg(default_value = "upload.toml")]
        path: PathBuf,
    },
    /// 显示模板内容
    Show {
        path: PathBuf,
    },
    /// 校验模板
    Validate {
        path: PathBuf,
    },
}

#[derive(Args, Debug, Default)]
pub struct UploadArgs {
    /// 视频文件，按顺序作为分 P
    #[arg(required = true)]
    pub files: Vec<PathBuf>,

    /// 投稿模板 TOML，命令行参数优先
    #[arg(long)]
    pub template: Option<PathBuf>,

    #[arg(long)]
    pub title: Option<String>,
    #[arg(long)]
    pub desc: Option<String>,
    /// 标签，逗号分隔
    #[arg(long)]
    pub tag: Option<String>,
    /// 分区 ID
    #[arg(long)]
    pub tid: Option<u16>,
    /// 新版分区 ID
    #[arg(long)]
    pub tid_v2: Option<u32>,
    /// 封面文件；传 URL 时自动下载
    #[arg(long)]
    pub cover: Option<String>,
    /// 1 自制，2 转载
    #[arg(long)]
    pub copyright: Option<u8>,
    /// 转载来源
    #[arg(long)]
    pub source: Option<String>,
    /// 粉丝动态
    #[arg(long)]
    pub dynamic: Option<String>,
    /// 定时发布：Unix 时间戳或 RFC3339
    #[arg(long)]
    pub dtime: Option<String>,
    /// 投稿成功后加入合集小节
    #[arg(long)]
    pub section_id: Option<u64>,
    /// 提交接口
    #[arg(long, value_enum)]
    pub submit: Option<SubmitArg>,
    /// 上传线路（auto 或 bldsa/cnbldsa/...）
    #[arg(long)]
    pub line: Option<String>,
    /// 单文件并发分片数
    #[arg(long)]
    pub limit: Option<usize>,
    /// 遇到 412 风控时不自动切换线路重试（默认自动）
    #[arg(long)]
    pub no_auto_line_retry: bool,
    /// 单文件最多换几次线路（默认 3）
    #[arg(long, default_value_t = 3)]
    pub line_retry_max: usize,
    /// 禁止转载
    #[arg(long)]
    pub no_reprint: bool,
    /// 仅自己可见
    #[arg(long)]
    pub only_self: bool,
    /// 开启充电
    #[arg(long)]
    pub charging_pay: bool,
    /// 开启杜比音效
    #[arg(long)]
    pub dolby: bool,
    /// 开启 Hi-Res
    #[arg(long)]
    pub hires: bool,
    /// 开启字幕
    #[arg(long)]
    pub open_subtitle: bool,
    /// 精选评论
    #[arg(long)]
    pub up_selection_reply: bool,
    /// 关闭评论
    #[arg(long)]
    pub up_close_reply: bool,
    /// 关闭弹幕
    #[arg(long)]
    pub up_close_danmu: bool,
    /// 互动视频
    #[arg(long)]
    pub interactive: bool,
    /// 额外提交参数（JSON 对象，覆盖/补充接口字段）
    #[arg(long)]
    pub extra: Option<String>,
}

#[derive(Args, Debug, Default)]
pub struct UploadEditArgs {
    /// 稿件 BV/av
    pub vid: String,
    /// 模板 TOML
    #[arg(long)]
    pub template: Option<PathBuf>,
    #[arg(long)]
    pub title: Option<String>,
    #[arg(long)]
    pub desc: Option<String>,
    #[arg(long)]
    pub tag: Option<String>,
    #[arg(long)]
    pub tid: Option<u16>,
    #[arg(long)]
    pub cover: Option<String>,
    #[arg(long)]
    pub copyright: Option<u8>,
    #[arg(long)]
    pub source: Option<String>,
    #[arg(long)]
    pub dynamic: Option<String>,
    #[arg(long)]
    pub dtime: Option<String>,
    #[arg(long, value_enum)]
    pub submit: Option<SubmitArg>,
    #[arg(long)]
    pub extra: Option<String>,
}

#[derive(Args, Debug, Default)]
pub struct UploadAppendArgs {
    /// 稿件 BV/av
    pub vid: String,
    /// 追加的视频文件
    #[arg(required = true)]
    pub files: Vec<PathBuf>,
    #[arg(long)]
    pub line: Option<String>,
    #[arg(long)]
    pub limit: Option<usize>,
    /// 遇到 412 风控时不自动切换线路重试（默认自动）
    #[arg(long)]
    pub no_auto_line_retry: bool,
    /// 单文件最多换几次线路（默认 3）
    #[arg(long, default_value_t = 3)]
    pub line_retry_max: usize,
    #[arg(long, value_enum)]
    pub submit: Option<SubmitArg>,
}

#[derive(ValueEnum, Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubmitArg {
    App,
    #[default]
    Web,
    Bcut,
}

#[derive(Subcommand, Debug)]
pub enum SeasonAction {
    /// 列出自己的合集
    List {
        #[arg(long, default_value_t = 1)]
        page: u32,
        #[arg(long, default_value_t = 20)]
        size: u32,
    },
    /// 查看小节及稿件
    Sections {
        section_id: u64,
        #[arg(long, default_value = "desc")]
        sort: String,
    },
    /// 把稿件加入小节
    Add {
        section_id: u64,
        /// BV 号或 av 号，可重复
        #[arg(long = "vid", required = true)]
        vids: Vec<String>,
    },
    /// 从合集移出稿件（参数是 episode id，不是 aid）
    Remove { episode_id: u64 },
    /// 重排小节稿件
    Sort {
        section_id: u64,
        season_id: u64,
        section_title: String,
        /// episode id，按期望顺序
        #[arg(required = true)]
        episode_ids: Vec<u64>,
        /// 整体倒序
        #[arg(long)]
        reverse: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum DanmakuAction {
    /// 发送视频弹幕
    Send(DanmakuSendArgs),
    /// 撤回自己发送的弹幕
    Recall {
        /// 弹幕 dmid
        #[arg(long)]
        dmid: u64,
        /// 视频 ID（用于解析 cid）
        id: String,
        #[arg(short = 'p', long, default_value_t = 1)]
        page: u32,
    },
}

#[derive(Args, Debug)]
pub struct DanmakuSendArgs {
    /// 视频 ID
    pub id: String,
    /// 弹幕内容
    pub message: String,
    /// 视频时间点，如 00:01:30 或 90s
    #[arg(long, default_value = "0")]
    pub at: String,
    /// 墙钟定时发送，如 "2026-10-25 20:00" 或 RFC3339
    #[arg(long)]
    pub schedule: Option<String>,
    /// 颜色：#RRGGBB 或十进制
    #[arg(long, default_value = "#FFFFFF")]
    pub color: String,
    /// 字号：18 / 25 / 36
    #[arg(long, default_value_t = 25)]
    pub fontsize: u32,
    /// 弹幕模式
    #[arg(long, value_enum, default_value = "scroll")]
    pub mode: DmModeArg,
    /// 弹幕池：0 普通，1 字幕，2 特殊
    #[arg(long, default_value_t = 0)]
    pub pool: u32,
    /// 第几 P
    #[arg(short = 'p', long, default_value_t = 1)]
    pub page: u32,
    /// 只打印请求，不实际发送
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(ValueEnum, Clone, Copy, Debug, Default)]
pub enum DmModeArg {
    #[default]
    Scroll,
    Top,
    Bottom,
    Reverse,
}

impl DmModeArg {
    pub fn value(self) -> u32 {
        match self {
            DmModeArg::Scroll => 1,
            DmModeArg::Bottom => 4,
            DmModeArg::Top => 5,
            DmModeArg::Reverse => 6,
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum LiveAction {
    /// 直播间信息与拉流地址
    Info { room: String },
    /// 录制直播（可选同时录制弹幕）
    Record(LiveRecordArgs),
    /// 只录制直播弹幕为 XML
    Danmaku(LiveDanmakuArgs),
    /// 在直播间发弹幕（实验性）
    Send {
        room: String,
        message: String,
        #[arg(long, default_value = "#FFFFFF")]
        color: String,
    },
}

#[derive(Args, Debug)]
pub struct LiveRecordArgs {
    /// 直播间链接或房间号
    pub room: String,
    /// 输出文件模板
    #[arg(short = 'o', long)]
    pub output: Option<PathBuf>,
    /// 文件名模板
    #[arg(long)]
    pub name: Option<String>,
    /// 同时录制弹幕
    #[arg(long)]
    pub danmaku: bool,
    /// 录制时长，如 30m、2h；不填则一直录到直播结束或 Ctrl+C
    #[arg(long)]
    pub duration: Option<String>,
}

#[derive(Args, Debug)]
pub struct LiveDanmakuArgs {
    pub room: String,
    #[arg(short = 'o', long)]
    pub output: Option<PathBuf>,
    #[arg(long)]
    pub duration: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum MessageAction {
    /// 列出私信会话
    List,
    /// 查看与某人的历史消息
    History {
        /// 对方 mid
        mid: u64,
        #[arg(long, default_value_t = 20)]
        count: u32,
    },
    /// 直接发送私信（不做自动消息判断）
    Send { mid: u64, message: String },
    /// 回复最新一条消息；检测到自动消息时默认跳过
    Reply {
        mid: u64,
        message: String,
        /// 即使检测为自动消息也发送
        #[arg(long)]
        force: bool,
    },
    /// 阅读最新消息后自动回复（单次）
    AutoReply {
        mid: u64,
        /// 回复内容，支持 {name} {mid} {content}
        #[arg(long)]
        text: Option<String>,
        /// 从文件读取回复模板
        #[arg(long)]
        template: Option<PathBuf>,
        /// 即使检测为自动消息也发送
        #[arg(long)]
        force: bool,
        /// 只打印将要发送的内容
        #[arg(long)]
        dry_run: bool,
    },
    /// 轮询未读私信并自动回复（跳过自动消息与已回复过的消息）
    Watch(MessageWatchArgs),
    /// 把与某人的会话标记为已读
    Read { mid: u64 },
    /// 查看未读总数
    Unread,
}

#[derive(Args, Debug)]
pub struct MessageWatchArgs {
    /// 轮询间隔（秒），默认读配置 dm_poll_seconds 或 60
    #[arg(long)]
    pub interval: Option<u64>,
    /// 回复内容，支持 {name} {mid} {content}
    #[arg(long)]
    pub text: Option<String>,
    /// 从文件读取回复模板
    #[arg(long)]
    pub template: Option<PathBuf>,
    /// 只打印将要发送的内容
    #[arg(long)]
    pub dry_run: bool,
    /// 只跑一轮，不常驻
    #[arg(long)]
    pub once: bool,
}
