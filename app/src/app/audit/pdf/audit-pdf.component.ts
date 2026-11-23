import {
  AfterViewInit,
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  HostListener,
  OnDestroy,
  ViewChild,
  computed,
  effect,
  inject,
  signal,
} from "@angular/core";
import { IcDocument } from "../../backend.models";
import { errText } from "../../error-text";
import { BackendService } from "../../backend.service";
import { PdfRenderService } from "../../pdf-render.service";
import { AuditStateService } from "../audit-state.service";
import { MiniMenuComponent } from "./mini-menu.component";
import { DocLinkPickerComponent } from "./doc-link-picker.component";

/** pdf.js 渲染取消异常（缩放/切页打断上一轮渲染，属正常流程）。 */
function isRenderCancelled(e: unknown): boolean {
  return (
    typeof e === "object" &&
    e !== null &&
    (e as { name?: string }).name === "RenderingCancelledException"
  );
}

/** CSS 像素矩形（覆盖层/框选用）。 */
interface CssRect {
  x0: number;
  y0: number;
  x1: number;
  y1: number;
}

/**
 * 内控测试 PDF 预览（独立组件，不改 PdfViewerComponent）：
 * 单页渲染（页码切换 + 适合宽度/缩放）+ 鼠标框选（0–1000 归一化）+
 * mini 菜单（添加节点/制度索引）+ 索引矩形点击跳转 + 前进/后退导航栈。
 * 渲染能力复用 PdfRenderService（loadBytes 字节入口）。
 */
@Component({
  selector: "app-audit-pdf",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MiniMenuComponent, DocLinkPickerComponent],
  templateUrl: "./audit-pdf.component.html",
  styleUrl: "./audit-pdf.component.css",
})
export class AuditPdfComponent implements AfterViewInit, OnDestroy {
  /** 点击组件外任意位置 / 切换窗口 → 关闭 mini 菜单/选择器/驻留框选。 */
  @HostListener("document:mousedown", ["$event"])
  onDocMouseDown(e: MouseEvent): void {
    if (!this.menuPos() && !this.pickerOpen() && !this.pendingRect()) return;
    const target = e.target as HTMLElement;
    if (!target.closest("app-audit-pdf")) {
      this.dismissOverlays();
    }
  }

  @HostListener("window:blur")
  onWindowBlur(): void {
    this.dismissOverlays();
  }

  protected readonly state = inject(AuditStateService);
  private readonly pdf = inject(PdfRenderService);
  private readonly backend = inject(BackendService);

  @ViewChild("scrollRef") scrollRef?: ElementRef<HTMLDivElement>;
  @ViewChild("canvasWrap") canvasWrap?: ElementRef<HTMLDivElement>;
  @ViewChild("pageCanvas") pageCanvas?: ElementRef<HTMLCanvasElement>;

  readonly loading = signal(false);
  readonly loadError = signal<string | null>(null);
  readonly pageCount = signal(0);
  readonly currentPage = signal(1);
  /** null = 适合宽度（默认）；数字 = 显式缩放 */
  readonly zoom = signal<number | null>(null);
  /** 最近实际生效的缩放（放大/缩小基准）。 */
  readonly lastScale = signal(1);
  /** 当前页已渲染 CSS 尺寸（0–1000 归一化坐标换算基准）。 */
  readonly pageCss = signal<{ w: number; h: number } | null>(null);

  // ---- 框选与 mini 菜单 ----
  /** 拖动中的框选矩形（CSS 像素，相对画布容器）。 */
  readonly dragRect = signal<CssRect | null>(null);
  /** 已完成的待处理框选矩形（0–1000 归一化 [x0,y0,x1,y1]）。 */
  readonly pendingRect = signal<number[] | null>(null);
  /** mini 菜单位置（相对画布容器）；null = 关闭。 */
  readonly menuPos = signal<{ x: number; y: number } | null>(null);
  /** 制度索引目标选择窗口开关。 */
  readonly pickerOpen = signal(false);
  private dragging = false;

  /** 当前预览文件名（工具栏展示）。 */
  readonly fileName = computed(() => {
    const id = this.state.activeDocId();
    return this.state.docs().find((d) => d.id === id)?.name ?? "";
  });

