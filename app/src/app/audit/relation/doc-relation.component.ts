import {
  AfterViewInit,
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  OnDestroy,
  ViewChild,
  effect,
  inject,
} from "@angular/core";
import { DirectedGraph } from "graphology";
import Sigma from "sigma";
import { AuditStateService } from "../audit-state.service";

/**
 * 制度关系页签（Obsidian 风格关系图谱，Sigma.js + graphology）：
 * - 节点=制度文件，有向边=已建索引；关联越多节点越大；
 * - 悬停节点：高亮其关联节点与连线，其余淡出；
 * - 点击节点：右侧预览该制度文件；
 * - 按住节点可临时拖动（牵引），松开后回到聚集分布；
 * - 图容器常驻挂载（数据到达即增量刷新，退出重进也能正常显示）。
 */
@Component({
  selector: "app-doc-relation",
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <div class="relation-wrap">
      <div class="graph-container" #graphRef></div>
      @if (state.docs().length === 0) {
        <div class="placeholder muted">
          尚无制度文件：先在右侧「导入制度文件」，框选后建立索引即出现关系
        </div>
      }
    </div>
  `,
  styles: `
    .relation-wrap { height: 100%; position: relative; overflow: hidden; padding: 0.3rem; }
    .graph-container {
      height: 100%; width: 100%;
      border: 1px solid var(--cds-alias-object-border-color, #d9d9d9);
      border-radius: 4px;
    }
    .placeholder {
      position: absolute; inset: 0; display: flex; align-items: center; justify-content: center;
      font-size: 0.65rem; text-align: center; padding: 1rem; pointer-events: none;
    }
    .muted { color: var(--cds-alias-typography-color-400, #888); }
  `,
})
export class DocRelationComponent implements AfterViewInit, OnDestroy {
  protected readonly state = inject(AuditStateService);

  @ViewChild("graphRef") graphRef?: ElementRef<HTMLDivElement>;

  private graph: DirectedGraph | null = null;
  private renderer: Sigma | null = null;
  private resizeObserver: ResizeObserver | null = null;
  private hoveredNode: string | null = null;
  private draggedNode: string | null = null;
  private dragOrigin: { x: number; y: number } | null = null;

  constructor() {
    // 文档/索引变化 → 重建图数据并刷新（容器常驻，重进任务也能恢复）
    effect(() => {
      this.state.docs();
      this.state.docLinks();
      this.rebuild();
    });
  }

  ngAfterViewInit(): void {
    const el = this.graphRef?.nativeElement;
    if (!el) return;
    this.graph = new DirectedGraph();
    this.renderer = new Sigma(this.graph, el, {
      defaultEdgeType: "arrow",
      renderEdgeLabels: false,
      labelSize: 12,
      // 悬停高亮：关联节点橙色、关联边蓝色加粗，其余淡出
      nodeReducer: (node, data) => {
        if (!this.hoveredNode || !this.graph) return data;
        if (node === this.hoveredNode) return { ...data, color: "#ff9100", zIndex: 2 };
        if (
          this.graph.hasEdge(this.hoveredNode, node) ||
          this.graph.hasEdge(node, this.hoveredNode)
        ) {
          return { ...data, color: "#0064dd", zIndex: 1 };
        }
        return { ...data, color: "#d9d9d9", label: "" };
      },
      edgeReducer: (edge, data) => {
        if (!this.hoveredNode || !this.graph) return data;
        const ext = this.graph.extremities(edge);
        if (ext.includes(this.hoveredNode)) {
          return { ...data, color: "#0064dd", size: 3 };
        }
        return { ...data, color: "#e6e6e6" };
      },
    });
    this.bindInteractions();
    this.resizeObserver = new ResizeObserver(() => this.renderer?.refresh());
    this.resizeObserver.observe(el);
    this.rebuild();
  }

  ngOnDestroy(): void {
    this.resizeObserver?.disconnect();
    this.renderer?.kill();
    this.renderer = null;
    this.graph = null;
  }

  /** 悬停高亮 / 点击预览 / 按住拖动（牵引回弹）。 */
  private bindInteractions(): void {
    const sigma = this.renderer;
    if (!sigma || !this.graph) return;

    sigma.on("enterNode", (e: { node: string }) => {
      this.hoveredNode = e.node;
      sigma.refresh();
    });
    sigma.on("leaveNode", () => {
      this.hoveredNode = null;
      sigma.refresh();
    });
    // 点击节点：右侧预览制度文件
    sigma.on("clickNode", (e: { node: string }) => {
      const docId = Number(e.node);
      if (Number.isFinite(docId)) {
        this.state.openDoc(docId, 1);
      }
    });
    // 按住拖动（牵引），松开回弹到聚集分布
    sigma.on("downNode", (e: { node: string }) => {
      if (!this.graph) return;
      this.draggedNode = e.node;
      const a = this.graph.getNodeAttributes(e.node);
      this.dragOrigin = { x: a["x"] as number, y: a["y"] as number };
    });
    sigma.getMouseCaptor().on("mousemovebody", (e: { x: number; y: number }) => {
      if (!this.draggedNode || !this.graph) return;
      const pos = sigma.viewportToGraph({ x: e.x, y: e.y });
      this.graph.setNodeAttribute(this.draggedNode, "x", pos.x);
      this.graph.setNodeAttribute(this.draggedNode, "y", pos.y);
      sigma.refresh();
    });
    sigma.getMouseCaptor().on("mouseup", () => {
      if (this.draggedNode && this.dragOrigin && this.graph) {
        // 回弹：恢复原聚集位置
        this.graph.setNodeAttribute(this.draggedNode, "x", this.dragOrigin.x);
        this.graph.setNodeAttribute(this.draggedNode, "y", this.dragOrigin.y);
        sigma.refresh();
      }
      this.draggedNode = null;
      this.dragOrigin = null;
    });
  }

  /** 重建图：节点圆周布局，边=制度索引（src → target）；度数越大节点越大。 */
  private rebuild(): void {
    if (!this.graph) return;
    const g = this.graph;
    g.clear();
    const docs = this.state.docs();
    const links = this.state.docLinks();
    const n = docs.length;
    const radius = Math.max(80, n * 26);
    docs.forEach((doc, i) => {
      const angle = (2 * Math.PI * i) / Math.max(1, n);
      g.addNode(String(doc.id), {
        label: doc.name,
        x: radius * Math.cos(angle),
        y: radius * Math.sin(angle),
        size: 8,
        color: "#0064dd",
      });
    });
    for (const l of links) {
      const src = String(l.src_doc_id);
      const dst = String(l.target_doc_id);
      if (g.hasNode(src) && g.hasNode(dst)) {
        g.addEdgeWithKey(`e${l.id}`, src, dst, { size: 2, color: "#7a6700" });
      }
    }
    // Obsidian 风格：关联越多节点越大
    g.forEachNode((node) => {
      g.setNodeAttribute(node, "size", Math.min(24, 6 + g.degree(node) * 3));
    });
    this.renderer?.refresh();
  }
}
