import {
  CUSTOM_ELEMENTS_SCHEMA,
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  ViewChild,
  computed,
  effect,
  inject,
  input,
  output,
  signal,
} from "@angular/core";
import { ClarityIcons } from "@cds/core/icon";
import { infoStandardIcon } from "@cds/core/icon/shapes/info-standard.js";
import "@cds/core/icon/register.js";
import { ClrDatagridModule, ClrSignpostModule } from "@clr/angular";
import { AppStateService } from "../app-state.service";
import { BackendService } from "../backend.service";
import { FileQueueStatus, SourceFileInfo } from "../backend.models";
import { FileSizePipe } from "../file-size.pipe";
import { MidEllipsisPipe } from "../middle-ellipsis.pipe";
import { ModalComponent } from "../modal.component";
import { StageBadgeComponent } from "../stage-badge.component";

ClarityIcons.addIcons(infoStandardIcon);

interface FileRow {
  file: SourceFileInfo;
  /** queue_status 中的三阶段状态；项目无 queue_status 时为 null。 */
  q: FileQueueStatus | null;
}

/** 「查看具体进度」右侧 Side Panel：各文件状态表（Clarity datagrid，
 *  错误信息 Expandable row 完整多行展示）+ 运行日志滚动区；
 *  标题栏「全部停止」中断并清空解析/提取任务。 */
@Component({
  selector: "app-progress-panel",
  changeDetection: ChangeDetectionStrategy.OnPush,
  schemas: [CUSTOM_ELEMENTS_SCHEMA],
  imports: [
    ClrDatagridModule,
    ClrSignpostModule,
    StageBadgeComponent,
    FileSizePipe,
    MidEllipsisPipe,
    ModalComponent,
  ],
  templateUrl: "./progress-panel.component.html",
  styleUrl: "./progress-panel.component.css",
})
export class ProgressPanelComponent {
  protected readonly state = inject(AppStateService);
  private readonly backend = inject(BackendService);

  readonly open = input.required<boolean>();
  readonly closed = output<void>();

  /** 「全部停止」确认弹窗。 */
  readonly stopOpen = signal(false);
  readonly stopping = signal(false);
  /** Token 预估：估算中标记 + 本轮失败文件的瞬时错误（file_id → error）。
   *  成功值由后端落库 source_files.estimated_tokens，经 refreshFiles 进列表，
   *  面板重开仍在；删除文件由后端随行连带删除。 */
  readonly estimating = signal(false);
  readonly estimateErrors = signal<ReadonlyMap<number, string>>(new Map());

  @ViewChild("logBox") logBox?: ElementRef<HTMLDivElement>;

  private prevOpen = false;

  constructor() {
    // 面板打开时默认滚到日志底部（看最新）；之后新日志到达时若用户
    // 已在底部附近则跟随滚底，上翻查看历史时不强制跳动
    effect(() => {
      const open = this.open();
      const justOpened = open && !this.prevOpen;
      this.prevOpen = open;
      this.state.taskLogs();
      setTimeout(() => {
        const el = this.logBox?.nativeElement;
        if (!el) return;
        const nearBottom =
          el.scrollHeight - el.scrollTop - el.clientHeight < 40;
        if (justOpened || nearBottom) el.scrollTop = el.scrollHeight;
      });
    });
  }

  /** 文件表行：静态文件信息 join 三阶段队列状态。 */
  readonly rows = computed<FileRow[]>(() => {
    const byId = new Map(
      (this.state.queueStatus()?.files ?? []).map((f) => [f.file_id, f]),
    );
    return this.state.files().map((file) => ({
      file,
      q: byId.get(file.id) ?? null,
    }));
  });

  /** queue_status 缺失时，从 source_files.status 推导解析阶段徽标状态。 */
  parseStatusOf(row: FileRow): string {
    if (row.q) return row.q.parse_status;
    switch (row.file.status) {
      case "parsing":
        return "running";
      default:
        return row.file.status; // pending/done/error 同名
    }
  }

  parsePositionOf(row: FileRow): number | null {
    return row.q?.parse_position ?? null;
  }

  /** 行错误全文（Expandable row 内容；无错误返回空串，行不可展开）。 */
  errorTextOf(row: FileRow): string {
    if (row.q?.parse_error) return `解析失败：${row.q.parse_error}`;
    if (row.q?.extract_error) return `提取失败：${row.q.extract_error}`;
    if (this.parseStatusOf(row) === "error") return "解析出错";
    return "";
  }

  /** 全部停止（确认弹窗 → stop_project → 全量刷新）。 */
  async confirmStop(): Promise<void> {
    this.stopping.set(true);
    try {
      const ok = await this.state.stopProject();
      if (ok) this.stopOpen.set(false);
    } finally {
      this.stopping.set(false);
    }
  }

  /** 逐文件估算提取输入 Token（仅 KIMI-API 可用）。
   *  后端逐文件落库后 refreshFiles 刷新列；本轮失败文件保留瞬时错误展示。 */
  async estimate(): Promise<void> {
    const id = this.state.selectedProjectId();
    if (id == null || this.state.activeProvider() === "ollama") return;
    this.estimating.set(true);
    this.estimateErrors.set(new Map());
    try {
      const list = await this.backend.estimateTokens(id);
      this.estimateErrors.set(
        new Map(
          list
            .filter((e) => e.tokens == null)
            .map((e) => [e.file_id, e.error ?? "未知错误"]),
        ),
      );
      await this.state.refreshFiles();
    } catch (e) {
      this.state.errorMessage.set(`Token 预估失败: ${e}`);
    } finally {
      this.estimating.set(false);
    }
  }

  /** 预估Token 列文本：本轮失败显示错误摘要；否则显示已落库预估值；未估算 —。 */
  tokenTextOf(row: FileRow): string {
    const err = this.estimateErrors().get(row.file.id);
    if (err != null) return `失败：${err.slice(0, 40)}`;
    const t = row.file.estimated_tokens;
    return t == null ? "—" : t.toLocaleString();
  }

  trackByFileId(_index: number, row: FileRow): number {
    return row.file.id;
  }
}

