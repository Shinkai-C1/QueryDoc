import {
  ChangeDetectionStrategy,
  Component,
  HostListener,
  OnDestroy,
  ViewChild,
  computed,
  effect,
  inject,
  signal,
  untracked,
} from "@angular/core";
import {
  FCanvasComponent,
  FCreateConnectionEvent,
  FFlowComponent,
  FFlowModule,
  FMoveNodesEvent,
  FSelectionChangeEvent,
  FZoomDirective,
  F_DRAG_SELECT_CONTROL_SCHEME,
  provideFFlow,
  withControlScheme,
} from "@foblex/flow";
import { IcEdge, IcGroup, IcNode } from "../../backend.models";
import { AuditStateService } from "../audit-state.service";
import { NodePropsPanelComponent } from "../props/node-props-panel.component";
import { NODE_TYPE_CASCADE } from "./node-types";

/** 连接点方位（四方向）。 */
type Side = "t" | "r" | "b" | "l";
const SIDES: Side[] = ["t", "r", "b", "l"];

/** 连接点 id 编解码：`n<节点id>-<方位>`。 */
function connId(nodeId: number, side: Side): string {
  return `n${nodeId}-${side}`;
}

function parseConnId(id: string): { nodeId: number; side: Side } | null {
  const m = /^n(\d+)-(t|r|b|l)$/.exec(id);
  return m ? { nodeId: Number(m[1]), side: m[2] as Side } : null;
}

/** 画布右键菜单状态。 */
interface MenuState {
  /** 相对组件宿主的位置 */
  x: number;
  y: number;
  /** 对应的画布流坐标（创建节点落点用） */
  flowX: number;
  flowY: number;
  /** 右侧展开空间不足时级联向左展开 */
  openLeft: boolean;
}

const FALLBACK_NODE = { w: 176, h: 84 };
const FALLBACK_SIDE_NODE = { w: 110, h: 56 };
const GROUP_PAD = 24;

/**
 * 流程图画布（Foblex Flow，classic 模式：图记录由 AuditStateService 持有并落库）。
 * - 节点：左上编号+名称标题、中部内容自适应换行；EVT/LOG 用 SVG 描边保证边框完整；
 *   四角方向连接点（source-target），右下缩放手柄可改大小（落库持久化）；
 * - 连线带箭头；flow 自下而上（b→t），角色/数据自 ACT 右侧挂出（r→l）；
 *   ROL 连到 ACT 后自动排到其左侧，DAT 排到其右侧；
 * - 右键菜单：增加节点（级联选类型/子类型）/ 创建环境组（IT/人工，框选后可用）；
 *   点击其他位置/切换窗口/Esc 均关闭菜单。
 */
@Component({
  selector: "app-flow-canvas",
  changeDetection: ChangeDetectionStrategy.OnPush,
  providers: [provideFFlow(withControlScheme(F_DRAG_SELECT_CONTROL_SCHEME))],
  imports: [FFlowModule, NodePropsPanelComponent],
  templateUrl: "./flow-canvas.component.html",
  styleUrl: "./flow-canvas.component.css",
})
export class FlowCanvasComponent implements OnDestroy {
  protected readonly state = inject(AuditStateService);

  @ViewChild(FCanvasComponent) canvas?: FCanvasComponent;
  @ViewChild(FFlowComponent) flow?: FFlowComponent;
  @ViewChild(FZoomDirective) zoomCtl?: FZoomDirective;

  protected readonly sides = SIDES;
  protected readonly cascade = NODE_TYPE_CASCADE;

  // ---- 右键菜单 ----
  readonly canvasMenu = signal<MenuState | null>(null);
  readonly nodeMenu = signal<(MenuState & { node: IcNode }) | null>(null);
  readonly groupMenu = signal<(MenuState & { group: IcGroup }) | null>(null);
  readonly connMenu = signal<(MenuState & { edge: IcEdge }) | null>(null);

