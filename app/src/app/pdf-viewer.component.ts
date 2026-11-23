import {
  AfterViewInit,
  CUSTOM_ELEMENTS_SCHEMA,
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  OnDestroy,
  QueryList,
  ViewChild,
  ViewChildren,
  computed,
  effect,
  inject,
  signal,
} from "@angular/core";
import { ClarityIcons } from "@cds/core/icon";
import { zoomInIcon } from "@cds/core/icon/shapes/zoom-in.js";
import { zoomOutIcon } from "@cds/core/icon/shapes/zoom-out.js";
import "@cds/core/icon/register.js";
import { AppStateService } from "./app-state.service";
import { errText } from "./error-text";
import { MidEllipsisPipe } from "./middle-ellipsis.pipe";
import { PdfRenderService } from "./pdf-render.service";

ClarityIcons.addIcons(zoomInIcon, zoomOutIcon);

interface CssRect {
  left: number;
  top: number;
  width: number;
  height: number;
}

/** pdf.js 渲染取消异常（缩放/切换打断上一轮渲染，属正常流程）。 */
function isRenderCancelled(e: unknown): boolean {
  return (
    typeof e === "object" &&
    e !== null &&
    (e as { name?: string }).name === "RenderingCancelledException"
  );
}

/**
 * PDF 预览面板：全页连续滚动 + 懒渲染（IntersectionObserver）+ 单字级联动高亮。
 *
 * 联动时序：state.pdfSourceFileId 或 flashSeq 变化 → syncView() → 需要时加载文档
 * → 目标页 scrollIntoView（同时直接入队渲染该页，不依赖 observer 时序）
 * → 渲染完成后红框 + 淡黄底高亮（常显，不闪烁）。
 * 高亮层为目标页 wrap 内绝对定位 div，坐标 0–1000 归一化映射到该页 CSS 尺寸。
 */
@Component({
  selector: "app-pdf-viewer",
  changeDetection: ChangeDetectionStrategy.OnPush,
  schemas: [CUSTOM_ELEMENTS_SCHEMA],
  imports: [MidEllipsisPipe],
  templateUrl: "./pdf-viewer.component.html",
  styleUrl: "./pdf-viewer.component.css",
})
export class PdfViewerComponent implements AfterViewInit, OnDestroy {
  protected readonly state = inject(AppStateService);
  private readonly pdf = inject(PdfRenderService);

  @ViewChild("scrollRef") scrollRef?: ElementRef<HTMLDivElement>;
  @ViewChildren("pageWrap") pageWraps?: QueryList<ElementRef<HTMLDivElement>>;
  @ViewChildren("pageCanvas") pageCanvases?: QueryList<
    ElementRef<HTMLCanvasElement>
  >;

  /** 页码列表（1..N）。 */
  readonly pages = signal<number[]>([]);
  readonly pageCount = computed(() => this.pages().length);
  /** 各页已渲染的 CSS 尺寸（高亮换算基准；懒渲染前无条目）。 */
  readonly pageSizes = signal<ReadonlyMap<number, { w: number; h: number }>>(
    new Map(),
  );
  /** 当前可见页（滚动推导，仅作工具栏指示）。 */
  readonly currentPage = signal(1);
  readonly loading = signal(false);
  readonly loadError = signal<string | null>(null);
  /** null = 适合宽度（默认）；数字 = 显式缩放 */
  readonly zoom = signal<number | null>(null);
  /** 最近实际生效的缩放（放大/缩小的基准） */
  readonly lastScale = signal(1);

  private renderSeq = 0;
  /** 渲染版本：缩放/适宽变化时 ++，过期渲染结果丢弃、可见页重渲。 */
  private renderVersion = 0;
  private readonly renderedVersion = new Map<number, number>();
  private readonly renderQueue: number[] = [];
  private rendering = false;
  /** 目标页待滚动（文档加载后 wraps 尚未渲染时暂存）。 */
  private pendingScrollPage: number | null = null;
  private observer: IntersectionObserver | null = null;
  private resizeObserver: ResizeObserver | null = null;
  private resizeTimer: ReturnType<typeof setTimeout> | null = null;
  private scrollRaf = 0;

  /** 预览文件名（工具栏展示） */
  readonly fileName = computed(() => {
    const id = this.state.pdfSourceFileId();
    return this.state.files().find((f) => f.id === id)?.original_name ?? "";
  });

  /** 选中单元格所在页（1 起始）；null = 无坐标选中。 */
  readonly highlightPage = computed(() => {
    const sel = this.state.selectedCell();
    if (!sel || sel.page_idx == null) return null;
    if (sel.sourceFileId !== this.state.pdfSourceFileId()) return null;
    return sel.page_idx + 1;
  });

