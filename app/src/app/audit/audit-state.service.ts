import { Injectable, inject, signal } from "@angular/core";
import {
  IcDocLink,
  IcDocument,
  IcEdge,
  IcGroup,
  IcNode,
  IcNodeInput,
  IcNodePatch,
  IcOrgRow,
  IcRosterRow,
  Project,
} from "../backend.models";
import { BackendService } from "../backend.service";
import { errText } from "../error-text";

/** 内控测试页签（流程图区域上方）。 */
export type AuditTab = "flow" | "detail" | "roster" | "org" | "relation";

/**
 * 内控测试模块的独立状态（原则 6：与 AppStateService 完全隔离，
 * 不向其中添加任何内控信号，避免影响 main-page 既有页面）。
 * 全部信号由本服务持有，组件 OnPush 直读。
 */
@Injectable({ providedIn: "root" })
export class AuditStateService {
  private readonly backend = inject(BackendService);

  // ---- 当前项目 ----
  readonly project = signal<Project | null>(null);
  /** 内控页内错误提示（页面级 notice；与 AppStateService.errorMessage 隔离）。 */
  readonly errorMessage = signal<string | null>(null);
  /** 操作成功反馈（导入完成等；success notice）。 */
  readonly infoMessage = signal<string | null>(null);

  // ---- 布局：制度文件树向上收起/向下展开（右上方） ----
  readonly treeCollapsed = signal(false);
  /** 左右分栏：流程图区域宽度百分比（拖拽调整，25–75）。 */
  readonly leftPct = signal(50);
  /** 右侧窗口（目录+PDF）整体收起为窄条。 */
  readonly rightCollapsed = signal(false);

  // ---- 流程图区域页签 ----
  readonly activeTab = signal<AuditTab>("flow");

  // ---- 制度文件 ----
  readonly docs = signal<IcDocument[]>([]);
  readonly docsLoading = signal(false);
  /** 右侧 PDF 预览当前打开的制度文件。 */
  readonly activeDocId = signal<number | null>(null);
  /** 框选「添加节点」的待处理来源（区域 OCR 建节点消费，见阶段 5）。 */
  readonly pendingNodeSelection = signal<{
    docId: number;
    page: number;
    rect: number[];
  } | null>(null);

  // ---- 流程图画布数据 ----
  readonly nodes = signal<IcNode[]>([]);
  readonly edges = signal<IcEdge[]>([]);
  readonly groups = signal<IcGroup[]>([]);
  /** 画布当前选中节点（PDF 矩形 ↔ 节点双向跳转用）。 */
  readonly selectedNodeId = signal<number | null>(null);
  /** 节点属性面板开关：仅右键「查看属性」打开（点击不展开）。 */
  readonly propsPanelOpen = signal(false);
  /** 区域 OCR 进行中（并发去重：上一次未完成时忽略新请求）。 */
  readonly ocrBusy = signal(false);
  /** 画布聚焦请求序号（PDF 侧点击节点矩形时 ++，画布居中对应节点）。 */
  readonly focusNodeSeq = signal(0);

  /** PDF 节点矩形点击：选中 + 切到流程图页签 + 请求画布聚焦。 */
  focusNode(nodeId: number): void {
    this.selectedNodeId.set(nodeId);
    this.activeTab.set("flow");
    this.focusNodeSeq.update((n) => n + 1);
  }

  // ---- 制度索引 ----
  readonly docLinks = signal<IcDocLink[]>([]);

  // ---- PDF 导航栈（文件+页；点索引矩形跳转入栈，后退/前进栈内移动） ----
  readonly navStack = signal<{ docId: number; page: number }[]>([]);
  readonly navIndex = signal(-1);
  /** 导航动作序号：每次入栈/后退/前进 ++，供预览组件同步页码。 */
  readonly navSeq = signal(0);

  // ---- 人员名册 / 组织结构（阶段 8 展示） ----
  readonly roster = signal<IcRosterRow[]>([]);
  readonly org = signal<IcOrgRow[]>([]);

