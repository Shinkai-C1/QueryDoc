# QueryDoc 离线版统一入口
# 新机器克隆仓库后：make setup 即可复原完整开发环境。

APP_DIR := app
CARGO ?= $(HOME)/.cargo/bin/cargo
PNPM ?= pnpm

.PHONY: setup dev build check models

setup:
	cd $(APP_DIR) && $(PNPM) install --frozen-lockfile
	cd $(APP_DIR)/src-tauri && $(CARGO) fetch --locked
	node $(APP_DIR)/scripts/download-models.mjs
	node $(APP_DIR)/scripts/download-runtimes.mjs

models:
	node $(APP_DIR)/scripts/download-models.mjs
	node $(APP_DIR)/scripts/download-runtimes.mjs

dev:
	cd $(APP_DIR) && $(PNPM) tauri dev

# 过滤 macOS 输入法框架的系统级噪音日志（TSM/IMK，WKWebView 文本输入场景触发，
# 属系统框架输出而非应用错误，应用侧无法消除，只能在终端过滤）
dev-q:
	cd $(APP_DIR) && $(PNPM) tauri dev 2>&1 | grep -v -E "TSM AdjustCapsLockLED|IMKCFRunLoopWakeUpReliable|error messaging the mach port"

build:
	cd $(APP_DIR) && $(PNPM) tauri build

check:
	cd $(APP_DIR)/src-tauri && $(CARGO) fmt --check && $(CARGO) clippy --all-targets -- -D warnings
	cd $(APP_DIR) && $(PNPM) ng lint
