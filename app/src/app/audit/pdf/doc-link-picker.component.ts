import {
  CUSTOM_ELEMENTS_SCHEMA,
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  output,
  signal,
} from "@angular/core";
import { FormsModule } from "@angular/forms";
import { ClarityIcons } from "@cds/core/icon";
import { fileIcon } from "@cds/core/icon/shapes/file.js";
import "@cds/core/icon/register.js";
import { IcDocument } from "../../backend.models";
import { AuditStateService } from "../audit-state.service";

ClarityIcons.addIcons(fileIcon);

/** 文件所在目录（rel_path 去掉文件名本身；根目录文件返回「根目录」）。 */
function dirOf(relPath: string): string {
  const idx = relPath.lastIndexOf("/");
  return idx < 0 ? "根目录" : relPath.slice(0, idx);
}

/**
 * 制度索引目标选择 mini 窗口（文件管理器风格）：
 * 顶部文件名搜索框；列表每行 = 文件名（主显示）+ 其下灰色小字所在目录
 * （不含文件名本身），行间隔行分色。选定后 emit pick。
 */
@Component({
  selector: "app-doc-link-picker",
  changeDetection: ChangeDetectionStrategy.OnPush,
  schemas: [CUSTOM_ELEMENTS_SCHEMA],
  imports: [FormsModule],
  template: `
    <div class="picker" role="dialog" aria-label="选择索引目标制度文件">
      <div class="picker-header">
        <span class="picker-title">选择索引目标</span>
        <button
          type="button"
          class="btn btn-sm btn-link"
          aria-label="关闭"
          (click)="cancelled.emit()"
        >
          ✕
        </button>
      </div>
      <div class="picker-search">
        <div class="clr-input-wrapper">
          <input
            type="text"
            class="clr-input"
            placeholder="按文件名搜索…"
            [ngModel]="query()"
            (ngModelChange)="query.set($event)"
            aria-label="按文件名搜索制度文件"
          />
        </div>
      </div>
      <div class="picker-body">
        @if (filtered().length === 0) {
          <p class="muted">{{ query().trim() ? "无匹配文件" : "尚未导入制度文件" }}</p>
        }
        @for (doc of filtered(); track doc.id; let i = $index) {
          <button
            type="button"
            class="file-row"
            [class.odd]="i % 2 === 1"
            [title]="doc.rel_path"
            (click)="pick.emit(doc)"
          >
            <cds-icon shape="file" class="ic-file"></cds-icon>
            <span class="file-text">
              <span class="file-name">{{ doc.name }}</span>
              <span class="file-dir">{{ dirLabel(doc.rel_path) }}</span>
            </span>
          </button>
        }
      </div>
    </div>
  `,
  styles: `
    :host { display: contents; }
    .picker {
      position: absolute;
      top: 2rem;
      right: 0.5rem;
      z-index: 40;
      width: 14rem;
      max-height: 70%;
      display: flex;
      flex-direction: column;
      background: var(--cds-alias-object-container-background, #fff);
      border: 1px solid var(--cds-alias-object-border-color, #d9d9d9);
      border-radius: 4px;
      box-shadow: 0 2px 10px rgba(0, 0, 0, 0.2);
    }
    .picker-header {
      display: flex; align-items: center; justify-content: space-between;
      padding: 0.25rem 0.5rem;
      border-bottom: 1px solid var(--cds-alias-object-border-color, #eee);
    }
    .picker-title { font-size: 0.65rem; font-weight: 700; }
    .picker-header .btn { margin: 0; min-width: 0; padding: 0 0.2rem; }
    .picker-search { padding: 0.3rem 0.5rem; }
    .picker-body {
      flex: 1; overflow: auto;
      border-top: 1px solid var(--cds-alias-object-border-color, #f0f0f0);
    }
    .file-row {
      display: flex; align-items: center; gap: 0.35rem; width: 100%;
      border: 0; background: #fff; padding: 0.22rem 0.45rem; margin: 0;
      cursor: pointer; text-align: left;
      border-bottom: 1px solid var(--cds-alias-object-border-color, #f0f0f0);
    }
    .file-row.odd { background: #f2f2f2; }
    .file-row:hover { background: #ebf2fc; }
    .ic-file { width: 0.7rem; height: 0.7rem; flex: none; color: #0053b8; }
    .file-text { flex: 1; min-width: 0; display: flex; flex-direction: column; }
    .file-name {
      font-size: 0.62rem; color: var(--cds-alias-typography-color-500, #333);
      overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
    }
    .file-dir {
      font-size: 0.52rem; color: var(--cds-alias-typography-color-400, #999);
      overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
    }
    .muted { color: var(--cds-alias-typography-color-400, #888); font-size: 0.6rem; padding: 0.4rem; }
  `,
})
export class DocLinkPickerComponent {
  protected readonly state = inject(AuditStateService);

  readonly query = signal("");
  readonly pick = output<IcDocument>();
  readonly cancelled = output<void>();

  readonly dirLabel = dirOf;

  /** 全部文件（按 rel_path 排序），搜索时按文件名/目录忽略大小写过滤。 */
  readonly filtered = computed(() => {
    const q = this.query().trim().toLowerCase();
    const docs = [...this.state.docs()].sort((a, b) =>
      a.rel_path.localeCompare(b.rel_path, "zh"),
    );
    if (!q) return docs;
    return docs.filter((d) => d.rel_path.toLowerCase().includes(q));
  });
}