  /** 进入内控页：装载项目与其制度文件/名册/组织。 */
  async load(project: Project): Promise<void> {
    this.project.set(project);
    await Promise.all([
      this.reloadDocs(),
      this.reloadRoster(),
      this.reloadOrg(),
      this.reloadDocLinks(),
      this.reloadNodes(),
      this.reloadEdges(),
      this.reloadGroups(),
    ]);
  }

  async reloadNodes(): Promise<void> {
    const p = this.project();
    if (!p) return;
    try {
      this.nodes.set(await this.backend.icListNodes(p.id));
    } catch (e) {
      this.errorMessage.set(`加载流程节点失败: ${errText(e)}`);
    }
  }

  /**
   * 框选「添加节点」全流程：区域 OCR（并发去重）→ 按类型建节点落到画布
   * 默认网格空位 → 选中。OCR 失败/无文字时置错误提示且不建节点。
   */
  async createNodeFromSelection(nodeType: string, subtype: string | null): Promise<void> {
    const p = this.project();
    const sel = this.pendingNodeSelection();
    if (!p || !sel || this.ocrBusy()) return;
    this.ocrBusy.set(true);
    try {
      const ocr = await this.backend.ocrRegion(p.id, sel.docId, sel.page, sel.rect);
      if (!ocr.text.trim()) {
        this.errorMessage.set("框选区域未识别到文字，请调整区域后重试");
        return;
      }
      const { x, y } = this.nextCanvasSlot();
      const node = await this.backend.icCreateNode(p.id, {
        node_type: nodeType,
        subtype,
        content: ocr.text,
        doc_id: sel.docId,
        page: sel.page,
        rect: JSON.stringify(sel.rect),
        canvas_x: x,
        canvas_y: y,
      } satisfies IcNodeInput);
      await this.reloadNodes();
      this.selectedNodeId.set(node.id);
      this.activeTab.set("flow");
      this.infoMessage.set(`节点 ${node.code} 已创建`);
    } catch (e) {
      this.errorMessage.set(`区域 OCR 失败: ${errText(e)}`);
    } finally {
      this.ocrBusy.set(false);
      this.pendingNodeSelection.set(null);
    }
  }

  /** 画布默认网格空位：按现有节点数左→右、上→下排布。 */
  private nextCanvasSlot(): { x: number; y: number } {
    const n = this.nodes().length;
    return { x: 40 + (n % 4) * 200, y: 40 + Math.floor(n / 4) * 120 };
  }

  /** 更新节点（名称/内容/坐标/属性等补丁），失败置 errorMessage。 */
  async updateNode(nodeId: number, patch: IcNodePatch): Promise<void> {
    const p = this.project();
    if (!p) return;
    try {
      const updated = await this.backend.icUpdateNode(p.id, nodeId, patch);
      this.nodes.update((list) => list.map((n) => (n.id === updated.id ? updated : n)));
    } catch (e) {
      this.errorMessage.set(`更新节点失败: ${errText(e)}`);
    }
  }

  /** 画布右键「增加节点」：无 OCR 来源，直接落于指定画布坐标。 */
  async createNodeAt(
    nodeType: string,
    subtype: string | null,
    x: number,
    y: number,
  ): Promise<void> {
    const p = this.project();
    if (!p) return;
    try {
      const node = await this.backend.icCreateNode(p.id, {
        node_type: nodeType,
        subtype,
        canvas_x: Math.round(x),
        canvas_y: Math.round(y),
      } satisfies IcNodeInput);
      await this.reloadNodes();
      this.selectedNodeId.set(node.id);
      this.infoMessage.set(`节点 ${node.code} 已创建`);
    } catch (e) {
      this.errorMessage.set(`创建节点失败: ${errText(e)}`);
    }
  }

  // ---- 连线与环境组 ----