  /** 选中单元格的 fragments 换算为目标页 CSS 矩形（该页渲染完成后才有值） */
  readonly highlightRects = computed<CssRect[]>(() => {
    const sel = this.state.selectedCell();
    const page = this.highlightPage();
    if (!sel || sel.page_idx == null || page == null) return [];
    const size = this.pageSizes().get(page);
    if (!size) return [];
    return sel.fragments.map((f) => ({
      left: (f[0] / 1000) * size.w,
      top: (f[1] / 1000) * size.h,
      width: Math.max(2, ((f[2] - f[0]) / 1000) * size.w),
      height: Math.max(2, ((f[3] - f[1]) / 1000) * size.h),
    }));
  });

  constructor() {
    // 联动入口：换文件（pdfSourceFileId）或点击单元格（flashSeq）
    effect(() => {
      this.state.pdfSourceFileId();
      this.state.flashSeq();
      void this.syncView();
    });
  }

  ngAfterViewInit(): void {
    const el = this.scrollRef?.nativeElement;
    this.observer = new IntersectionObserver(
      (entries) => {
        for (const e of entries) {
          if (e.isIntersecting) {
            const n = Number((e.target as HTMLElement).dataset["page"]);
            if (Number.isInteger(n)) this.enqueueRender(n);
          }
        }
      },
      { root: el, rootMargin: "300px 0px" },
    );
    // 页 wrap 列表变化（新文档 pages() 更新）后重新挂载观察
    this.pageWraps?.changes.subscribe(() => this.reobserve());
    // 适合宽度模式下容器尺寸变化 → 可见页重渲（防抖）
    if (el && typeof ResizeObserver !== "undefined") {
      this.resizeObserver = new ResizeObserver(() => this.onContainerResize());
      this.resizeObserver.observe(el);
    }
  }

  ngOnDestroy(): void {
    this.observer?.disconnect();
    this.resizeObserver?.disconnect();
    if (this.resizeTimer) clearTimeout(this.resizeTimer);
    if (this.scrollRaf) cancelAnimationFrame(this.scrollRaf);
  }

  /** 联动主编排：换文件/点单元格时加载、滚动到目标页。 */
  private async syncView(): Promise<void> {
    const seq = ++this.renderSeq;
    const fileId = this.state.pdfSourceFileId();
    if (fileId == null) {
      this.pages.set([]);
      this.pageSizes.set(new Map());
      this.renderedVersion.clear();
      await this.pdf.close();
      return;
    }
    const sel = this.state.selectedCell();
    const selPage =
      sel && sel.sourceFileId === fileId && sel.page_idx != null
        ? sel.page_idx
        : null;

    if (this.pdf.loadedFileId !== fileId) {
      this.loading.set(true);
      this.loadError.set(null);
      try {
        const n = await this.pdf.load(fileId);
        if (seq !== this.renderSeq) return;
        // 清空旧文档残留位图（页数相同时 @for 复用 wrap/canvas，旧画面会残留）
        for (const c of this.pageCanvases ?? []) {
          c.nativeElement.width = 0;
          c.nativeElement.height = 0;
        }
        this.pages.set(Array.from({ length: n }, (_, i) => i + 1));
        this.pageSizes.set(new Map());
        this.renderedVersion.clear();
        this.renderVersion++;
        this.currentPage.set(1);
      } catch (e) {
        if (seq === this.renderSeq) {
          this.loadError.set(`PDF 加载失败: ${errText(e)}`);
          this.pages.set([]);
          this.pageSizes.set(new Map());
          this.loading.set(false);
        }
        return;
      }
      this.loading.set(false);
      // wraps 由 @for 异步渲染：先登记目标页，待 reobserve（wraps 就绪）时滚动
      if (selPage != null) {
        this.pendingScrollPage = selPage + 1;
      }
      // @for 按页码 track：页数相同的文档切换不会触发 changes，
      // 必须显式重挂 observer 并触发可见页重渲染
      setTimeout(() => {
        if (seq === this.renderSeq) this.reobserve();
      });
    } else if (this.pages().length === 0 && this.pdf.pageCount > 0) {
      // 组件被 @if 销毁重建（收起/展开窗格、进出任务页）：文档仍在 service
      // 单例中，但 pages/canvas 已重置——重建页列表并强制重渲，否则空白
      this.pages.set(
        Array.from({ length: this.pdf.pageCount }, (_, i) => i + 1),
      );
      this.pageSizes.set(new Map());
      this.renderedVersion.clear();
      this.renderVersion++;
      if (selPage != null) {
        this.pendingScrollPage = selPage + 1;
      }
      setTimeout(() => {
        if (seq === this.renderSeq) this.reobserve();
      });
    } else if (selPage != null) {
      this.scrollToPage(selPage + 1);
    }
  }

  /** 滚动到指定页并直接入队渲染（不等 observer，保证跳转与高亮及时生效）。 */
  private scrollToPage(page: number): void {
    this.enqueueRender(page);
    const wrap = this.pageWraps?.get(page - 1)?.nativeElement;
    wrap?.scrollIntoView({ block: "start" });
    this.currentPage.set(page);
  }

  // ---------------------------------------------------------------------------
  // 懒渲染
  // ---------------------------------------------------------------------------

