//! B 站接口封装。

pub mod account;
pub mod client;
pub mod comment;
pub mod danmaku;
pub mod dm;
pub mod download;
pub mod file;
pub mod interact;
pub mod live;
pub mod season;
pub mod upload;
pub mod video;
pub mod wbi;

pub use client::ApiClient;