  /** 当前页的制度索引标记（归一化 → CSS 换算后供模板渲染；rect 损坏行跳过）。 */
  readonly linkMarkers = computed(() => {
    const docId = this.state.activeDocId();
    const page = this.currentPage();
    const size = this.pageCss();
    if (docId === null || !size) return [];
    const out: {
      id: number;
      targetDocId: number;
      targetName: string;
      left: number;
      top: number;
      width: number;
      height: number;
    }[] = [];
    for (const l of this.state.docLinks()) {
      if (l.src_doc_id !== docId || l.page !== page) continue;
      try {
        const [x0, y0, x1, y1] = JSON.parse(l.rect) as number[];
        const target = this.state.docs().find((d) => d.id === l.target_doc_id);
        out.push({
          id: l.id,
          targetDocId: l.target_doc_id,
          targetName: target?.name ?? "目标文件（已删除）",
          left: (x0 / 1000) * size.w,
          top: (y0 / 1000) * size.h,
          width: ((x1 - x0) / 1000) * size.w,
          height: ((y1 - y0) / 1000) * size.h,
        });
      } catch {
        // 忽略损坏的 rect 行
      }
    }
    return out;
  });

  readonly canBack = computed(() => this.state.navIndex() > 0);
  readonly canForward = computed(
    () => this.state.navIndex() < this.state.navStack().length - 1,
  );

  /** 当前页的节点来源矩形（已建节点；蓝色标记，点击定位到流程图；rect 损坏行跳过）。 */
  readonly nodeMarkers = computed(() => {
    const docId = this.state.activeDocId();
    const page = this.currentPage();
    const size = this.pageCss();
    if (docId === null || !size) return [];
    const out: {
      id: number;
      code: string;
      selected: boolean;
      left: number;
      top: number;
      width: number;
      height: number;
    }[] = [];
    for (const n of this.state.nodes()) {
      if (n.doc_id !== docId || n.page !== page || !n.rect) continue;
      try {
        const [x0, y0, x1, y1] = JSON.parse(n.rect) as number[];
        out.push({
          id: n.id,
          code: n.code,
          selected: this.state.selectedNodeId() === n.id,
          left: (x0 / 1000) * size.w,
          top: (y0 / 1000) * size.h,
          width: ((x1 - x0) / 1000) * size.w,
          height: ((y1 - y0) / 1000) * size.h,
        });
      } catch {
        // 忽略损坏的 rect 行
      }
    }
    return out;
  });

  private renderSeq = 0;
  private resizeObserver: ResizeObserver | null = null;
  private resizeTimer: ReturnType<typeof setTimeout> | null = null;

  /** 驻留框选矩形（归一化 → CSS）：mouseup 后保留显示，取消/点他处才清除。 */
  readonly pendingCssRect = computed(() => {
    const rect = this.pendingRect();
    const size = this.pageCss();
    if (!rect || !size) return null;
    return {
      left: (rect[0] / 1000) * size.w,
      top: (rect[1] / 1000) * size.h,
      width: ((rect[2] - rect[0]) / 1000) * size.w,
      height: ((rect[3] - rect[1]) / 1000) * size.h,
    };
  });

  constructor() {
    // 联动入口：切换制度文件 → 加载并重渲
    effect(() => {
      const id = this.state.activeDocId();
      void this.onDocChanged(id);
    });
    // 导航栈后退/前进：同步页码（同文件时不触发上面的加载 effect）
    effect(() => {
      this.state.navSeq();
      const cur = this.currentNavEntry();
      if (cur && cur.docId === this.state.activeDocId()) {
        if (cur.page !== this.currentPage() && cur.page <= this.pageCount()) {
          this.currentPage.set(cur.page);
          this.requestRender();
        }
      }
    });
  }

  private currentNavEntry(): { docId: number; page: number } | null {
    const idx = this.state.navIndex();
    const stack = this.state.navStack();
    return idx >= 0 && idx < stack.length ? stack[idx] : null;
  }

  ngAfterViewInit(): void {
    const el = this.scrollRef?.nativeElement;
    if (el) {
      this.resizeObserver = new ResizeObserver(() => {
        if (this.resizeTimer) clearTimeout(this.resizeTimer);
        // 仅适宽模式下随容器宽度重算
        if (this.zoom() === null && this.pageCount() > 0) {
          this.resizeTimer = setTimeout(() => this.requestRender(), 150);
        }
      });
      this.resizeObserver.observe(el);
    }
  }

