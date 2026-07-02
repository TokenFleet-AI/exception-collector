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

# ── Release ──────────────────────────────────────────────────────────
# Three-step flow:
#   make release                    → tag + CHANGELOG + push（不修改版本号）
#   make release-publish            → crates.io 发布
#   make bump VERSION=patch|minor   → 发布成功后 bump 版本号

CURRENT_VERSION := $(shell grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')

release: release-push ## Step 1: 用当前版本号打 tag + 生成 CHANGELOG + 推送
	@echo ""
	@echo "==> ✅ Step 1 完成: tag v$(CURRENT_VERSION) 已推送"
	@echo "==> 请等待 GitHub Actions CI 通过"
	@echo "==> 查看 CI 状态: gh run list --limit 1"
	@echo "==> CI 通过后执行: make release-publish"
	@echo "==> 发布成功后执行: make bump VERSION=patch|minor"

release-push: ## Step 1: 生成 CHANGELOG、创建 tag（不修改版本号）、推送
	@echo "📦 准备发布 v$(CURRENT_VERSION)..."
	@git cliff --tag "v$(CURRENT_VERSION)" -o CHANGELOG.md
	@git commit -a -n -m "chore: update CHANGELOG for v$(CURRENT_VERSION)" || true
	@git tag -a "v$(CURRENT_VERSION)" -m "Release v$(CURRENT_VERSION)"
	@git push origin main --tags
	@echo "✅ tag v$(CURRENT_VERSION) 已创建并推送"

release-publish: ## Step 2: 发布到 crates.io（CI 通过后执行）
	@cargo release publish --execute --no-confirm

bump: ## Step 3: 发布成功后升级版本号（Usage: make bump VERSION=patch|minor|major）
ifndef VERSION
	$(error Usage: make bump VERSION=patch|minor|major)
endif
	@cargo release version $(VERSION) --execute --no-confirm
	@cargo release commit --execute --no-confirm
	@git push origin main
	@echo "✅ 版本号已升级并推送"

# 清理
clean:
	@cargo clean

.PHONY: build test fmt clippy audit lint doc-check clean release release-push release-publish bump
