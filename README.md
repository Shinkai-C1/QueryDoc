# QueryDoc

本地离线桌面应用：上传扫描版 PDF → PP-OCRv6 内嵌解析 → LLM 关键词提取
（本地 Ollama 或 OpenAI 兼容 API）→ 表格核对（单元格与 PDF 原文单字级联动高亮）→
导出 xlsx / zip。另含内控测试工作区（流程图建模、制度文件关联、人员名册管理）。

A local-first desktop app that turns scanned PDFs into structured data:
embedded PP-OCRv6 parsing, LLM keyword extraction (Ollama or any OpenAI-compatible API),
cell-to-source cross-highlighting, and xlsx/zip export.

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![CI](https://github.com/Shinkai-C1/QueryDoc/actions/workflows/tauri-build.yml/badge.svg)](https://github.com/Shinkai-C1/QueryDoc/actions/workflows/tauri-build.yml)

> **项目性质**：这是个人首个开源仓库，属于 **vibe coding 的 demo 产物**——核心功能可以跑通，
> 但稳定性、错误处理、测试覆盖等均无法与成熟产品相比。仅作技术验证与分享，
> 按"现状"提供，**不接受 Issue、PR 等任何形式的协作贡献**，也无长期维护承诺。

## 功能特性

- **解析提取**：批量上传 PDF → OCR 解析 → 配置关键词 → LLM 提取（引用未命中原文时单元格标「疑」提示人工复核）→ 表格与 PDF 预览单字级联动 → 导出。
- **PDF 预览**：多页懒渲染、缩放、CJK 字体映射与扫描件（JBIG2）解码，单元格点击跳转原文位置。
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
