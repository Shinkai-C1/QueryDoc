import {
  CUSTOM_ELEMENTS_SCHEMA,
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  signal,
} from "@angular/core";
import { ClarityIcons } from "@cds/core/icon";
import { angleIcon } from "@cds/core/icon/shapes/angle.js";
import { folderIcon } from "@cds/core/icon/shapes/folder.js";
import { fileIcon } from "@cds/core/icon/shapes/file.js";
import "@cds/core/icon/register.js";
import { open, save } from "@tauri-apps/plugin-dialog";
import { IcDocument } from "../../backend.models";
import { errText } from "../../error-text";
import { BackendService } from "../../backend.service";
import { ModalComponent } from "../../modal.component";
import { AuditStateService } from "../audit-state.service";
import { ImportDocsModalComponent } from "./import-docs-modal.component";

ClarityIcons.addIcons(angleIcon, folderIcon, fileIcon);

/** 制度文件树节点：目录节点有 children，文件节点带 doc。 */
export interface DocTreeNode {
  name: string;
  children: DocTreeNode[];
  doc: IcDocument | null;
}

/** 按 rel_path（`/` 分隔）把扁平文件列表组装成目录树（目录在前、文件在后，各自按名称排序）。 */
export function buildDocTree(docs: IcDocument[]): DocTreeNode[] {
  const root: DocTreeNode = { name: "", children: [], doc: null };
  for (const doc of docs) {
    const segs = doc.rel_path.split("/");
    let cur = root;
    for (let i = 0; i < segs.length - 1; i++) {
      const seg = segs[i];
      let next = cur.children.find((c) => c.doc === null && c.name === seg);
      if (!next) {
        next = { name: seg, children: [], doc: null };
        cur.children.push(next);
      }
      cur = next;
    }
    cur.children.push({ name: segs[segs.length - 1], children: [], doc });
  }
  const sortRec = (n: DocTreeNode) => {
    n.children.sort((a, b) => {
      if ((a.doc === null) !== (b.doc === null)) return a.doc === null ? -1 : 1;
      return a.name.localeCompare(b.name, "zh");
    });
    n.children.forEach(sortRec);
  };
  sortRec(root);
  return root.children;
}

/** 可见行（展开状态拍平后的文件管理器行）。 */
interface TreeRow {
  key: string;
  depth: number;
  name: string;
  folder: boolean;
  /** 目录节点的路径 key（收起状态判定）。 */
  path: string;
  doc: IcDocument | null;
}

/**
 * 制度文件目录（文件管理器风格）：
 * - 目录/文件图标 + 缩进层级，目录点击展开/收起；
 * - 行间隔行分色（zebra），当前预览文件高亮；
 * - 头部：导入制度文件 + 名册/组织两组 Clarity 按钮组（同一行）。
 */
@Component({
  selector: "app-doc-tree",
  changeDetection: ChangeDetectionStrategy.OnPush,
  schemas: [CUSTOM_ELEMENTS_SCHEMA],
  imports: [ImportDocsModalComponent, ModalComponent],
  templateUrl: "./doc-tree.component.html",
  styleUrl: "./doc-tree.component.css",
})
export class DocTreeComponent {
  protected readonly state = inject(AuditStateService);
  private readonly backend = inject(BackendService);

  readonly importOpen = signal(false);
  readonly deleteTarget = signal<IcDocument | null>(null);
  /** 收起中的目录路径集合（默认全部展开）。 */
  readonly collapsed = signal<ReadonlySet<string>>(new Set());

  /** 展开状态拍平的可见行（zebra 按行号着色）。 */
  readonly rows = computed<TreeRow[]>(() => {
    const out: TreeRow[] = [];
    const collapsed = this.collapsed();
    const walk = (nodes: DocTreeNode[], depth: number, prefix: string) => {
      for (const n of nodes) {
        const path = prefix ? `${prefix}/${n.name}` : n.name;
        const folder = n.doc === null;
        out.push({ key: path, depth, name: n.name, folder, path, doc: n.doc });
        if (folder && !collapsed.has(path)) {
          walk(n.children, depth + 1, path);
        }
      }
    };
    walk(buildDocTree(this.state.docs()), 0, "");
    return out;
  });

  toggleFolder(path: string): void {
    this.collapsed.update((set) => {
      const next = new Set(set);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  }

  isCollapsed(path: string): boolean {
    return this.collapsed().has(path);
  }

  async confirmDelete(): Promise<void> {
    const target = this.deleteTarget();
    if (!target) return;
    await this.state.deleteDoc(target.id);
    this.deleteTarget.set(null);
  }

  /** 下载名册/组织模版 xlsx：保存对话框选路径 → 后端 rust_xlsxwriter 生成。 */
  async downloadTemplate(kind: "roster" | "org"): Promise<void> {
    const label = kind === "roster" ? "人员名册" : "组织结构";
    try {
      const path = await save({
        defaultPath: `${label}模版.xlsx`,
        filters: [{ name: "Excel", extensions: ["xlsx"] }],
      });
      if (typeof path !== "string") return;
      await this.backend.icWriteTemplate(kind, path);
      this.state.infoMessage.set(`${label}模版已保存：${path}`);
    } catch (e) {
      this.state.errorMessage.set(`${label}模版生成失败: ${errText(e)}`);
    }
  }

  /** 导入名册/组织 xlsx：选文件 → 导入 → 成功/失败提示。 */
  async importXlsx(kind: "roster" | "org"): Promise<void> {
    const label = kind === "roster" ? "人员名册" : "组织结构";
    try {
      const picked = await open({
        directory: false,
        multiple: false,
        filters: [{ name: "Excel", extensions: ["xlsx"] }],
      });
      if (typeof picked !== "string") return;
      const n =
        kind === "roster"
          ? await this.state.importRoster(picked)
          : await this.state.importOrg(picked);
      if (n !== null) {
        this.state.infoMessage.set(`${label}导入成功：${n} 行`);
      }
    } catch (e) {
      this.state.errorMessage.set(`${label}导入失败: ${errText(e)}`);
    }
  }
}
