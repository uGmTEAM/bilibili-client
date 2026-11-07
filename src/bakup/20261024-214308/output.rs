//! 统一输出：`--json` 时输出结构化 JSON，否则输出人类可读文本。

use serde::Serialize;

#[derive(Debug, Clone, Copy)]
pub struct Output {
    pub json: bool,
}

impl Output {
    pub fn new(json: bool) -> Self {
        Self { json }
    }

    /// 输出一个结构化结果；`human` 只在非 JSON 模式下执行。
    pub fn emit(&self, value: impl Serialize, human: impl FnOnce() -> String) {
        if self.json {
            match serde_json::to_string_pretty(&value) {
                Ok(text) => println!("{text}"),
                Err(error) => println!("{{\"error\":\"{error}\"}}"),
            }
        } else {
            println!("{}", human());
        }
    }

    pub fn info(&self, message: impl AsRef<str>) {
        if !self.json {
            println!("{}", message.as_ref());
        }
    }
}
