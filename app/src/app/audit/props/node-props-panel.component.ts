import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  signal,
} from "@angular/core";
import { FormsModule } from "@angular/forms";
import { IcActProps, IcNode } from "../../backend.models";
import { AuditStateService } from "../audit-state.service";

const CONTROL_NATURES = ["人工", "自动化", "含人工的自动化"];
const PREVENTIVE_OR_DETECTIVE = ["预防", "检查"];
const FREQUENCIES = ["年", "季", "月", "周", "日", "多次", "不定期"];
const YES_NO = ["Y", "N"];

function parseProps(node: IcNode): IcActProps {
  try {
    const v = JSON.parse(node.props) as IcActProps;
    return typeof v === "object" && v !== null ? v : {};
  } catch {
    return {};
  }
}

/**
 * 节点属性编辑面板（选中节点后从流程图右侧侧滑出现）：
 * 类型/编号/来源只读（来源可点击跳转 PDF），名称与内容可编辑（失焦保存），
 * ACT 追加控制性质/预防检查/频率/关键控制/特别风险五个扩展属性（改动即存）。
 */
@Component({
  selector: "app-node-props-panel",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [FormsModule],
  templateUrl: "./node-props-panel.component.html",
  styleUrl: "./node-props-panel.component.css",
})
export class NodePropsPanelComponent {
  protected readonly state = inject(AuditStateService);

  readonly natures = CONTROL_NATURES;
  readonly preventiveDetective = PREVENTIVE_OR_DETECTIVE;
  readonly frequencies = FREQUENCIES;
  readonly yesNo = YES_NO;

  /** 当前选中节点（null = 面板关闭）。 */
  readonly node = computed(() => {
    const id = this.state.selectedNodeId();
    return this.state.nodes().find((n) => n.id === id) ?? null;
  });

  /** 名称/内容编辑草稿（选中节点切换时重置为节点现值）。 */
  readonly nameDraft = signal("");
  readonly contentDraft = signal("");

  /** ACT 扩展属性（解析自 props json）。 */
  readonly actProps = computed(() => {
    const n = this.node();
    return n ? parseProps(n) : {};
  });

  /** 来源制度文件名 + 页码（只读）。 */
  readonly sourceLabel = computed(() => {
    const n = this.node();
    if (!n || n.doc_id === null || n.page === null) return null;
    const doc = this.state.docs().find((d) => d.id === n.doc_id);
    return `${doc?.name ?? "未知文件"}（第 ${n.page} 页）`;
  });

  /** 相关角色/数据节点（按 role/data 边实时查询，方向不限）。 */
  readonly related = computed(() => {
    const n = this.node();
    if (!n) return { roles: [] as string[], data: [] as string[] };
    const collect = (kind: string) =>
      this.state
        .edges()
        .filter(
          (e) =>
            e.edge_kind === kind && (e.from_node === n.id || e.to_node === n.id),
        )
        .map((e) => {
          const otherId = e.from_node === n.id ? e.to_node : e.from_node;
          const t = this.state.nodes().find((x) => x.id === otherId);
          return t ? t.name || t.code : `#${otherId}`;
        });
    return { roles: collect("role"), data: collect("data") };
  });

  constructor() {
    // 选中节点变化 → 重置草稿
    effect(() => {
      const n = this.node();
      this.nameDraft.set(n?.name ?? "");
      this.contentDraft.set(n?.content ?? "");
    });
  }

  close(): void {
    this.state.propsPanelOpen.set(false);
  }

  /** 名称/内容失焦保存（值未变不发请求）。 */
  saveName(): void {
    const n = this.node();
    if (!n) return;
    const v = this.nameDraft().trim();
    if (v !== n.name) void this.state.updateNode(n.id, { name: v });
  }

  saveContent(): void {
    const n = this.node();
    if (!n) return;
    const v = this.contentDraft();
    if (v !== n.content) void this.state.updateNode(n.id, { content: v });
  }

  /** ACT 扩展属性：改动即存（merge 进 props json）。 */
  setProp(key: keyof IcActProps, value: string): void {
    const n = this.node();
    if (!n) return;
    const next = { ...parseProps(n), [key]: value };
    void this.state.updateNode(n.id, { props: JSON.stringify(next) });
  }

  /** 跳转来源 pdf（复用双向跳转通道）。 */
  gotoSource(): void {
    const n = this.node();
    if (n && n.doc_id !== null && n.page !== null) {
      this.state.openDoc(n.doc_id, n.page);
    }
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
}