  // ---- 选择集（foblex 框选/点选） ----
  readonly selectedNodeIds = signal<number[]>([]);
  readonly selectedConnIds = signal<string[]>([]);

  /** 节点实测尺寸（fNodeSizeChange），供环境组包围盒与自动排布计算。 */
  private readonly nodeSizes = new Map<number, { w: number; h: number }>();
  private justDraggedUntil = 0;
  private dragGuardTimer: ReturnType<typeof setTimeout> | null = null;
  /** 缩放中的节点（mouseup 时落库最终尺寸）。 */
  private resizingNodeId: number | null = null;

  readonly hasSelection = computed(() => this.selectedNodeIds().length > 0);

  constructor() {
    // 仅 PDF 侧点击节点矩形（focusNodeSeq ++）时画布居中该节点；
    // 画布内点选/连线不触发居中（否则连线中途画布平移，连线不跟手）。
    effect(() => {
      this.state.focusNodeSeq();
      const id = untracked(() => this.state.selectedNodeId());
      if (id === null) return;
      queueMicrotask(() => this.canvas?.centerGroupOrNode(`n${id}`));
    });
  }

  ngOnDestroy(): void {
    if (this.dragGuardTimer) clearTimeout(this.dragGuardTimer);
  }

  // ---- 模板辅助 ----

  connId(n: IcNode, side: Side): string {
    return connId(n.id, side);
  }

  groupRect(g: IcGroup): number[] {
    try {
      return JSON.parse(g.rect) as number[];
    } catch {
      return [0, 0, 1, 1];
    }
  }

  /** 连线端点规范映射：flow 自下而上（b→t）；角色/数据自 ACT 右侧挂到对端左侧（r→l）。 */
  edgeSourceId(e: IcEdge): string {
    return connId(e.from_node, e.edge_kind === "flow" ? "b" : "r");
  }

  edgeTargetId(e: IcEdge): string {
    return connId(e.to_node, e.edge_kind === "flow" ? "t" : "l");
  }

  subtypeLabel(n: IcNode): string {
    const maps: Record<string, Record<string, string>> = {
      EVT: { time: "时间", message: "消息" },
      ROL: { person: "个人", dept: "部门", it: "IT" },
      DAT: { electronic: "电子", paper: "纸质", record: "记录" },
    };
    return (n.subtype && maps[n.node_type]?.[n.subtype]) || "";
  }

  typeLabel(n: IcNode): string {
    const map: Record<string, string> = {
      ACT: "控制活动",
      EVT: "事件",
      ROL: "角色",
      DAT: "数据资料",
      LOG: "逻辑",
    };
    return map[n.node_type] ?? n.node_type;
  }

  // ---- 事件：选择 / 拖动 / 连线 ----

  onSelectionChange(e: FSelectionChangeEvent): void {
    this.selectedNodeIds.set(
      e.nodeIds
        .map((id) => /^n(\d+)$/.exec(id)?.[1])
        .filter((x): x is string => !!x)
        .map(Number),
    );
    this.selectedConnIds.set(e.connectionIds);
    // 单选节点同步到全局选中态（PDF 双向跳转高亮）
    if (e.nodeIds.length === 1) {
      const m = /^n(\d+)$/.exec(e.nodeIds[0]);
      if (m) this.state.selectedNodeId.set(Number(m[1]));
    }
  }

  onNodeSize(nodeId: number, rect: { width: number; height: number }): void {
    this.nodeSizes.set(nodeId, { w: rect.width, h: rect.height });
  }

  onMoveNodes(e: FMoveNodesEvent): void {
    const moves = e.nodes
      .map((m) => {
        const match = /^n(\d+)$/.exec(m.id);
        return match ? { id: Number(match[1]), x: m.position.x, y: m.position.y } : null;
      })
      .filter((m): m is { id: number; x: number; y: number } => m !== null);
    if (moves.length > 0) {
      void this.state.persistNodePositions(moves).then(() => this.refreshGroupRects(moves));
    }
    // 拖动结束后的 click 不应触发跳转
    this.justDraggedUntil = Date.now() + 200;
    if (this.dragGuardTimer) clearTimeout(this.dragGuardTimer);
    this.dragGuardTimer = setTimeout(() => (this.justDraggedUntil = 0), 250);
  }

