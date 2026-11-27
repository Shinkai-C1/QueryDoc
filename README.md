# QueryDoc

本地桌面应用：上传扫描版 PDF → PP-OCRv6 内嵌解析 → LLM 关键词提取
（本地 Ollama 或 OpenAI 兼容 API）→ 表格核对（单元格与 PDF 原文单字级联动高亮）→
导出 xlsx / zip。

具体应用操作详见[QueryDoc使用说明](./doc/QueryDoc使用说明.md)

A local-first desktop app that turns scanned PDFs into structured data:
embedded PP-OCRv6 parsing, LLM keyword extraction (Ollama or any OpenAI-compatible API),
cell-to-source cross-highlighting, and xlsx/zip export.

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![CI](https://github.com/Shinkai-C1/QueryDoc/actions/workflows/tauri-build.yml/badge.svg)](https://github.com/Shinkai-C1/QueryDoc/actions/workflows/tauri-build.yml)

> [!WARNING]
> 本项目属于 **vibe coding 的 demo 产物**，核心功能已实现，但稳定性等无法确保与成熟产品一致，仅作分享。

## 项目背景
传统财务审计执行细节测试的场景：执业人员同时打开excel底稿与相关附件（pdf或照片），手工摘录/OCR提取后复制至底稿，并完善整理成表格。存在以下几类问题：

- 在Excel 底稿与各类附件（PDF、图片等）之间频繁切换窗口，打开、关闭与检索操作，交互效率低。
- 手工摘录或OCR辅助复制，重复性的数据搬运，严重挤压了执业人员发挥职业判断时间窗口。
- 成果底稿中的明细数据与底层原始凭证缺乏关联（基本仅靠执业人员底稿标注索引的精细程度），无法快速追溯数据在原文件中的具体位置，易导致审计证据链条断裂，增加了复核时间成本与审计风险
- 附件归档与底稿索引的编制高度依赖执业人员核对，附件索引的编制与维护繁琐且易因漏记、错记导致索引失效。

为此构建一个“底稿-附件一体化、数据提取自动可追溯”操作平台十分必要。

## 功能特性

- **解析提取**：批量上传 PDF → OCR 解析 → 配置关键词 → LLM 提取（引用未命中原文时单元格标「疑」提示人工复核）→ 表格与 PDF 预览单字级联动 → 导出。
- **PDF 预览**：提取结果单元格点击可跳转原文位置，供人工追溯快速复核，形成人机回环。
- **离线优先**：OCR 引擎（ONNX Runtime + PP-OCRv6 small）随包内置；LLM 可选本地 Ollama（默认）或 OpenAI 兼容 API。
- **数据本地**：元数据存 SQLite，文件存用户选择的项目目录，无登录、不上传。

## 架构

```text
Tauri v2 桌面应用（单进程）
├── 前端：Angular 21 + Clarity v18（webview）
└── Rust 核心（Tauri Commands + tokio 串行任务队列）
    ├── projects / files / queue / settings
    ├── ocr（PP-OCRv6 ONNX：方向分类 → 仿射矫正 → det → rec → CTC 单字框）
    ├── llm（OpenAI Chat Completions 协议；schema-first 提取 + 引用校验）
    └── export（rust_xlsxwriter + zip）
```

## 运行与构建

环境：macOS（Apple Silicon）/ Windows x64；开发需 Node.js ≥ 22（pnpm）、Rust 1.98.0。

```bash
make setup   # 安装依赖并下载 OCR 模型与运行时（约 200MB，sha256 校验）
make dev     # 开发模式（热重载）
make build   # 生产构建（macOS 输出 dmg）
make check   # cargo fmt/clippy + ng lint
```

网络受限时：模型可走镜像 `HF_ENDPOINT=https://hf-mirror.com make models`；
运行时下载遵循标准代理变量 `HTTPS_PROXY=...`。

## 许可证

代码以 [MIT](LICENSE) 开源。第三方组件保留各自许可证（PaddleOCR / pdf.js 为 Apache-2.0，
ONNX Runtime 为 MIT，PDFium 为 BSD-3-Clause），详见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。