  private reobserve(): void {
    if (!this.observer) return;
    this.observer.disconnect();
    for (const wrap of this.pageWraps ?? []) {
      this.observer.observe(wrap.nativeElement);
    }
    // 初始可见页立即入队（首屏不必等 observer 首轮回调）
    this.enqueueVisiblePages();
    // wraps 就绪：消费暂存的目标页（新文档加载后的联动跳转）
    if (this.pendingScrollPage != null) {
      const page = this.pendingScrollPage;
      this.pendingScrollPage = null;
      this.scrollToPage(page);
    }
  }

  private enqueueRender(n: number): void {
    if (n < 1 || n > this.pageCount()) return;
    if (this.renderedVersion.get(n) === this.renderVersion) return;
    if (this.renderQueue.includes(n)) return;
    this.renderQueue.push(n);
    void this.processQueue();
  }

  /** 可见（含缓冲）页全部入队（缩放/适宽变化后的重渲入口）。 */
  private enqueueVisiblePages(): void {
    const container = this.scrollRef?.nativeElement;
    if (!container) return;
    const crect = container.getBoundingClientRect();
    for (const wrap of this.pageWraps ?? []) {
      const r = wrap.nativeElement.getBoundingClientRect();
      if (r.bottom > crect.top - 300 && r.top < crect.bottom + 300) {
        const n = Number(wrap.nativeElement.dataset["page"]);
        if (Number.isInteger(n)) this.enqueueRender(n);
      }
    }
  }

  private async processQueue(): Promise<void> {
    if (this.rendering) return;
    this.rendering = true;
    try {
      while (this.renderQueue.length > 0) {
        const n = this.renderQueue.shift()!;
        if (this.renderedVersion.get(n) === this.renderVersion) continue;
        await this.renderPage(n);
      }
    } finally {
      this.rendering = false;
    }
  }

  private async renderPage(n: number): Promise<void> {
    const canvas = this.pageCanvases?.get(n - 1)?.nativeElement;
    if (!canvas || this.pdf.pageCount === 0) return;
    const version = this.renderVersion;
    try {
      const scale = await this.resolveScale(n);
      const size = await this.pdf.renderPage(canvas, n, scale);
      if (version !== this.renderVersion) return; // 缩放已变，结果丢弃
      this.renderedVersion.set(n, version);
      this.lastScale.set(scale);
      this.pageSizes.update((m) =>
        new Map(m).set(n, { w: size.width, h: size.height }),
      );
    } catch (e) {
      if (isRenderCancelled(e)) return;
      this.loadError.set(`第 ${n} 页渲染失败: ${errText(e)}`);
    }
  }

  private async resolveScale(page: number): Promise<number> {
    const z = this.zoom();
    if (z != null) return z;
    const containerW = (this.scrollRef?.nativeElement.clientWidth ?? 0) - 16;
    if (containerW <= 0) return 1;
    return this.pdf.fitWidthScale(page, containerW);
  }

  // ---------------------------------------------------------------------------
  // 滚动与缩放
  // ---------------------------------------------------------------------------

  /** 滚动节流推导当前可见页（工具栏指示用）。 */
  onScroll(): void {
    if (this.scrollRaf) return;
    this.scrollRaf = requestAnimationFrame(() => {
      this.scrollRaf = 0;
      const container = this.scrollRef?.nativeElement;
      if (!container) return;
      const top = container.getBoundingClientRect().top;
      for (const wrap of this.pageWraps ?? []) {
        const r = wrap.nativeElement.getBoundingClientRect();
        if (r.bottom > top + 40) {
          const n = Number(wrap.nativeElement.dataset["page"]);
          if (Number.isInteger(n)) this.currentPage.set(n);
          break;
        }
      }
    });
  }

  private onContainerResize(): void {
    if (this.zoom() !== null || this.pageCount() === 0) return;
    if (this.resizeTimer) clearTimeout(this.resizeTimer);
    this.resizeTimer = setTimeout(() => {
      this.renderVersion++;
      this.enqueueVisiblePages();
    }, 200);
  }

  zoomIn(): void {
    this.zoom.set(Math.min(4, this.lastScale() * 1.25));
    this.renderVersion++;
    this.enqueueVisiblePages();
  }

  zoomOut(): void {
    this.zoom.set(Math.max(0.2, this.lastScale() / 1.25));
    this.renderVersion++;
    this.enqueueVisiblePages();
  }

  /** 重置缩放：回到默认「适合宽度」（默认态下按钮禁用）。 */
  resetZoom(): void {
    this.zoom.set(null);
    this.renderVersion++;
    this.enqueueVisiblePages();
  }

  /** 收起 PDF 窗格（右侧窄条，主表格区让位）。 */
  collapse(): void {
    this.state.pdfCollapsed.set(true);
  }

  /** 点击预览区空白处：清除单元格选中（红色选中框移除）。 */
  onViewerClick(): void {
    this.state.clearSelectedCell();
  }
}
