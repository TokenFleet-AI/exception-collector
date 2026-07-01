# exception-collector

全项目组异常收集系统。自动捕获 `panic`、`tracing::error!` 和 `Result::Err`，本地签名去重后写入 SQLite 缓冲区。Desktop Hub 定期扫描所有 TokenFleet 组件的异常数据，通过 LLM 语义去重后自动创建 GitHub Issue。

## 快速集成

```rust
use exception_collector::{ExceptionBuffer, collect_result_err};

// 每个项目一行接入
let buffer = ExceptionBuffer::with_default_dir("your-component")?;

// 在错误处捕获
if let Err(e) = fallible_operation() {
    collect_result_err(&buffer, "your-component", &e.to_string());
}
```

## 架构

```
各项目（写）                    Desktop Hub（读+上报）
──────────                     ────────────────────
agent-proxy-rust.db ──┐
tokenless.db ────────┼──→ collect_unreported()
rtk.db ──────────────┘       → Pipeline
                              → LLM 分类
                              → GitHub Issue
```

## 模块

| 模块 | 功能 |
|------|------|
| `signature` | SHA256 异常签名计算 |
| `normalize` | 错误消息脱敏归一化 |
| `buffer` | DashMap + SQLite 双缓冲 |
| `dedup` | 本地签名去重 |
| `reporter` | GitHub Issue / 自定义平台上报 |
| `llm` | LLM 语义去重分类（可选） |
| `pipeline` | 上报流水线编排 |
| `collector` | 数据库扫描入口 |

## License

Apache-2.0
