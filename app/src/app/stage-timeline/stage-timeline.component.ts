import {
  CUSTOM_ELEMENTS_SCHEMA,
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  signal,
} from "@angular/core";
import { ClarityIcons } from "@cds/core/icon";
import { angleIcon } from "@cds/core/icon/shapes/angle.js";
import { circleIcon } from "@cds/core/icon/shapes/circle.js";
import { errorStandardIcon } from "@cds/core/icon/shapes/error-standard.js";
import { exclamationTriangleIcon } from "@cds/core/icon/shapes/exclamation-triangle.js";
import { successStandardIcon } from "@cds/core/icon/shapes/success-standard.js";
import "@cds/core/icon/register.js";
import { ClrDropdownModule, ClrSignpostModule } from "@clr/angular";
import { open } from "@tauri-apps/plugin-dialog";
import { AppStateService } from "../app-state.service";
import { KeywordModalComponent } from "../keyword-modal/keyword-modal.component";
import { ProgressPanelComponent } from "../progress-panel/progress-panel.component";

// 时间线状态形状 + 导出下拉箭头 + 过期提示 signpost 锚点图标
ClarityIcons.addIcons(
  circleIcon,
  successStandardIcon,
  errorStandardIcon,
  angleIcon,
  exclamationTriangleIcon,
);

/** 节点状态 → clr-ui timeline 视觉：circle / spinner-md / success-standard / error-standard。 */
type StepState = "not-started" | "processing" | "success" | "error";

/** 'YYYY-MM-DD HH:MM:SS' → 毫秒（比较用，空格换 T 保证解析一致）。 */
function tsOf(s: string): number {
  return Date.parse(s.replace(" ", "T"));
}

/**
 * 任务五阶段时间线（上传 → 解析 → 添加关键词 → 提取 → 导出）。
 * 节点按钮（Outline）下方常驻进度数字；提取节点在「部分文件重新解析」或
 * 「关键词变动」后转为出错态并附 signpost 提示，按钮转为「重新提取」。
 * 框内顶行：导出/复制成功 alert（可关闭）+ 查看具体进度按钮。
 */
@Component({
  selector: "app-stage-timeline",
  changeDetection: ChangeDetectionStrategy.OnPush,
  schemas: [CUSTOM_ELEMENTS_SCHEMA],
  imports: [
    ClrDropdownModule,
    ClrSignpostModule,
    KeywordModalComponent,
    ProgressPanelComponent,
  ],
  templateUrl: "./stage-timeline.component.html",
  styleUrl: "./stage-timeline.component.css",
})
export class StageTimelineComponent {
  protected readonly state = inject(AppStateService);

  readonly keywordOpen = signal(false);
  readonly progressOpen = signal(false);
  /** 框内顶行临时 alert（可关闭）：解析完成/提取完成/导出成功，同一时刻
   *  只显示一条——新事件直接替换旧提示，不并列。 */
  readonly topAlert = signal<string | null>(null);

  /** 点击导出后置位，等 exportTasks 出现最新 done 时弹成功提示。 */
  private watchExportDone = false;
  private lastExportAlertTaskId: number | null = null;

  constructor() {
    // 导出完成 → 成功 alert（显示项目内相对路径）
    effect(() => {
      const tasks = this.state.exportTasks();
      const latest = tasks[0];
      if (
        this.watchExportDone &&
        latest?.status === "done" &&
        latest.id !== this.lastExportAlertTaskId
      ) {
        this.watchExportDone = false;
        this.lastExportAlertTaskId = latest.id;
        this.topAlert.set(`已导出：${this.relOutputPath(latest.output_path)}`);
      }
    });
    // 解析/提取转入全部完成 → 替换为对应提示（含进入已完成任务的首秀）
    let prevParse: StepState | null = null;
    let prevExtract: StepState | null = null;
    effect(() => {
      const p = this.parseState();
      if (prevParse !== "success" && p === "success") {
        this.topAlert.set("已全部完成解析");
      }
      prevParse = p;
    });
    effect(() => {
      const e = this.extractState();
      if (prevExtract !== "success" && e === "success") {
        this.topAlert.set("已全部完成提取");
      }
      prevExtract = e;
    });
  }

  /** 导出产物路径显示为项目内相对路径（在项目目录外时原样显示）。 */
  private relOutputPath(path: string): string {
    const proj = this.state.selectedProject();
    if (!proj) return path;
    const root = `${proj.base_path}/${proj.project_dir}/`;
    return path.startsWith(root) ? path.slice(root.length) : path;
  }

  // ---------------------------------------------------------------------------
  // 节点状态推导
  // ---------------------------------------------------------------------------

  /** 1 上传文件：busy=进行中（仅初次无文件时）；已有文件=完成。
   *  已有文件时再上传不再闪"进行中" spinner（消除可见闪烁）。 */
  readonly uploadState = computed<StepState>(() => {
    if (this.state.files().length > 0) return "success";
    return this.state.busy() ? "processing" : "not-started";
  });

