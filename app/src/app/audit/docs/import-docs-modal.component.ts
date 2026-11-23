import {
  ChangeDetectionStrategy,
  Component,
  effect,
  inject,
  input,
  output,
  signal,
} from "@angular/core";
import { open } from "@tauri-apps/plugin-dialog";
import { ModalComponent } from "../../modal.component";
import { NoticeComponent } from "../../notice.component";
import { AuditStateService } from "../audit-state.service";

/**
 * 导入制度文件弹窗：选文件夹（tauri-plugin-dialog）→ 导入（后端异步执行，
 * 界面不冻结；小批量导入很快，无进度条）→ 显示结果。
 * 导入中弹窗可关闭（后台继续，完成后树自动刷新）。
 * 树数据由 AuditStateService.importDocs 内部刷新。
 */
@Component({
  selector: "app-import-docs-modal",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [ModalComponent, NoticeComponent],
  template: `
    <app-modal [open]="open()" title="导入制度文件" (closed)="onClose()">
      <div class="clr-form-control">
        <div class="clr-control-label">源文件夹（递归收集其中全部 PDF，导入软件数据库）</div>
        <div class="clr-control-container dir-picker">
          <button
            type="button"
            class="btn btn-sm"
            [disabled]="importing()"
            (click)="pickDir()"
          >
            选择文件夹…
          </button>
          <span class="picked-dir" [title]="srcDir()">
            {{ srcDir() || "未选择" }}
          </span>
        </div>
      </div>

      @if (importing()) {
        <p class="muted">导入中…</p>
      }
      @if (result(); as r) {
        <app-notice type="success" [closable]="false">
          导入完成：新导入 {{ r.imported }} 个文件@if (r.updated > 0) {，覆盖更新 {{ r.updated }} 个}
        </app-notice>
      }

      <div modalFooter>
        <button type="button" class="btn btn-sm btn-link" (click)="onClose()">
          {{ importing() ? "后台继续并关闭" : "关闭" }}
        </button>
        <button
          type="button"
          class="btn btn-sm btn-primary"
          [disabled]="!srcDir() || importing()"
          (click)="submit()"
        >
          {{ importing() ? "导入中…" : "开始导入" }}
        </button>
      </div>
    </app-modal>
  `,
  styles: `
    .dir-picker { display: flex; align-items: center; gap: 0.5rem; }
    .picked-dir {
      font-size: 0.6rem; color: var(--cds-alias-typography-color-400, #666);
      overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
    }
    .muted { color: var(--cds-alias-typography-color-400, #666); font-size: 0.65rem; }
  `,
})
export class ImportDocsModalComponent {
  protected readonly state = inject(AuditStateService);

  readonly open = input.required<boolean>();
  readonly closed = output<void>();

  readonly srcDir = signal("");
  readonly importing = signal(false);
  readonly result = signal<{ imported: number; updated: number } | null>(null);

  constructor() {
    // 弹窗每次打开时重置内部状态
    effect(() => {
      if (this.open()) {
        this.srcDir.set("");
        this.importing.set(false);
        this.result.set(null);
      }
    });
  }

  async pickDir(): Promise<void> {
    const dir = await open({ directory: true });
    if (typeof dir === "string") {
      this.srcDir.set(dir);
      this.result.set(null);
    }
  }

  async submit(): Promise<void> {
    const dir = this.srcDir();
    if (!dir || this.importing()) return;
    this.importing.set(true);
    try {
      const r = await this.state.importDocs(dir);
      if (r) {
        this.result.set(r);
      }
      // 失败时 state.errorMessage 已在页面级 notice 展示
    } finally {
      this.importing.set(false);
    }
  }

  /** 导入中也可关闭：导入在后台继续，完成后树自动刷新。 */
  onClose(): void {
    this.closed.emit();
  }
}
