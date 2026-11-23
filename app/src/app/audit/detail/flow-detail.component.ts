import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  signal,
} from "@angular/core";
import { FormsModule } from "@angular/forms";
import { IcNode } from "../../backend.models";
import { AuditStateService } from "../audit-state.service";

type SortKey = "code" | "type" | "name" | "source";

/**
 * 流程明细页签：编号 / 节点类型 / 节点名称 / 节点内容 / 来源制度（pdf 文件名）。
 * 支持搜索（编号/名称/内容/来源）、类型筛选、列头排序；随节点增删改实时刷新。
 * 点击行：选中节点；有来源制度的行同时定位跳转到该 pdf 页并高亮坐标矩形。
 */
@Component({
  selector: "app-flow-detail",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [FormsModule],
  template: `
    <div class="detail-wrap">
      <div class="detail-toolbar">
        <div class="clr-input-wrapper search-box">
          <input
            type="text"
            class="clr-input"
            placeholder="搜索编号/名称/内容/来源…"
            [ngModel]="query()"
            (ngModelChange)="query.set($event)"
            aria-label="搜索流程明细"
          />
        </div>
        <div class="clr-select-wrapper">
          <select
            class="clr-select"
            aria-label="按节点类型筛选"
            [ngModel]="typeFilter()"
            (ngModelChange)="typeFilter.set($event)"
          >
            <option value="">全部类型</option>
            <option value="ACT">控制活动</option>
            <option value="EVT">事件</option>
            <option value="ROL">角色</option>
            <option value="DAT">数据资料</option>
            <option value="LOG">逻辑</option>
          </select>
        </div>
        <span class="muted count">{{ rows().length }} 项</span>
      </div>

      @if (rows().length === 0) {
        <div class="placeholder muted">
          {{ query() || typeFilter() ? "无匹配节点" : "尚无节点：在流程图页签右键「增加节点」，或在 PDF 框选创建" }}
        </div>
      } @else {
        <table class="table">
          <thead>
            <tr>
              <th class="left">
                <button type="button" class="th-sort" (click)="toggleSort('code')">
                  编号{{ sortMark("code") }}
                </button>
              </th>
              <th class="left">
                <button type="button" class="th-sort" (click)="toggleSort('type')">
                  节点类型{{ sortMark("type") }}
                </button>
              </th>
              <th class="left">
                <button type="button" class="th-sort" (click)="toggleSort('name')">
                  节点名称{{ sortMark("name") }}
                </button>
              </th>
              <th class="left">节点内容</th>
              <th class="left">
                <button type="button" class="th-sort" (click)="toggleSort('source')">
                  来源制度{{ sortMark("source") }}
                </button>
              </th>
            </tr>
          </thead>
          <tbody>
            @for (r of rows(); track r.node.id) {
              <tr
                class="row-click"
                [class.row-selected]="state.selectedNodeId() === r.node.id"
                (click)="onRowClick(r.node)"
              >
                <td class="left mono">{{ r.node.code }}</td>
                <td class="left">{{ typeLabel(r.node) }}{{ r.node.subtype ? " / " + subtypeLabel(r.node) : "" }}</td>
                <td class="left">{{ r.node.name }}</td>
                <td class="left content-cell" [title]="r.node.content">{{ r.node.content }}</td>
                <td class="left">{{ r.sourceName }}</td>
              </tr>
            }
          </tbody>
        </table>
      }
    </div>
  `,
  styles: `
    .detail-wrap { height: 100%; overflow: auto; padding: 0.3rem; }
    .detail-toolbar {
      display: flex; align-items: center; gap: 0.5rem; margin-bottom: 0.35rem;
    }
    .search-box { width: 16rem; }
    .count { margin-left: auto; font-size: 0.6rem; }
    .placeholder { padding: 2rem 1rem; text-align: center; font-size: 0.65rem; }
    .mono { font-family: monospace; }
    .th-sort {
      border: 0; background: none; padding: 0; margin: 0; cursor: pointer;
      font: inherit; color: inherit; font-weight: 600;
    }
    .th-sort:hover { color: var(--cds-alias-typography-link-color, #0072a3); }
    .content-cell {
      max-width: 18rem; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
    }
    .row-click { cursor: pointer; }
    .row-click:hover { background: var(--cds-alias-object-container-background-shade, #f5f5f5); }
    .row-selected { background: #ebf2fc; }
    .muted { color: var(--cds-alias-typography-color-400, #888); }
  `,
})
export class FlowDetailComponent {
  protected readonly state = inject(AuditStateService);

  readonly query = signal("");
  readonly typeFilter = signal("");
  readonly sortKey = signal<SortKey>("code");
  readonly sortDir = signal<"asc" | "desc">("asc");

  readonly rows = computed(() => {
    const q = this.query().trim().toLowerCase();
    const tf = this.typeFilter();
    const docs = this.state.docs();
    let list = this.state.nodes().map((node) => ({
      node,
      sourceName: docs.find((d) => d.id === node.doc_id)?.name ?? "",
    }));
    if (tf) list = list.filter((r) => r.node.node_type === tf);
    if (q) {
      list = list.filter(
        (r) =>
          r.node.code.toLowerCase().includes(q) ||
          r.node.name.toLowerCase().includes(q) ||
          r.node.content.toLowerCase().includes(q) ||
          r.sourceName.toLowerCase().includes(q),
      );
    }
    const key = this.sortKey();
    const dir = this.sortDir() === "asc" ? 1 : -1;
    const keyOf = (r: { node: IcNode; sourceName: string }): string => {
      switch (key) {
        case "code":
          return r.node.code;
        case "type":
          return r.node.node_type + (r.node.subtype ?? "");
        case "name":
          return r.node.name;
        case "source":
          return r.sourceName;
      }
    };
    return [...list].sort((a, b) => keyOf(a).localeCompare(keyOf(b), "zh") * dir);
  });

  toggleSort(key: SortKey): void {
    if (this.sortKey() === key) {
      this.sortDir.update((d) => (d === "asc" ? "desc" : "asc"));
    } else {
      this.sortKey.set(key);
      this.sortDir.set("asc");
    }
  }

  sortMark(key: SortKey): string {
    if (this.sortKey() !== key) return "";
    return this.sortDir() === "asc" ? " ▲" : " ▼";
  }

  /** 点击行：选中节点；有来源制度 → 跳转 pdf 页并高亮坐标矩形；无来源 → 画布定位。 */
  onRowClick(node: IcNode): void {
    this.state.selectedNodeId.set(node.id);
    if (node.doc_id !== null && node.page !== null) {
      this.state.openDoc(node.doc_id, node.page);
    } else {
      this.state.focusNode(node.id);
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

  subtypeLabel(n: IcNode): string {
    const maps: Record<string, Record<string, string>> = {
      EVT: { time: "时间", message: "消息" },
      ROL: { person: "个人", dept: "部门", it: "IT" },
      DAT: { electronic: "电子", paper: "纸质", record: "记录" },
    };
    return (n.subtype && maps[n.node_type]?.[n.subtype]) || "";
  }
}
