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