  /** 2 解析文件：进行中=有运行/排队（项目解析中）；有错=出错；全部解析完=完成。 */
  readonly parseState = computed<StepState>(() => {
    const total = this.state.files().length;
    if (total === 0) return "not-started";
    const proj = this.state.selectedProject();
    const qs = this.state.queueStatus();
    const s = qs?.summary.parse;
    if (s && (s.running > 0 || (s.pending > 0 && qs?.project_status === "parsing"))) {
      return "processing";
    }
    // 入队瞬间 queueStatus 尚未刷新的兜底
    if (!qs && proj?.status === "parsing") return "processing";
    if (s && s.error > 0) return "error";
    if (this.state.parsedFileCount() === total) return "success";
    return "not-started";
  });

  /** 解析进度数字（常驻：done/总数）。 */
  readonly parseNumbers = computed(() => {
    const s = this.state.queueStatus()?.summary.parse;
    return `${s?.done ?? 0}/${this.state.files().length}`;
  });

  /** 3 添加关键词：有已保存关键词=完成。 */
  readonly keywordState = computed<StepState>(() =>
    this.state.extractConfigs().length > 0 ? "success" : "not-started",
  );

  readonly keywordCount = computed(() => this.state.extractConfigs().length);

  /** 部分文件重新解析后（显式事件标记）：仍有已解析未提取文件 → 提取过期。 */
  readonly reparseStale = computed(() => {
    if (!this.state.reparseStaleFlag()) return false;
    const proj = this.state.selectedProject();
    if (!proj || proj.status === "parsing" || proj.status === "extracting") {
      return false;
    }
    const results = this.state.extractResults();
    if (results.length === 0) return false;
    const extracted = new Set(results.map((r) => r.source_file_id));
    const parsedIds = (this.state.queueStatus()?.files ?? [])
      .filter((f) => f.parse_status === "done")
      .map((f) => f.file_id);
    return parsedIds.some((id) => !extracted.has(id));
  });

  /** 关键词在提取之后又被修改（非新增）→ 需重新提取。
   *  提取后新增的关键词不算过期：不触发节点反应，表格列头另有 signpost 提示。 */
  readonly keywordStale = computed(() => {
    const cfgs = this.state.extractConfigs();
    const results = this.state.extractResults();
    if (cfgs.length === 0 || results.length === 0) return false;
    // 新增列（column_index 不在任何结果的 cell_data 中）→ 交由列头提示
    const resultCols = new Set(results.flatMap((r) => Object.keys(r.cell_data)));
    if (cfgs.some((c) => !resultCols.has(String(c.column_index)))) return false;
    const maxCfg = Math.max(...cfgs.map((c) => tsOf(c.created_at)));
    const maxRes = Math.max(...results.map((r) => tsOf(r.created_at)));
    return maxCfg > maxRes;
  });

  /** 提取过期（任一原因）：节点出错态 + signpost 提示 + 按钮转「重新提取」。 */
  readonly extractStale = computed(
    () => this.reparseStale() || this.keywordStale(),
  );

  /** signpost 提示文案。 */
  readonly extractStaleText = computed(() =>
    this.keywordStale()
      ? "关键词已变动，需重新提取信息"
      : "部分文件已重新解析，请确认关键词后，点击重新提取",
  );

  /**
   * 4 提取信息。状态由任务中所有文件的提取状态推导（queueStatus 与后端
   * eligible_files 同口径；projects.status 仅作入队瞬间的辅助）：
   * 1. 任一文件提取中/排队中 → 进行中（转圈）；
   * 2. 无进行中且存在提取失败 → 出错（删除失败文件后自然重算，规则 4）；
   * 3. 关键词变动/部分重解析（过期）→ 出错 + signpost 提示重新提取；
   * 4. 全部文件提取成功 → 已完成；
   * 5. 其余 → 未开始。
   * 刷新时点：文件状态变化（选中重提/待提取/提取中/完成/失败/删除/重置）
   * 由 queue://progress 事件 + 2s 轮询 + 操作后 selectProject 全量刷新驱动。
   */
  readonly extractState = computed<StepState>(() => {
    if (this.state.files().length === 0) return "not-started";
    const proj = this.state.selectedProject();
    const qs = this.state.queueStatus();
    const s = qs?.summary.extract;
    // 1. 提取中：有运行/排队（项目提取中）文件
    if (s && (s.running > 0 || (s.pending > 0 && qs?.project_status === "extracting"))) {
      return "processing";
    }
    // 入队瞬间 queueStatus 尚未刷新的兜底（乐观更新已置 extracting）
    if (!qs && proj?.status === "extracting") return "processing";
    // 2. 存在提取失败
    if (s && s.error > 0) return "error";
    // 3. 关键词变动/部分文件重新解析导致提取过期 → 出错 + signpost
    if (this.extractStale()) return "error";
    // 4. 全部提取成功：有结果且任务内所有文件均提取完成（新上传/未解析
    // 文件的 extract_status 为 pending，节点回到未开始——与解析节点一致）
    const resultsExist = this.state.extractResults().length > 0;
    if (qs) {
      const qf = qs.files;
      if (
        resultsExist &&
        qf.length > 0 &&
        qf.every((f) => f.extract_status === "done")
      ) {
        return "success";
      }
    } else if (resultsExist && this.extractEligibleCount() === 0) {
      return "success";
    }
    return "not-started";
  });

