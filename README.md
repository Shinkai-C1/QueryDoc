# QueryDoc

本地离线桌面应用：上传 PDF（绝大多数为扫描件）→ PP-OCRv6 内嵌解析（版面/方向/单字坐标）→
LLM 关键词提取（本地 Ollama 或 OpenAI 兼容 API）→ 表格核对（单元格与 PDF 原文单字级联动高亮）→
导出 xlsx / zip。另含内控测试工作区（流程图建模、制度文件关联、人员名册/组织结构管理）。

A local-first, offline desktop app that turns scanned contract PDFs into structured data:
embedded PP-OCRv6 parsing, LLM keyword extraction (local Ollama or any OpenAI-compatible API),
cell-to-source character-level cross-highlighting in the preview, and xlsx/zip export.
Single user, no login — all data stays on your machine.

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![CI](https://github.com/Shinkai-C1/QueryDoc/actions/workflows/tauri-build.yml/badge.svg)](https://github.com/Shinkai-C1/QueryDoc/actions/workflows/tauri-build.yml)

> 截图与演示 GIF 待补充（Screenshots coming soon）。

## 功能特性（Features）

- **解析提取（主流程）**：批量上传 PDF → OCR 解析 → 配置关键词（必填）+ 关键词定义/信息类型（选填，
  作为提示词注入提升准确度）→ LLM 提取（reasoning→quote→value 单次提交；quote 未命中原文时不重试、
  单元格标「疑」提示人工复核，照常返回值召回优先）→ 结果表格（排序/筛选/编辑）与 PDF 预览单字级联动 → 导出。
- **PDF 预览**：多页连续滚动懒渲染、缩放、CJK 字体映射与扫描件（JBIG2）解码，单元格点击跳转原文位置。
- **内控测试工作区**：流程图（@foblex/flow）建模、节点属性面板、制度文件导入与树管理、
  PDF 矩形索引与跳转、人员名册/组织结构/制度关系表。
- **离线优先**：OCR 引擎（ONNX Runtime + PP-OCRv6 small）随包内置，完全离线可用；
  LLM 可选本地 Ollama（默认）或 Kimi 等 OpenAI 兼容 API。
- **数据本地**：元数据存 SQLite（系统应用数据目录），文件存用户选择的项目目录。

## 架构（Architecture）

```text
Tauri v2 桌面应用（单进程）
├── 前端：Angular 21 + Clarity v18（webview）
└── Rust 核心（Tauri Commands + tokio 串行任务队列）
    ├── projects / files / queue / settings
    ├── ocr（PP-OCRv6 ONNX：方向分类 → 仿射矫正 → det → rec → CTC 单字框 → 启发式段落聚类）
    ├── llm（OpenAI Chat Completions 协议；schema-first 提取 + 引用校验）
    └── export（rust_xlsxwriter + zip）
```

## 运行环境（Requirements）

- macOS（Apple Silicon）/ Windows x64（近 3 年 Intel/AMD，16–32 GB 内存推荐）
- 开发：Node.js ≥ 22（corepack 启用 pnpm）、Rust 1.98.0（见 `app/rust-toolchain.toml`）

## 快速开始（Getting Started）

```bash
# 1. 克隆后安装依赖并下载 OCR 模型与运行时（约 200MB，按 sha256 校验）
make setup

# 2. 开发模式（热重载）
make dev

# 3. 生产构建（输出平台安装包，macOS 为 dmg）
make build

# 代码检查（cargo fmt/clippy + ng lint）
make check
```

`make setup` 通过 `app/scripts/download-models.mjs` / `download-runtimes.mjs`
从公开源拉取模型与运行时（清单锁定版本与哈希），也可单独运行 `make models`。

网络受限环境提示：
- 模型默认从 HuggingFace 下载；直连受限时可走镜像
  `HF_ENDPOINT=https://hf-mirror.com make models`。
- 运行时下载脚本调用 curl，遵循标准代理变量：`HTTPS_PROXY=http://127.0.0.1:7890 make models`。

首次使用：启动后新建任务 → 上传 PDF → 解析 →「编辑关键词」配置提取列
（关键词必填；定义与信息类型选填）→ 提取 → 在表格中核对（点击单元格跳转原文）→ 导出。
LLM 服务在应用内「设置」配置：本地 Ollama（无需 Key）或 KIMI-API（OpenAI 兼容，填 sk- Key）。

## 项目结构（Layout）

```
app/                  # 前端（Angular）与桌面壳
├── src/              #   Angular 组件（任务页/提取表格/PDF 预览/关键词弹窗/audit 工作区）
├── scripts/          #   模型与运行时下载校验脚本
└── src-tauri/        # Rust 后端
    ├── src/          #   commands / queue / ocr / llm / export / ic（内控）
    ├── resources/    #   OCR 模型清单（models.toml，二进制由脚本下载）
    └── tests/        #   IPC 集成测试（单元测试内嵌于 src 各模块）
docs/                 # 设计与使用文档
```

## 文档（Docs）

- [安装与运行环境说明](docs/安装与运行环境说明.md)
- [前端颜色要求](docs/前端颜色要求.md)、[提示框前端设计要点](docs/提示框前端设计要点.md)

## 贡献（Contributing）

欢迎 Issue 与 PR。提交前请本地通过 `make check`（cargo fmt/clippy + ng lint）与
`cargo test`（单元测试内嵌于 Rust 各模块）。CI 会对 main 分支与 PR 执行完整检查与双平台构建。

## 项目定位（Project Status）

个人兴趣开发的演示项目：验证实务场景中 OCR 与 LLM 结合的可行性（扫描件解析、
关键词提取、原文联动核对等），demo 性质为主，无商业用途与长期维护承诺，
按"现状"提供（见许可证免责条款）。

## 许可证（License）

本项目代码以 [MIT](LICENSE) 开源。
集成的第三方组件保留其各自许可证——其中 OCR 模型（PaddleOCR）、pdf.js 等
为 **Apache-2.0**，ONNX Runtime 为 MIT，PDFium 为 BSD-3-Clause——
详见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。
