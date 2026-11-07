# 第三方代码

本项目的 `vendor/` 目录包含从 [biliup/biliup](https://github.com/biliup/biliup) 复制的代码：

| 目录 | 上游 | 许可 |
| --- | --- | --- |
| `vendor/biliup` | `crates/biliup`（biliup 1.3.4） | MIT OR Apache-2.0 |
| `vendor/danmaku-factory` | `crates/danmaku-factory`（biliup 1.3.4） | (MIT OR Apache-2.0) AND MIT |

上游版权归 biliup 项目及其贡献者所有。原始 `LICENSE` 见仓库根目录。

为适配本项目的 TLS 方案，`vendor/biliup/Cargo.toml` 未做逻辑修改，依赖仍通过工作区统一声明（rustls）。