  async reloadEdges(): Promise<void> {
    const p = this.project();
    if (!p) return;
    try {
      this.edges.set(await this.backend.icListEdges(p.id));
    } catch (e) {
      this.errorMessage.set(`加载连线失败: ${errText(e)}`);
    }
  }

  async reloadGroups(): Promise<void> {
    const p = this.project();
    if (!p) return;
    try {
      this.groups.set(await this.backend.icListGroups(p.id));
    } catch (e) {
      this.errorMessage.set(`加载环境组失败: ${errText(e)}`);
    }
  }

  /** 新增连线（类型校验在后端；失败原因提示用户）。返回是否成功。 */
  async addEdge(fromNode: number, toNode: number, edgeKind: string): Promise<boolean> {
    const p = this.project();
    if (!p) return false;
    try {
      await this.backend.icAddEdge(p.id, fromNode, toNode, edgeKind);
      await this.reloadEdges();
      return true;
    } catch (e) {
      this.errorMessage.set(`连线失败: ${errText(e)}`);
      return false;
    }
  }

  /** 用户拖边框改节点大小后持久化尺寸（0=内容自适应）。 */
  async persistNodeSize(nodeId: number, w: number, h: number): Promise<void> {
    const p = this.project();
    if (!p) return;
    try {
      const updated = await this.backend.icUpdateNode(p.id, nodeId, {
        canvas_w: Math.round(w),
        canvas_h: Math.round(h),
      });
      this.nodes.update((list) => list.map((n) => (n.id === updated.id ? updated : n)));
    } catch (e) {
      this.errorMessage.set(`保存节点大小失败: ${errText(e)}`);
    }
  }

  async removeEdge(edgeId: number): Promise<void> {
    const p = this.project();
    if (!p) return;
    try {
      await this.backend.icRemoveEdge(p.id, edgeId);
      await this.reloadEdges();
    } catch (e) {
      this.errorMessage.set(`删除连线失败: ${errText(e)}`);
    }
  }

  /** 创建环境组（rect 由画布按选中节点包围盒现算）。 */
  async createGroup(envType: string, rect: string, nodeIds: number[]): Promise<void> {
    const p = this.project();
    if (!p) return;
    try {
      await this.backend.icCreateGroup(p.id, envType, rect, nodeIds);
      await Promise.all([this.reloadGroups(), this.reloadNodes()]);
    } catch (e) {
      this.errorMessage.set(`创建环境组失败: ${errText(e)}`);
    }
  }

  /** 解散环境组（节点保留）。 */
  async deleteGroup(groupId: number): Promise<void> {
    const p = this.project();
    if (!p) return;
    try {
      await this.backend.icDeleteGroup(p.id, groupId);
      await Promise.all([this.reloadGroups(), this.reloadNodes()]);
    } catch (e) {
      this.errorMessage.set(`解散环境组失败: ${errText(e)}`);
    }
  }

  /** 节点拖动后回写环境组包围矩形。 */
  async updateGroupRect(groupId: number, rect: string): Promise<void> {
    const p = this.project();
    if (!p) return;
    try {
      await this.backend.icUpdateGroupRect(p.id, groupId, rect);
    } catch (e) {
      this.errorMessage.set(`更新环境组失败: ${errText(e)}`);
    }
  }

  /** 节点拖动结束：批量回写坐标（fMoveNodes 一次性给出全部移动节点）。 */
  async persistNodePositions(moves: { id: number; x: number; y: number }[]): Promise<void> {
    const p = this.project();
    if (!p || moves.length === 0) return;
    // 先乐观更新本地信号，再逐一落库
    const byId = new Map(moves.map((m) => [m.id, m]));
    this.nodes.update((list) =>
      list.map((n) => {
        const m = byId.get(n.id);
        return m ? { ...n, canvas_x: m.x, canvas_y: m.y } : n;
      }),
    );
    for (const m of moves) {
      try {
        await this.backend.icUpdateNode(p.id, m.id, { canvas_x: m.x, canvas_y: m.y });
      } catch (e) {
        this.errorMessage.set(`保存节点位置失败: ${errText(e)}`);
        await this.reloadNodes();
        return;
      }
    }
  }