  /** 提取进度数字（常驻：done/本次涉及文件数）。 */
  readonly extractNumbers = computed(() => {
    const s = this.state.queueStatus()?.summary.extract;
    if (!s) return "0/0";
    const total = s.done + s.pending + s.running + s.error;
    return `${s.done}/${total}`;
  });

  /** 5 导出结果：进行中>最新出错>已有成功>未开始。 */
  readonly exportState = computed<StepState>(() => {
    const p = this.state.exportProgress();
    if (p && (p.status === "running" || p.status === "pending")) {
      return "processing";
    }
    const tasks = this.state.exportTasks();
    if (tasks.length === 0) return "not-started";
    if (tasks.some((t) => t.status === "running" || t.status === "pending")) {
      return "processing";
    }
    if (tasks[0].status === "error") return "error";
    return tasks.some((t) => t.status === "done") ? "success" : "not-started";
  });

  readonly exportNumbers = computed(() => {
    const p = this.state.exportProgress();
    return `${p?.done ?? 0}/${p?.total ?? 1}`;
  });

  // ---------------------------------------------------------------------------
  // 操作可用性与提示
  // ---------------------------------------------------------------------------

  readonly canStartParse = computed(() => {
    const proj = this.state.selectedProject();
    if (!proj || proj.status === "parsing" || proj.status === "extracting") {
      return false;
    }
    return this.state.hasPendingParse();
  });

  /** 可提取文件数：已解析但尚无成功提取结果的文件（错误行也可重提）。 */
  readonly extractEligibleCount = computed(() => {
    const qf = this.state.queueStatus()?.files ?? [];
    return qf.filter(
      (f) => f.parse_status === "done" && f.extract_status !== "done",
    ).length;
  });

  /** 提取按钮可用性：空闲 + 已保存关键词 + （有可提取文件 或 提取过期需重提）。
   *  防止对已全量提取完成的任务重复提交。 */
  readonly canExtractNow = computed(() => {
    const proj = this.state.selectedProject();
    if (!proj || proj.status === "extracting" || proj.status === "parsing") {
      return false;
    }
    if (this.state.extractConfigs().length === 0) return false;
    return this.extractEligibleCount() > 0 || this.extractStale();
  });

  readonly hasResults = computed(() => this.state.extractResults().length > 0);
  readonly exportRunning = computed(() => this.exportState() === "processing");

  // ---------------------------------------------------------------------------
  // 节点操作
  // ---------------------------------------------------------------------------

  async upload(): Promise<void> {
    const selected = await open({
      multiple: true,
      filters: [{ name: "PDF 文件", extensions: ["pdf"] }],
    });
    if (!selected) return; // 用户取消
    const paths = Array.isArray(selected) ? selected : [selected];
    await this.state.uploadPdfs(paths);
  }

  startParse(): void {
    void this.state.startParse();
  }

  /**
   * 提取按钮：过期时分流——关键词变动 → 全部已提取文件重提（删行后队列全量重跑）；
   * 否则（含部分文件重新解析）→ 入队提取，队列只处理尚无提取结果的文件。
   */
  onExtractClick(): void {
    if (this.keywordStale()) {
      const ids = [
        ...new Set(
          this.state.extractResults().map((r) => r.source_file_id),
        ),
      ];
      void this.state.reextractFiles(ids);
    } else {
      void this.state.startExtract();
    }
  }

  // ---- 导出下拉（ClrDropdown：开关/定位/外点关闭自管） ----

  onExport(kind: "table" | "full"): void {
    if (this.exportRunning() || !this.hasResults()) return;
    this.topAlert.set(null);
    this.watchExportDone = true;
    void this.state.startExport(kind);
  }

  /** 复制提取结果表格（含表头）为 TSV，便于粘贴到 Excel。 */
  async onCopy(): Promise<void> {
    this.topAlert.set(null);
    if (!this.hasResults()) return;
    const cols = this.state.extractConfigs();
    const esc = (v: string) => v.replace(/[\t\r\n]+/g, " ").trim();
    const header = ["文件名", ...cols.map((c) => c.keyword)];
    const lines = [header.map(esc).join("\t")];
    for (const row of this.state.extractResults()) {
      const cells = [
        esc(row.source_file_name),
        ...cols.map((c) =>
          esc(row.cell_data[String(c.column_index)]?.value ?? ""),
        ),
      ];
      lines.push(cells.join("\t"));
    }
    try {
      await navigator.clipboard.writeText(lines.join("\n"));
      this.topAlert.set("已复制到剪贴板");
    } catch (e) {
      this.state.errorMessage.set(
        `复制到剪贴板失败: ${e instanceof Error ? e.message : String(e)}`,
      );
    }
  }
}
