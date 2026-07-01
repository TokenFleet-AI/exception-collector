# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 项目概述

exception-collector 是 TokenFleet-AI 全项目组异常收集系统。自动捕获 `panic`、`tracing::error!` 和 `Result::Err`，本地签名去重后写入 SQLite 缓冲区。Desktop Hub 定期扫描所有组件的异常数据，通过 LLM 语义去重后自动创建 GitHub Issue。

## 架构设计

### 双缓冲机制

```
各项目（写）                    Desktop Hub（读+上报）
──────────                     ────────────────────
component.db ──────────┐
                       ├──→ collect_unreported()
                       │    → Pipeline 编排
                       │    → LLM 分类去重
                       │    → GitHub Issue / 自研平台
                       └──→ mark_reported()
```

- **ExceptionBuffer**: DashMap（内存）+ SQLite（持久化）双缓冲
- **签名去重**: SHA256 归一化签名，本地去重后写入 SQLite
- **LLM 去重**: 语义层面去重，避免重复 Issue

### 模块职责

| 模块 | 职责 |
|------|------|
| `lib` | 核心类型定义（ExceptionRecord, ExceptionBatch 等） |
| `buffer` | DashMap + SQLite 双缓冲，`collect()`, `flush()`, `load()` |
| `collector` | 扫描 `*.db` 文件，`collect_unreported()`, `collect_result_err()` |
| `signature` | SHA256 异常签名计算 |
| `normalize` | 错误消息脱敏归一化（移除路径、ID、时间戳等） |
| `dedup` | 本地签名去重引擎 |
| `llm` | LLM 语义去重分类（feature: `http-llm`） |
| `pipeline` | 上报流水线编排，协调去重、分类、上报 |
| `reporter` | GitHub Issue / 自定义平台上报实现 |
| `retry` | 重试策略（指数退避） |
| `config` | 从 `repo-map.toml` 加载组件→仓库映射 |

### 数据流

1. **写入端**（各项目）:
   ```rust
   let buffer = ExceptionBuffer::with_default_dir("component-name")?;
   collect_result_err(&buffer, "component-name", &error.to_string());
   // 自动计算签名，DashMap 聚合，flush() 持久化到 SQLite
   ```

2. **读取端**（Desktop Hub）:
   ```rust
   let all_records = collect_unreported("/path/to/exceptions")?;
   // 扫描所有 *.db，返回未上报的异常记录
   // 通过 Pipeline 处理：去重 → LLM 分类 → 上报
   ```

## 关键类型

- **ExceptionRecord**: 单条异常记录（含 component, kind, message, stacktrace, dedup_signature 等）
- **AggregatedException**: 按签名聚合的异常（first_seen, last_seen, count, sample）
- **ExceptionBatch**: 上报批次（包含多条 records，状态为 Pending/Reported/Failed）
- **ReportTarget**: 上报目标（GitHub Issue 或 CustomPlatform）

## 常用命令

```bash
# 构建
cargo build

# 运行测试
cargo test

# 运行单个测试
cargo test <test_name>

# Lint 检查
cargo clippy --all-targets --all-features

# 代码格式化
cargo fmt

# 检查文档
cargo doc --no-deps --open
```

## Feature Flags

- `http-llm`: 启用 HTTP LLM 通道（reqwest），用于远程 LLM 分类
- `testing`: 测试辅助功能

## 配置

`repo-map.toml` 定义组件到 GitHub 仓库的映射：

```toml
[components]
"token-fleet-switch" = "TokenFleet-AI/token-fleet-switch"
"agent-proxy-rust" = "TokenFleet-AI/agent-proxy-rust"
```

## 开发约定

- **禁止 unsafe code**: `#![forbid(unsafe_code)]`
- **强制文档注释**: `#![warn(missing_docs)]`
- **中文优先**: 注释、文档、错误信息使用中文
- **SQLite 存储**: 所有异常数据持久化到 SQLite（`rusqlite` bundled 模式）
- **异步运行时**: 使用 `tokio`（rt-multi-thread, macros, time, process）
- **并发安全**: DashMap 用于内存聚合，Mutex 保护 SQLite 连接

## 发布流程

### 发布前检查
```bash
# 运行所有检查
make lint

# 或者分步执行
make fmt      # 格式化
make clippy   # lint 检查
make audit    # 安全审计
make test     # 运行测试
```

### 发布新版本
```bash
# Step 1: 更新版本、生成 CHANGELOG、创建 tag、推送
make release VERSION=patch  # 或 minor, major

# Step 2: 等待 CI 通过后，发布到 crates.io
make release-publish
```

### 版本更新策略
- **patch**: 修复 bug，不改变 API（0.1.0 → 0.1.1）
- **minor**: 新增功能，向后兼容（0.1.0 → 0.2.0）
- **major**: 破坏性变更（0.1.0 → 1.0.0）

## Pre-commit 配置

安装 pre-commit hooks：
```bash
pip install pre-commit  # 或使用 pipx
pre-commit install
```

首次运行所有检查：
```bash
pre-commit run --all-files
```

## 必需工具

- `cargo-nextest`: 测试运行器（`cargo install cargo-nextest`）
- `cargo-release`: 发布工具（`cargo install cargo-release`）
- `git-cliff`: CHANGELOG 生成（`cargo install git-cliff`）
- `cargo-deny`: 依赖检查（`cargo install cargo-deny`）
- `cargo-audit`: 安全审计（`cargo install cargo-audit`）
- `typos`: 拼写检查（`cargo install typos-cli`）