  ngOnDestroy(): void {
    this.resizeObserver?.disconnect();
    if (this.resizeTimer) clearTimeout(this.resizeTimer);
  }

  private async onDocChanged(docId: number | null): Promise<void> {
    const seq = ++this.renderSeq;
    this.loadError.set(null);
    this.dismissOverlays();
    if (docId === null) {
      this.pageCount.set(0);
      this.pageCss.set(null);
      return;
    }
    const doc = this.state.docs().find((d) => d.id === docId);
    if (!doc) return;
    this.loading.set(true);
    try {
      const project = this.state.project();
      if (!project) return;
      const bytes = await this.backend.icReadDocBytes(project.id, docId);
      const count = await this.pdf.loadBytes(bytes, `ic:${docId}`);
      if (seq !== this.renderSeq) return; // 已被更新的请求取代
      this.pageCount.set(count);
      // 导航栈目标页优先（后退/前进跨文件时回到栈内记录的页码）
      const nav = this.currentNavEntry();
      const target = nav && nav.docId === docId ? nav.page : 1;
      this.currentPage.set(Math.min(Math.max(1, target), count));
      this.zoom.set(null);
      // 先落 loading=false（canvas 常驻挂载，见模板），等一轮变更检测后再渲染，
      // 保证 canvas 已在 DOM 中。
      this.loading.set(false);
      await new Promise((r) => setTimeout(r, 0));
      if (seq !== this.renderSeq) return;
      this.requestRender();
    } catch (e) {
      if (seq === this.renderSeq) {
        this.loadError.set(errText(e));
      }
    } finally {
      if (seq === this.renderSeq) this.loading.set(false);
    }
  }

  /** 渲染当前页（缩放变化/翻页/适宽重算共用）。仅供渲染泵调用（串行，见 requestRender）。 */
  private async renderCurrentPage(): Promise<void> {
    const canvas = this.pageCanvas?.nativeElement;
    const container = this.scrollRef?.nativeElement;
    if (!canvas || !container || this.pageCount() === 0) return;
    const seq = ++this.renderSeq;
    const page = this.currentPage();
    try {
      let scale = this.zoom();
      if (scale === null) {
        // 适合宽度：容器宽（留 2rem 内边距）/ 页面 1.0 宽
        scale = await this.pdf.fitWidthScale(page, container.clientWidth - 32);
      }
      // intent "print"：微任务调度（不走 rAF），页面不可见时也能完成渲染——
      // 树点击自动渲染可能落在窗口未激活期（rAF 停火会卡死 display 渲染）
      const size = await this.pdf.renderPage(canvas, page, scale, "print");
      if (seq !== this.renderSeq) return;
      this.lastScale.set(scale);
      this.pageCss.set({ w: size.width, h: size.height });
    } catch (e) {
      if (!isRenderCancelled(e) && seq === this.renderSeq) {
        this.loadError.set(errText(e));
      }
    }
  }

  // ---- 渲染串行化（与 PdfViewerComponent 同一模式）----
  // pdf.js 对"取消中的渲染任务 + 同页新任务"并发组合会出现新任务永不 settle；
  // 所有渲染入口统一走队列：在渲期间的新请求仅标记 renderAgain，不并行打断。
  private rendering = false;
  private renderAgain = false;

  private requestRender(): void {
    this.renderAgain = true;
    if (!this.rendering) {
      void this.pumpRenders();
    }
  }

  private async pumpRenders(): Promise<void> {
    this.rendering = true;
    try {
      while (this.renderAgain) {
        this.renderAgain = false;
        await this.renderCurrentPage();
      }
    } finally {
      this.rendering = false;
    }
  }

  gotoPage(n: number): void {
    const target = Math.min(Math.max(1, n), this.pageCount());
    if (target === this.currentPage()) return;
    this.currentPage.set(target);
    this.state.syncNavPage(target);
    this.dismissOverlays();
    this.requestRender();
  }

  zoomIn(): void {
    this.zoom.set(Math.min(4, this.lastScale() * 1.2));
    this.requestRender();
  }

  zoomOut(): void {
    this.zoom.set(Math.max(0.2, this.lastScale() / 1.2));
    this.requestRender();
  }

  fitWidth(): void {
    this.zoom.set(null);
    this.requestRender();
  }

  // ---- 框选（0–1000 归一化；单页模式下天然以当前页为准） ----