  /** 删除节点（级联清边），失败置 errorMessage。 */
  async deleteNode(nodeId: number): Promise<void> {
    const p = this.project();
    if (!p) return;
    try {
      await this.backend.icDeleteNode(p.id, nodeId);
      if (this.selectedNodeId() === nodeId) this.selectedNodeId.set(null);
      await this.reloadNodes();
    } catch (e) {
      this.errorMessage.set(`删除节点失败: ${errText(e)}`);
    }
  }

  async reloadDocLinks(): Promise<void> {
    const p = this.project();
    if (!p) return;
    try {
      this.docLinks.set(await this.backend.icListDocLinks(p.id));
    } catch (e) {
      this.errorMessage.set(`加载制度索引失败: ${errText(e)}`);
    }
  }

  /** 新增制度索引并刷新（失败置 errorMessage 返回 null）。 */
  async addDocLink(
    srcDocId: number,
    page: number,
    rect: string,
    targetDocId: number,
  ): Promise<number | null> {
    const p = this.project();
    if (!p) return null;
    try {
      const id = await this.backend.icAddDocLink(p.id, srcDocId, page, rect, targetDocId);
      await this.reloadDocLinks();
      this.infoMessage.set("制度索引已建立");
      return id;
    } catch (e) {
      this.errorMessage.set(`建立制度索引失败: ${errText(e)}`);
      return null;
    }
  }

  async removeDocLink(linkId: number): Promise<void> {
    const p = this.project();
    if (!p) return;
    try {
      await this.backend.icRemoveDocLink(p.id, linkId);
      await this.reloadDocLinks();
    } catch (e) {
      this.errorMessage.set(`删除制度索引失败: ${errText(e)}`);
    }
  }

  // ---- PDF 导航栈 ----

  /** 打开制度文件到右侧 PDF 预览并入栈导航历史。目标已删除时提示且不动。 */
  openDoc(docId: number, page = 1): void {
    if (!this.docs().some((d) => d.id === docId)) {
      this.infoMessage.set("制度文件已删除，跳转/索引失效");
      return;
    }
    this.pushNav({ docId, page });
    this.activeDocId.set(docId);
  }

  /** 删除制度文件：流程图节点与索引保留，跳转失效；若正在预览则清空预览。 */
  async deleteDoc(docId: number): Promise<void> {
    const p = this.project();
    if (!p) return;
    try {
      await this.backend.icDeleteDoc(p.id, docId);
      if (this.activeDocId() === docId) {
        this.activeDocId.set(null);
      }
      // 导航栈中指向已删文件的条目清掉，避免后退到失效位置
      const stack = this.navStack().filter((e) => e.docId !== docId);
      const idx = this.navIndex();
      this.navStack.set(stack);
      this.navIndex.set(Math.min(idx, stack.length - 1));
      await this.reloadDocs();
      this.infoMessage.set("制度文件已删除（流程图保留，跳转链接已失效）");
    } catch (e) {
      this.errorMessage.set(`删除制度文件失败: ${errText(e)}`);
    }
  }

  private pushNav(entry: { docId: number; page: number }): void {
    const stack = this.navStack();
    const idx = this.navIndex();
    const cur = idx >= 0 ? stack[idx] : null;
    // 与栈顶相同（同文件同页）不重复入栈
    if (cur && cur.docId === entry.docId && cur.page === entry.page) return;
    const next = [...stack.slice(0, idx + 1), entry];
    this.navStack.set(next);
    this.navIndex.set(next.length - 1);
    this.navSeq.update((n) => n + 1);
  }

