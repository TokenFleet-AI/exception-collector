<!-- Issue: #1 -->
# Pre-commit 和发布流程配置实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 exception-collector 项目配置 pre-commit hooks 和标准化发布流程

**Architecture:** 基于 tokenless 项目的配置，针对库项目定制化调整。使用 pre-commit 框架管理 git hooks，Makefile 提供发布命令，git cliff 生成 CHANGELOG。

**Tech Stack:** Rust, pre-commit, cargo-release, git-cliff, cargo-nextest, cargo-deny, cargo-audit

## Global Constraints

- 保持与 tokenless 项目的一致性，便于团队协作
- 针对库项目优化：cargo-test 在 pre-commit 运行，cargo-doc 检查文档
- 使用 pedantic 级别的 clippy 检查
- 不需要 AGENTS.md，简化配置

## GitHub Issue 规划

**Issue 标题:** feat: 配置 pre-commit hooks 和发布流程

**Issue 标签:** enhancement,infrastructure,priority:high

**Issue 描述:**
为 exception-collector 项目建立代码质量保障和发布流程标准。配置 pre-commit hooks 包括格式化、编译检查、lint、测试、文档检查等。实现两步式发布流程支持 crates.io 和 GitHub Release。自动生成 CHANGELOG。

**验收标准:**
- [ ] pre-commit hooks 安装成功并运行通过
- [ ] Makefile 发布流程可用
- [ ] CHANGELOG.md 可生成
- [ ] CLAUDE.md 更新完成
- [ ] 所有测试通过

**关联:**
- 计划文件: `docs/superpowers/plans/2026-07-01-precommit-release-setup.md`
- 参考项目: `../tokenless`

## File Structure

```
exception-collector/
├── .pre-commit-config.yaml          # 新增
├── Makefile                          # 新增
├── CHANGELOG.md                      # 新增（git cliff 生成）
├── CLAUDE.md                         # 更新
└── .gitignore                        # 更新（添加 .claude/gh-issue/）
```

## Tasks

### Task 1: 创建 Issue

**Description:** 从 "Issue 规划" 部分提取信息，创建 Issue 并保存编号到 `.claude/gh-issue/current-issue.txt`。

- [ ] **Step 1: 运行 scripts/create-issue.sh**

```bash
bash /Users/byx/.claude/skills/writing-plans-with-issue/scripts/create-issue.sh docs/superpowers/plans/2026-07-01-precommit-release-setup.md
```

- [ ] **Step 2: 验证 Issue 已创建**

```bash
cat .claude/gh-issue/current-issue.txt
gh issue view "$(cat .claude/gh-issue/current-issue.txt)"
```

### Task 2: 同步 Issue 状态为 in-progress

**Description:** 将 Issue 状态更新为 `status: in-progress`，表示开发已开始。

- [ ] **Step 1: 运行 scripts/sync-status.sh**

```bash
bash /Users/byx/.claude/skills/writing-plans-with-issue/scripts/sync-status.sh in-progress
```

- [ ] **Step 2: 确认**

```bash
echo "✅ Issue #$(cat .claude/gh-issue/current-issue.txt) 已标记为 in-progress"
```

### Task 3: 创建 .pre-commit-config.yaml

**Description:** 创建 pre-commit 配置文件，包含所有必要的 hooks。

- [ ] **Step 1: 创建 .pre-commit-config.yaml 文件**

```yaml
fail_fast: false
default_install_hook_types: [pre-commit, commit-msg]
default_stages: [pre-commit, manual]

repos:
  - repo: https://github.com/pre-commit/pre-commit-hooks
    rev: v6.0.0
    hooks:
      - id: fix-byte-order-marker
      - id: check-case-conflict
      - id: check-merge-conflict
      - id: check-symlinks
      - id: check-yaml
      - id: end-of-file-fixer
      - id: mixed-line-ending
      - id: trailing-whitespace
  - repo: local
    hooks:
      - id: cargo-fmt
        name: cargo fmt
        description: Format files with rustfmt.
        entry: bash -c 'cargo fmt -- --check'
        language: system
        files: \.rs$
        pass_filenames: false
      - id: cargo-check
        name: cargo check
        description: Check the package for errors.
        entry: bash -c 'cargo check --all'
        language: system
        files: \.rs$
        pass_filenames: false
      - id: cargo-clippy
        name: cargo clippy
        description: Lint rust sources
        entry: bash -c 'cargo clippy --all-targets --all-features -- -D warnings -W clippy::pedantic'
        language: system
        files: \.rs$
        pass_filenames: false
      - id: cargo-deny
        name: cargo deny check
        description: Check cargo dependencies
        entry: bash -c 'cargo deny check -d'
        language: system
        files: \.rs$
        pass_filenames: false
      - id: cargo-audit
        name: cargo audit
        description: Check for known security vulnerabilities
        entry: bash -c 'cargo audit'
        language: system
        files: (Cargo\.lock|Cargo\.toml)$
        pass_filenames: false
      - id: typos
        name: typos
        description: check typo
        entry: bash -c 'typos'
        language: system
        files: \.*$
        pass_filenames: false
      - id: cargo-test
        name: cargo test
        description: unit test for the project
        entry: bash -c 'cargo nextest run --all-features'
        language: system
        files: \.rs$
        pass_filenames: false
        stages: [pre-commit]
      - id: cargo-doc
        name: cargo doc
        description: Check documentation completeness
        entry: bash -c 'cargo doc --no-deps --all-features'
        language: system
        files: \.rs$
        pass_filenames: false
  - repo: https://github.com/gitleaks/gitleaks
    rev: v8.18.0
    hooks:
      - id: gitleaks
        args: ["protect", "--staged", "--verbose"]
        stages: [pre-commit]
```