  /** 节点拖动后重算所属环境组包围盒并回写。 */
  private refreshGroupRects(moves: { id: number }[]): void {
    const moved = new Set(moves.map((m) => m.id));
    const nodes = this.state.nodes();
    const affected = new Set(
      nodes.filter((n) => moved.has(n.id) && n.group_id !== null).map((n) => n.group_id as number),
    );
    for (const gid of affected) {
      const members = nodes.filter((n) => n.group_id === gid);
      if (members.length === 0) continue;
      const rect = this.boundingRect(members);
      this.state.groups.update((list) =>
        list.map((g) => (g.id === gid ? { ...g, rect: JSON.stringify(rect) } : g)),
      );
      void this.state.updateGroupRect(gid, JSON.stringify(rect));
    }
  }

  private boundingRect(members: IcNode[]): number[] {
    let x0 = Infinity;
    let y0 = Infinity;
    let x1 = -Infinity;
    let y1 = -Infinity;
    for (const n of members) {
      const size = this.nodeSizes.get(n.id) ?? {
        w: n.canvas_w > 0 ? n.canvas_w : FALLBACK_NODE.w,
        h: n.canvas_h > 0 ? n.canvas_h : FALLBACK_NODE.h,
      };
      x0 = Math.min(x0, n.canvas_x);
      y0 = Math.min(y0, n.canvas_y);
      x1 = Math.max(x1, n.canvas_x + size.w);
      y1 = Math.max(y1, n.canvas_y + size.h);
    }
    return [x0 - GROUP_PAD, y0 - GROUP_PAD, x1 + GROUP_PAD, y1 + GROUP_PAD];
  }

  onCreateConnection(e: FCreateConnectionEvent): void {
    if (!e.targetId) return; // 拖到空白处：忽略
    const src = parseConnId(e.sourceId);
    const dst = parseConnId(e.targetId);
    if (!src || !dst) return;
    const nodes = this.state.nodes();
    const from = nodes.find((n) => n.id === src.nodeId);
    const to = nodes.find((n) => n.id === dst.nodeId);
    if (!from || !to) return;
    // 连线类型推导（后端兜底同规则）：任意节点均可 flow 连接；
    // {ACT, ROL} 任意方向为 role（角色关联）；{ACT, DAT} 任意方向为 data（数据关联）。
    const pair = new Set([from.node_type, to.node_type]);
    let kind: string;
    if (pair.has("ACT") && pair.has("ROL")) {
      kind = "role";
    } else if (pair.has("ACT") && pair.has("DAT")) {
      kind = "data";
    } else {
      kind = "flow";
    }
    void this.state.addEdge(from.id, to.id, kind).then((ok) => {
      if (ok && (kind === "role" || kind === "data")) {
        // 自动排布的是非 ACT 一端（角色/数据节点）到 ACT 的左/右侧
        const act = from.node_type === "ACT" ? from : to;
        const sideNode = from.node_type === "ACT" ? to : from;
        void this.autoArrange(act, sideNode, kind);
      }
    });
  }

