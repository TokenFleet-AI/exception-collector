# Pre-commit 和发布流程配置设计

## 背景

exception-collector 是 TokenFleet-AI 的 Rust 库项目，需要建立代码质量保障和发布流程标准。参考同组织的 tokenless 项目，为 exception-collector 定制化配置。

## 目标

- 建立 pre-commit hooks 保障代码质量
- 实现标准化的发布流程（crates.io + GitHub Release）
- 自动生成 CHANGELOG
- 针对库项目优化配置

## 设计决策

### Pre-commit Hooks

**保留的配置（来自 tokenless）：**
- 基础检查（BOM、合并冲突、YAML、行尾、尾随空格）
- cargo-fmt（格式化检查）
- cargo-check（编译检查）
- cargo-clippy（pedantic 级别 lint）
- cargo-deny（依赖许可检查）
- cargo-audit（安全漏洞检查）
- typos（拼写检查）
- gitleaks（密钥泄露检测）

**针对库项目的调整：**
- ✅ cargo-test 在 pre-commit 自动运行（不是 manual stage）
- ✅ 添加 cargo-doc 检查文档完整性（库项目必须有完整文档）
- ✅ 移除 check-agent-sync（不需要 AGENTS.md）
- ✅ 移除 black（Python 工具）

### Makefile 发布流程

**核心命令：**
- `make lint`：运行所有检查（fmt, clippy, audit, doc-check）
- `make release VERSION=patch|minor|major`：两步式发布第一步
- `make release-publish`：两步式发布第二步，发布到 crates.io

**关键差异（vs tokenless）：**
- 移除 `--workspace`（exception-collector 是单 crate，不是 workspace）
- 移除 adapter-install 等特定命令
- 添加 `doc-check` 命令
- 推送分支改为 `main`（tokenless 是 `master`）

### 文档更新

- CLAUDE.md：添加发布流程和工具说明
- 新增 CHANGELOG.md（git cliff 生成）
- 不需要 AGENTS.md

### 必需工具

- cargo-nextest：测试运行器
- cargo-release：发布工具
- git-cliff：CHANGELOG 生成
- cargo-deny：依赖检查
- cargo-audit：安全审计
- typos-cli：拼写检查

## 配置清单

### 1. .pre-commit-config.yaml

包含以下 hooks：
- pre-commit-hooks（基础检查）
- cargo-fmt
- cargo-check
- cargo-clippy（pedantic）
- cargo-deny
- cargo-audit
- typos
- gitleaks
- cargo-test（pre-commit stage）
- cargo-doc

### 2. Makefile

包含以下 targets：
- lint, fmt, clippy, audit, doc-check, test
- release, release-push, release-publish
- clean

### 3. CLAUDE.md 更新

添加：
- 发布流程说明
- Pre-commit 配置说明
- 必需工具列表

### 4. .gitignore 更新

添加：
- CHANGELOG.md（可选，如果不想提交自动生成的）

## 验收标准

- [ ] pre-commit hooks 安装成功
- [ ] 所有 hooks 运行通过
- [ ] Makefile 发布流程可用
- [ ] CHANGELOG.md 可生成
- [ ] CLAUDE.md 更新完成