  /** 当前栈顶页码随翻页原位更新（不产生新历史条目）。 */
  syncNavPage(page: number): void {
    const idx = this.navIndex();
    const stack = this.navStack();
    if (idx < 0 || idx >= stack.length) return;
    if (stack[idx].page === page) return;
    const next = stack.slice();
    next[idx] = { ...next[idx], page };
    this.navStack.set(next);
  }

  navBack(): void {
    const idx = this.navIndex();
    if (idx <= 0) return;
    this.navIndex.set(idx - 1);
    const entry = this.navStack()[idx - 1];
    this.activeDocId.set(entry.docId);
    this.navSeq.update((n) => n + 1);
  }

  navForward(): void {
    const idx = this.navIndex();
    const stack = this.navStack();
    if (idx >= stack.length - 1) return;
    this.navIndex.set(idx + 1);
    const entry = stack[idx + 1];
    this.activeDocId.set(entry.docId);
    this.navSeq.update((n) => n + 1);
  }

  async reloadDocs(): Promise<void> {
    const p = this.project();
    if (!p) return;
    try {
      this.docs.set(await this.backend.icListDocs(p.id));
    } catch (e) {
      this.errorMessage.set(`加载制度文件失败: ${errText(e)}`);
    }
  }

  /** 导入制度文件夹；返回结果摘要（失败置 errorMessage 并返回 null）。 */
  async importDocs(
    srcDir: string,
  ): Promise<{ imported: number; updated: number } | null> {
    const p = this.project();
    if (!p) return null;
    this.docsLoading.set(true);
    try {
      const r = await this.backend.icImportDocs(p.id, srcDir);
      await this.reloadDocs();
      return r;
    } catch (e) {
      this.errorMessage.set(`导入制度文件失败: ${errText(e)}`);
      return null;
    } finally {
      this.docsLoading.set(false);
    }
  }

  /** 导入人员名册/组织结构；成功返回行数，失败置 errorMessage 返回 null。 */
  async importRoster(xlsxPath: string): Promise<number | null> {
    const p = this.project();
    if (!p) return null;
    try {
      const n = await this.backend.icImportRoster(p.id, xlsxPath);
      await this.reloadRoster();
      return n;
    } catch (e) {
      this.errorMessage.set(`导入人员名册失败: ${errText(e)}`);
      return null;
    }
  }

  async importOrg(xlsxPath: string): Promise<number | null> {
    const p = this.project();
    if (!p) return null;
    try {
      const n = await this.backend.icImportOrg(p.id, xlsxPath);
      await this.reloadOrg();
      return n;
    } catch (e) {
      this.errorMessage.set(`导入组织结构失败: ${errText(e)}`);
      return null;
    }
  }

  async reloadRoster(): Promise<void> {
    const p = this.project();
    if (!p) return;
    try {
      this.roster.set(await this.backend.icListRoster(p.id));
    } catch (e) {
      this.errorMessage.set(`加载人员名册失败: ${errText(e)}`);
    }
  }

  async reloadOrg(): Promise<void> {
    const p = this.project();
    if (!p) return;
    try {
      this.org.set(await this.backend.icListOrg(p.id));
    } catch (e) {
      this.errorMessage.set(`加载组织结构失败: ${errText(e)}`);
    }
  }

  /** 离开内控页面时清空，避免串项目。 */
  reset(): void {
    this.project.set(null);
    this.errorMessage.set(null);
    this.infoMessage.set(null);
    this.treeCollapsed.set(false);
    this.leftPct.set(50);
    this.rightCollapsed.set(false);
    this.activeTab.set("flow");
    this.docs.set([]);
    this.activeDocId.set(null);
    this.pendingNodeSelection.set(null);
    this.navStack.set([]);
    this.navIndex.set(-1);
    this.navSeq.set(0);
    this.nodes.set([]);
    this.edges.set([]);
    this.groups.set([]);
    this.selectedNodeId.set(null);
    this.propsPanelOpen.set(false);
    this.ocrBusy.set(false);
    this.docLinks.set([]);
    this.roster.set([]);
    this.org.set([]);
  }
}