  /** 角色连到 ACT → 自动排到其左侧；数据连到 ACT → 自动排到其右侧（多个纵向排开）。 */
  private async autoArrange(from: IcNode, to: IcNode, kind: "role" | "data"): Promise<void> {
    const fromSize = this.nodeSizes.get(from.id) ?? {
      w: from.canvas_w > 0 ? from.canvas_w : FALLBACK_NODE.w,
      h: from.canvas_h > 0 ? from.canvas_h : FALLBACK_NODE.h,
    };
    const toSize = this.nodeSizes.get(to.id) ?? {
      w: to.canvas_w > 0 ? to.canvas_w : FALLBACK_SIDE_NODE.w,
      h: to.canvas_h > 0 ? to.canvas_h : FALLBACK_SIDE_NODE.h,
    };
    const sideCount = this.state
      .edges()
      .filter((e) => e.from_node === from.id && e.edge_kind === kind).length;
    const idx = Math.max(0, sideCount - 1);
    const gap = 48;
    const vgap = 16;
    const x =
      kind === "role"
        ? from.canvas_x - toSize.w - gap
        : from.canvas_x + fromSize.w + gap;
    const y = from.canvas_y + fromSize.h / 2 - toSize.h / 2 + idx * (toSize.h + vgap);
    await this.state.updateNode(to.id, {
      canvas_x: Math.round(x),
      canvas_y: Math.round(y),
    });
    this.refreshGroupRects([{ id: to.id }]);
  }

  // ---- 节点点击（选中 + 跳转 PDF 来源；连接点与拖动作业不触发） ----

  onNodeClick(n: IcNode, e: MouseEvent): void {
    if (Date.now() < this.justDraggedUntil) return;
    // 点在连接点/缩放手柄上不选中跳转
    if ((e.target as HTMLElement).closest(".conn, .resize-handle")) return;
    this.state.selectedNodeId.set(n.id);
    if (n.doc_id !== null && n.page !== null) {
      this.state.openDoc(n.doc_id, n.page);
    }
  }

  // ---- 缩放（拖右下手柄；mouseup 落库最终尺寸） ----

  onResizeStart(nodeId: number): void {
    this.resizingNodeId = nodeId;
  }

  @HostListener("document:mouseup")
  onDocMouseUp(): void {
    if (this.resizingNodeId === null) return;
    const id = this.resizingNodeId;
    this.resizingNodeId = null;
    const size = this.nodeSizes.get(id);
    if (!size) return;
    const n = this.state.nodes().find((x) => x.id === id);
    if (!n) return;
    // 与库存值一致则不必写库
    if (Math.round(size.w) === Math.round(n.canvas_w) && Math.round(size.h) === Math.round(n.canvas_h)) {
      return;
    }
    void this.state.persistNodeSize(id, size.w, size.h);
  }

  // ---- 右键菜单（点击他处/切窗/Esc 关闭） ----

  @HostListener("document:mousedown", ["$event"])
  onDocMouseDown(e: MouseEvent): void {
    if (!this.canvasMenu() && !this.nodeMenu() && !this.groupMenu() && !this.connMenu())
      return;
    // 仅真实元素目标参与判定（document/window 级事件无 closest）
    if (!(e.target instanceof Element)) return;
    if (!e.target.closest(".ctx-menu")) {
      this.closeMenus();
    }
  }

  @HostListener("window:blur")
  onWindowBlur(): void {
    this.closeMenus();
  }

  private menuState(e: MouseEvent, host: HTMLElement): MenuState {
    const r = host.getBoundingClientRect();
    const flowRect = this.flow?.getPositionInFlow({ x: e.clientX, y: e.clientY });
    const x = e.clientX - r.left;
    return {
      x,
      y: e.clientY - r.top,
      flowX: flowRect?.x ?? 0,
      flowY: flowRect?.y ?? 0,
      // 两级级联约需 20rem，右侧不足则向左展开
      openLeft: host.clientWidth - x < 320,
    };
  }

  onCanvasContextMenu(e: MouseEvent, host: HTMLElement): void {
    e.preventDefault();
    this.nodeMenu.set(null);
    this.groupMenu.set(null);
    this.connMenu.set(null);
    const m = this.menuState(e, host);
    // 菜单出界内收（主菜单宽约 10rem、高约 8rem），保证右下角点击也完整可见
    const hw = host.clientWidth;
    const hh = host.clientHeight;
    this.canvasMenu.set({
      ...m,
      x: Math.min(m.x, Math.max(0, hw - 160)),
      y: Math.min(m.y, Math.max(0, hh - 130)),
    });
  }