  private eventPoint(e: MouseEvent): { x: number; y: number } | null {
    const wrap = this.canvasWrap?.nativeElement;
    if (!wrap) return null;
    const r = wrap.getBoundingClientRect();
    return {
      x: Math.min(Math.max(0, e.clientX - r.left), r.width),
      y: Math.min(Math.max(0, e.clientY - r.top), r.height),
    };
  }

  onOverlayMouseDown(e: MouseEvent): void {
    if (e.button !== 0) return;
    // 起点落到已有交互元素（索引标记）时不开始框选
    if ((e.target as HTMLElement).closest(".link-marker")) return;
    const pt = this.eventPoint(e);
    if (!pt) return;
    this.dismissOverlays();
    this.dragging = true;
    this.dragRect.set({ x0: pt.x, y0: pt.y, x1: pt.x, y1: pt.y });
    e.preventDefault();
  }

  onOverlayMouseMove(e: MouseEvent): void {
    if (!this.dragging) return;
    const pt = this.eventPoint(e);
    const rect = this.dragRect();
    if (!pt || !rect) return;
    this.dragRect.set({ ...rect, x1: pt.x, y1: pt.y });
  }

  onOverlayMouseUp(e: MouseEvent): void {
    if (!this.dragging) return;
    this.dragging = false;
    const rect = this.dragRect();
    this.dragRect.set(null);
    const size = this.pageCss();
    if (!rect || !size) return;
    // 规范化方向 + 过小将视为误触
    const x0 = Math.min(rect.x0, rect.x1);
    const x1 = Math.max(rect.x0, rect.x1);
    const y0 = Math.min(rect.y0, rect.y1);
    const y1 = Math.max(rect.y0, rect.y1);
    if (x1 - x0 < 6 || y1 - y0 < 6) return;
    // CSS → 0–1000 归一化
    const norm = [
      Math.round((x0 / size.w) * 1000),
      Math.round((y0 / size.h) * 1000),
      Math.round((x1 / size.w) * 1000),
      Math.round((y1 / size.h) * 1000),
    ];
    this.pendingRect.set(norm);
    // mini 菜单落在框选右下角，出界时内收
    const pt = this.eventPoint(e) ?? { x: x1, y: y1 };
    this.menuPos.set({
      x: Math.min(pt.x, Math.max(0, size.w - 92)),
      y: Math.min(pt.y, Math.max(0, size.h - 90)),
    });
  }

  /** 关闭 mini 菜单 / 选择器并清除驻留框选（取消或点击其他位置）。 */
  dismissOverlays(): void {
    this.menuPos.set(null);
    this.pickerOpen.set(false);
    this.pendingRect.set(null);
  }

  /** mini 菜单「添加节点」（级联选定类型/子类型）：直接触发 OCR 建节点。 */
  onMenuAddNode(e: { nodeType: string; subtype: string | null }): void {
    const rect = this.pendingRect();
    const docId = this.state.activeDocId();
    if (!rect || docId === null) return;
    this.state.pendingNodeSelection.set({
      docId,
      page: this.currentPage(),
      rect,
    });
    this.pendingRect.set(null);
    this.menuPos.set(null);
    void this.state.createNodeFromSelection(e.nodeType, e.subtype);
  }

  /** mini 菜单「制度索引」：打开目标选择窗口（驻留矩形保留到选定/取消）。 */
  onMenuAddLink(): void {
    this.menuPos.set(null);
    this.pickerOpen.set(true);
  }

  /** 选定索引目标：落库 ic_add_doc_link。 */
  async onPickLinkTarget(target: IcDocument): Promise<void> {
    const rect = this.pendingRect();
    const docId = this.state.activeDocId();
    if (!rect || docId === null) return;
    const ok = await this.state.addDocLink(
      docId,
      this.currentPage(),
      JSON.stringify(rect),
      target.id,
    );
    if (ok !== null) {
      this.dismissOverlays();
    }
  }

  /** 点击索引矩形：跳转目标 pdf（入栈导航历史）。 */
  onMarkerClick(targetDocId: number): void {
    this.state.openDoc(targetDocId, 1);
  }

  /** 点击节点来源矩形：流程图定位并选中对应节点。 */
  onNodeMarkerClick(nodeId: number): void {
    this.state.focusNode(nodeId);
  }



  navBack(): void {
    this.state.navBack();
  }

  navForward(): void {
    this.state.navForward();
  }
}
