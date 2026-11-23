# 第三方组件声明（Third-Party Notices）

QueryDoc 本体代码以 [MIT](LICENSE) 许可开源；其集成的下列第三方组件保留
**各自原有许可证**（其中 OCR 模型、pdf.js、rxjs 等为 **Apache-2.0**），
本项目的 MIT 许可不改变它们的授权条款。除特别说明外，随包/下载的二进制
均为**未修改分发**（下载脚本按 sha256 校验，见 `app/scripts/` 与
`app/src-tauri/resources/ocr-models/models.toml`）。

## 下载的模型与运行时（构建/首次运行前由脚本拉取）

| 组件 | 来源 | 许可证 |
| --- | --- | --- |
| PP-OCRv6 small det/rec 模型、PP-LCNet 文档方向分类模型、识别字典 | [PaddlePaddle（HuggingFace 官方 org）](https://huggingface.co/PaddlePaddle) | **Apache-2.0** |
| ONNX Runtime 1.28.0 | [microsoft/onnxruntime](https://github.com/microsoft/onnxruntime) | MIT |
| PDFium（chromium/8035 预编译包） | [bblanchon/pdfium-binaries](https://github.com/bblanchon/pdfium-binaries)（Google PDFium 构建） | BSD-3-Clause（The PDFium Authors） |

Apache-2.0 许可全文见 <https://www.apache.org/licenses/LICENSE-2.0>；
使用上述组件须遵守其许可证要求（保留版权与许可声明）。

## 随包内置的前端资源（pdf.js 发行物）

| 组件 | 许可证 | 说明 |
| --- | --- | --- |
| pdf.js（pdfjs-dist 6.3，Mozilla） | Apache-2.0 | PDF 渲染 |
| jbig2.wasm | BSD-3-Clause（The PDFium Authors） | JBIG2 图像解码 |
| openjpeg.wasm | BSD-2-Clause | JPX 图像解码 |
| qcms.wasm | MIT（Mozilla / Marti Maria） | 色彩管理 |
| CMaps（Adobe） | 宽松许可（须保留声明） | CJK 字符映射 |
| Foxit 标准字体 | BSD-3-Clause（The PDFium Authors） | 非内嵌字体回退渲染 |
| Liberation 字体（Red Hat） | GPLv2 + Font Exception | 仅约束字体文件本身；捆绑/嵌入不感染宿主软件 |

上述 `LICENSE_JBIG2`、`LICENSE_OPENJPEG`、`LICENSE_QCMS`、`LICENSE_FOXIT`、
`LICENSE_LIBERATION` 全文随应用 assets 一并分发。

## 框架与库

- 前端：Angular（MIT）、Clarity（MIT）、@foblex/flow（MIT）、graphology（MIT）、
  sigma.js（MIT）、rxjs（Apache-2.0）、zone.js（MIT）、tslib（0BSD）。
- 桌面框架：Tauri v2（MIT / Apache-2.0 双许可）。
- Rust  crates：以 Cargo.lock 为准，绝大多数为 MIT / Apache-2.0 / BSD / Zlib /
  ISC / Unlicense / CC0 / Unicode-3.0 等宽松许可；少量 MPL-2.0 crate
  （cssparser、selectors、dtoa-short、option-ext、cssparser-macros）以**未修改**
  形式作为编译依赖使用，其源码与许可文本见 crates.io。

各依赖的完整许可文本可在其包目录（`node_modules/<pkg>/LICENSE`、
`~/.cargo/registry/src/**/LICENSE*`）中找到。