  onNodeContextMenu(n: IcNode, e: MouseEvent, host: HTMLElement): void {
    e.preventDefault();
    e.stopPropagation();
    this.canvasMenu.set(null);
    this.groupMenu.set(null);
    this.connMenu.set(null);
    this.nodeMenu.set({ ...this.menuState(e, host), node: n });
  }

  onGroupContextMenu(g: IcGroup, e: MouseEvent, host: HTMLElement): void {
    e.preventDefault();
    e.stopPropagation();
    this.canvasMenu.set(null);
    this.nodeMenu.set(null);
    this.connMenu.set(null);
    this.groupMenu.set({ ...this.menuState(e, host), group: g });
  }

  closeMenus(): void {
    this.canvasMenu.set(null);
    this.nodeMenu.set(null);
    this.groupMenu.set(null);
    this.connMenu.set(null);
  }

  /** 连线右键：删除该连线。 */
  onConnectionContextMenu(e: IcEdge, ev: MouseEvent, host: HTMLElement): void {
    ev.preventDefault();
    ev.stopPropagation();
    this.closeMenus();
    this.connMenu.set({ ...this.menuState(ev, host), edge: e });
  }

  deleteConnection(edge: IcEdge): void {
    this.closeMenus();
    void this.state.removeEdge(edge.id);
  }

  /** 节点右键「查看属性」：选中并打开属性面板（仅此入口展开面板）。 */
  viewNodeProps(n: IcNode): void {
    this.closeMenus();
    this.state.selectedNodeId.set(n.id);
    this.state.propsPanelOpen.set(true);
  }

  /** 画布缩放控件：放大 / 缩小 / 重置适应全部节点。 */
  zoomIn(): void {
    this.zoomCtl?.zoomIn();
  }

  zoomOut(): void {
    this.zoomCtl?.zoomOut();
  }

  fitAll(): void {
    this.canvas?.fitToScreen(undefined, true, false);
  }

  /** 级联菜单选类型（含子类型）→ 直接建于右键处。 */
  addNodeOfType(nodeType: string, subtype: string | null): void {
    const menu = this.canvasMenu();
    this.closeMenus();
    if (!menu) return;
    void this.state.createNodeAt(nodeType, subtype, menu.flowX, menu.flowY);
  }

  /** 画布菜单「创建环境组」：把当前选中节点归入新组。 */
  createGroup(envType: string): void {
    const ids = this.selectedNodeIds();
    this.closeMenus();
    if (ids.length === 0) return;
    const members = this.state.nodes().filter((n) => ids.includes(n.id));
    if (members.length === 0) return;
    const rect = this.boundingRect(members);
    void this.state.createGroup(envType, JSON.stringify(rect), ids);
  }

  deleteNode(n: IcNode): void {
    this.closeMenus();
    void this.state.deleteNode(n.id);
  }

  dissolveGroup(g: IcGroup): void {
    this.closeMenus();
    void this.state.deleteGroup(g.id);
  }

  /** Delete/Backspace：删除选中连线与节点（节点级联清边，编号不复用）。
   *  属性面板等输入框内的删除键不触发（事件冒泡防护）。 */
  onDeleteKey(e: Event): void {
    const target = e.target as HTMLElement;
    if (target.closest("input, textarea, select, [contenteditable]")) return;
    const connIds = this.selectedConnIds();
    const nodeIds = this.selectedNodeIds();
    if (connIds.length === 0 && nodeIds.length === 0) return;
    for (const cid of connIds) {
      const m = /^e(\d+)$/.exec(cid);
      if (m) void this.state.removeEdge(Number(m[1]));
    }
    for (const nid of nodeIds) {
      void this.state.deleteNode(nid);
    }
  }
}