- [ ] **Step 2: 提交文件**

```bash
git add .pre-commit-config.yaml
git commit -m "feat: add pre-commit configuration (#N)"
```

### Task 4: 创建 Makefile

**Description:** 创建 Makefile，包含开发命令和发布流程。

- [ ] **Step 1: 创建 Makefile 文件**

```makefile
# 开发命令
lint: fmt clippy audit doc-check
	@echo "所有检查通过"

fmt:
	@cargo fmt

clippy:
	@cargo clippy --all-targets --all-features -- -D warnings -W clippy::pedantic

audit:
	@cargo audit

doc-check:
	@cargo doc --no-deps --all-features

test:
	@cargo nextest run --all-features

# 发布流程（两步式）
release: release-push ## Usage: make release VERSION=patch|minor|major (step 1)
	@echo ""
	@echo "==> Step 1 完成: 代码已推送并创建 tag"
	@echo "==> 请等待 GitHub Actions CI 通过"
	@echo "==> 查看 CI 状态: gh run list --limit 1"
	@echo "==> CI 通过后执行: make release-publish"

release-push: ## Step 1: 更新版本、提交、生成 CHANGELOG、创建 tag、推送
ifndef VERSION
	$(error Usage: make release-push VERSION=patch|minor|major)
endif
	@cargo release version $(VERSION) --execute --no-confirm
	@cargo release commit --execute --no-confirm
	@git cliff -o CHANGELOG.md
	@git commit -a -n -m "Update CHANGELOG.md" || true
	@cargo release tag --execute --no-confirm
	@git push origin main --tags

release-publish: ## Step 2: 发布到 crates.io（CI 通过后执行）
	@cargo release publish --execute --no-confirm

# 清理
clean:
	@cargo clean

.PHONY: build test fmt clippy audit lint doc-check clean release release-push release-publish
```

- [ ] **Step 2: 提交文件**

```bash
git add Makefile
git commit -m "feat: add Makefile with release workflow (#N)"
```

### Task 5: 更新 CLAUDE.md

**Description:** 更新 CLAUDE.md，添加发布流程和工具说明。

- [ ] **Step 1: 在 CLAUDE.md 末尾添加以下内容**

```markdown
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
```

- [ ] **Step 2: 提交文件**

```bash
git add CLAUDE.md
git commit -m "docs: update CLAUDE.md with release workflow (#N)"
```

### Task 6: 更新 .gitignore

**Description:** 更新 .gitignore，添加 .claude/gh-issue/ 目录。

- [ ] **Step 1: 检查 .gitignore 是否已包含 .claude/gh-issue/**

```bash
grep -q '.claude/gh-issue/' .gitignore && echo "已存在" || echo "需要添加"
```

- [ ] **Step 2: 如果不存在，添加到 .gitignore**

```bash
echo '.claude/gh-issue/' >> .gitignore
```

- [ ] **Step 3: 提交文件**

```bash
git add .gitignore
git commit -m "chore: add .claude/gh-issue/ to .gitignore (#N)"
```

### Task 7: 验证配置

**Description:** 验证所有配置是否正常工作。

- [ ] **Step 1: 安装 pre-commit**

```bash
# 如果未安装
pip install pre-commit

# 安装 hooks
pre-commit install
```

- [ ] **Step 2: 运行所有 pre-commit hooks**

```bash
pre-commit run --all-files
```

- [ ] **Step 3: 验证 Makefile 命令**

```bash
make lint
```

- [ ] **Step 4: 提交验证修复（如果有）**

```bash
# 如果有文件被修改
git add -A
git commit -m "fix: resolve pre-commit issues (#N)"
```

### Task 8: 收尾 — 本地合并后关闭 Issue

**Description:** 开发完成并本地合并到 base 分支后，push 并关闭 Issue。

- [ ] **Step 1: 确保已合并到 base 分支**

```bash
git branch --show-current  # 应该在 main 上
```

- [ ] **Step 2: 运行 scripts/finish-issue.sh**

```bash
bash /Users/byx/.claude/skills/writing-plans-with-issue/scripts/finish-issue.sh
```

- [ ] **Step 3: 确认 Issue 已关闭**

```bash
gh issue view "$(cat .claude/gh-issue/current-issue.txt 2>/dev/null || echo 'already cleaned')"
```
