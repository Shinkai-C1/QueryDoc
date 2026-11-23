import { Injectable, inject } from "@angular/core";
import {
  getDocument,
  GlobalWorkerOptions,
  type PDFDocumentLoadingTask,
  type PDFDocumentProxy,
  type RenderTask,
} from "pdfjs-dist";
import { BackendService } from "./backend.service";

/**
 * pdf.js 渲染封装：文档加载（经后端受控读字节）与单页渲染。
 *
 * worker 路径：pdf.worker.min.mjs 经 angular.json assets 配置拷贝到
 * dist/app/browser/assets/（tauri frontendDist 指向此处），dev server 与
 * 生产构建同路径可用（esbuild 不支持 Vite 风格 `?url` 导入后缀）。
 *
 * 资源包（wasm/cmaps/standard_fonts 同样经 angular.json 拷贝到 assets/）：
 * - wasmUrl：JBIG2/JPX 等图像的 wasm 解码器；缺失时扫描件图像静默解码失败，
 *   页面渲染为空白（Scofx-936 类 JBIG2 扫描件第 2 页起全空白的根因）。
 * - cMapUrl / standardFontDataUrl：中文合同常用非内嵌 CJK 字体与标准字体，
 *   缺失时文字缺字形/乱码。
 * 生产环境两项配套（自诊断实测定位）：
 * - tauri.conf.json CSP script-src 必须含 'wasm-unsafe-eval'，否则 WebAssembly
 *   被禁止，JBIG2 落入纯 JS 回退（全页扫描慢到等同挂起）。
 * - tauri:// 自定义协议下 pdf.js 自动判定 useWorkerFetch=false（仅认 http(s)），
 *   其主线程取数回退路径在本环境取不到 wasm 字节，故显式强制 worker 直接
 *   fetch（Windows 生产 http://tauri.localhost 本就自动走该路径）。
 */
@Injectable({ providedIn: "root" })
export class PdfRenderService {
  private readonly backend = inject(BackendService);

  private doc: PDFDocumentProxy | null = null;
  private loadingTask: PDFDocumentLoadingTask | null = null;
  private fileId: number | null = null;
  /** 字节源文档去重键（内控制度文件等 loadBytes 入口；与 fileId 互斥）。 */
  private bytesKey: string | null = null;
  private renderTask: RenderTask | null = null;
  /** 加载序号：并发 load 时过期完成者自弃（快速连点不同文件的竞态防护） */
  private loadSeq = 0;

  constructor() {
    GlobalWorkerOptions.workerSrc = "/assets/pdf.worker.min.mjs";
  }

  /** getDocument 公共参数：资源包 URL（见类注释）。 */
  private docParams(data: Uint8Array) {
    return {
      data,
      cMapUrl: "/assets/cmaps/",
      cMapPacked: true,
      standardFontDataUrl: "/assets/standard_fonts/",
      wasmUrl: "/assets/wasm/",
      // tauri:// 自定义协议下 pdf.js 自动判定 useWorkerFetch=false（仅认 http(s)），
      // 其主线程取数回退路径在本环境取不到 wasm 字节，JBIG2 解码挂起；
      // worker 直接 fetch 经自诊断验证可用（Windows 生产 http://tauri.localhost
      // 本就自动走该路径），故显式强制。
      useWorkerFetch: true,
    };
  }

  get pageCount(): number {
    return this.doc?.numPages ?? 0;
  }

  get loadedFileId(): number | null {
    return this.fileId;
  }

  /** 加载源文件 PDF（重复加载同一文件直接复用）；返回页数。 */
  async load(sourceFileId: number): Promise<number> {
    if (this.doc && this.fileId === sourceFileId) {
      return this.doc.numPages;
    }
    await this.close();
    const seq = ++this.loadSeq;
    const path = await this.backend.readSourcePdfPath(sourceFileId);
    const bytes = await this.backend.readFileBytes(path);
    const task = getDocument(this.docParams(new Uint8Array(bytes)));
    const doc = await task.promise;
    if (seq !== this.loadSeq) {
      // 期间已有更新的加载：销毁这份过期文档
      await task.destroy();
      throw new Error("PDF 加载已被更新的请求取代");
    }
    this.loadingTask = task;
    this.doc = doc;
    this.fileId = sourceFileId;
    this.bytesKey = null;
    return doc.numPages;
  }

  /**
   * 加载字节源 PDF（内控测试制度文件等任意路径；重复加载同一 docKey 直接复用）。
   * 新增入口：不改 load() 的既有语义（load() 仍按 sourceFileId 去重）。
   */
  async loadBytes(bytes: number[], docKey: string): Promise<number> {
    if (this.doc && this.bytesKey === docKey) {
      return this.doc.numPages;
    }
    await this.close();
    const seq = ++this.loadSeq;
    const task = getDocument(this.docParams(new Uint8Array(bytes)));
    const doc = await task.promise;
    if (seq !== this.loadSeq) {
      await task.destroy();
      throw new Error("PDF 加载已被更新的请求取代");
    }
    this.loadingTask = task;
    this.doc = doc;
    this.fileId = null;
    this.bytesKey = docKey;
    return doc.numPages;
  }

  /**
   * 渲染一页到 canvas；返回该页 CSS 像素尺寸（= 高亮层坐标换算基准）。
   * 按 devicePixelRatio 放大位图保证清晰度；上一轮未完成 render 先取消。
   * `intent: "print"`（可选，默认 "display" 不变）：改用微任务调度而非 rAF，
   * 页面不可见（rAF 停火）时也能完成渲染——内控预览在树点击后自动渲染需要它。
   */
  async renderPage(
    canvas: HTMLCanvasElement,
    pageNum: number,
    scale: number,
    intent: "display" | "print" = "display",
  ): Promise<{ width: number; height: number }> {
    if (!this.doc) throw new Error("PDF 未加载");
    const page = await this.doc.getPage(pageNum);
    const viewport = page.getViewport({ scale });
    const dpr = window.devicePixelRatio || 1;
    canvas.width = Math.floor(viewport.width * dpr);
    canvas.height = Math.floor(viewport.height * dpr);
    canvas.style.width = `${Math.floor(viewport.width)}px`;
    canvas.style.height = `${Math.floor(viewport.height)}px`;
    const ctx = canvas.getContext("2d");
    if (!ctx) throw new Error("无法创建 canvas 2d 上下文");

    this.renderTask?.cancel();
    const task = page.render({
      canvas,
      viewport,
      transform: dpr !== 1 ? [dpr, 0, 0, dpr, 0, 0] : undefined,
      intent,
    });
    this.renderTask = task;
    try {
      await task.promise;
    } finally {
      if (this.renderTask === task) {
        this.renderTask = null;
      }
    }
    return { width: viewport.width, height: viewport.height };
  }

  /** 「适合宽度」缩放：容器 CSS 宽 / 页面 1.0 宽。 */
  async fitWidthScale(pageNum: number, containerCssWidth: number): Promise<number> {
    if (!this.doc) throw new Error("PDF 未加载");
    const page = await this.doc.getPage(pageNum);
    const base = page.getViewport({ scale: 1 });
    return base.width > 0 ? containerCssWidth / base.width : 1;
  }

  async close(): Promise<void> {
    this.loadSeq += 1; // 作废旧加载
    this.renderTask?.cancel();
    this.renderTask = null;
    if (this.loadingTask) {
      await this.loadingTask.destroy();
      this.loadingTask = null;
      this.doc = null;
      this.fileId = null;
      this.bytesKey = null;
    }
  }
}
