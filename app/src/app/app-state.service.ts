import { Injectable, computed, inject, signal } from "@angular/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { errText } from "./error-text";
import {
  ExportTaskInfo,
  ExtractConfigInfo,
  ExtractConfigInput,
  ExtractResultInfo,
  LogPayload,
  ProgressPayload,
  Project,
  ProjectProgress,
  ProjectQueueStatus,
  SelectedCell,
  SettingsView,
  SourceFileInfo,
} from "./backend.models";
import { BackendService } from "./backend.service";

/** 队列状态/项目进度轮询间隔（任务页 tickQueue 与列表页状态列共用）。 */
export const POLL_INTERVAL_MS = 2000;
/** 任务日志前端环形缓冲上限（完整日志在任务目录 `任务日志.log`）。 */
const TASK_LOG_MAX_LINES = 500;

function projectActive(s: ProjectQueueStatus): boolean {
  if (s.project_status === "parsing" || s.project_status === "extracting") {
    return true;
  }
  return [s.summary.parse, s.summary.extract].some(
    (c) => c.pending > 0 || c.running > 0,
  );
}

/**
 * 全局页面状态（signal store）：项目列表、选中项目、文件、三阶段队列状态、解析进度。
 * 轮询策略：选中项目的 queue_status 每 2s 一次，空闲（无 pending/running）即停止；
 * 任何入队/上传/进度事件都会重新触发一次轮询。
 */
@Injectable({ providedIn: "root" })
export class AppStateService {
  private readonly backend = inject(BackendService);

  readonly projects = signal<Project[]>([]);
  /** 列表页状态列数据（project_id → 解析/提取进度计数）。 */
  readonly projectProgress = signal<ReadonlyMap<number, ProjectProgress>>(
    new Map(),
  );
  readonly selectedProjectId = signal<number | null>(null);
  readonly selectedProject = computed(
    () => this.projects().find((p) => p.id === this.selectedProjectId()) ?? null,
  );
  readonly files = signal<SourceFileInfo[]>([]);
  readonly queueStatus = signal<ProjectQueueStatus | null>(null);

  // ---- 提取（Phase 5）----
  /** 已保存的关键词列配置（C 列起，按 column_index 升序）。 */
  readonly extractConfigs = signal<ExtractConfigInfo[]>([]);
  readonly extractResults = signal<ExtractResultInfo[]>([]);
  /** 当前选中单元格（PDF 联动高亮数据源）；null = 无选中。 */
  readonly selectedCell = signal<SelectedCell | null>(null);
  /** 单元格点击序号：每次点击 +1，驱动 PDF 黄色闪烁（同一格重复点击也触发）。 */
  readonly flashSeq = signal(0);
  /** PDF 预览当前加载的源文件 id；null = 未加载。 */
  readonly pdfSourceFileId = signal<number | null>(null);
  /** PDF 预览窗格收起态（true = 收起为右侧窄条）。 */
  readonly pdfCollapsed = signal(false);
  /** 重新解析后提取结果过期标记：reparseFiles 成功置位，
   *  一次提取运行结束（done/error）或切换/重置任务时清除。 */
  readonly reparseStaleFlag = signal(false);

  // ---- 导出（Phase 6）----
  readonly exportProgress = signal<ProgressPayload | null>(null);
  readonly exportTasks = signal<ExportTaskInfo[]>([]);

  // ---- 任务日志（进度面板滚动区；完整文件在任务目录）----
  readonly taskLogs = signal<string[]>([]);

  /** 页面级错误提示（alert 展示，可手动关闭）。 */
  readonly errorMessage = signal<string | null>(null);
  /** 上传/删除等短操作的进行中标记（禁用按钮）。 */
  readonly busy = signal(false);

  // ---- 设置（provider 分存；任务页徽标数据源）----
  readonly settingsView = signal<SettingsView | null>(null);
  /** 当前使用的 LLM provider（默认 ollama，与后端缺省一致）。 */
  readonly activeProvider = computed(
    () => this.settingsView()?.active_provider ?? "ollama",
  );

  /** 当前项目是否存在可解析（parse_status=pending）的文件。 */
  readonly hasPendingParse = computed(
    () =>
      this.queueStatus()?.files.some((f) => f.parse_status === "pending") ??
      false,
  );

  /** 已解析完成的文件数（提取的前置条件）。 */
  readonly parsedFileCount = computed(
    () =>
      this.queueStatus()?.files.filter((f) => f.parse_status === "done")
        .length ?? 0,
  );

  private queuePollTimer: ReturnType<typeof setTimeout> | null = null;
  private unlistenProgress: UnlistenFn | null = null;
  private unlistenLog: UnlistenFn | null = null;

  async init(): Promise<void> {
    try {
      this.unlistenProgress = await listen<ProgressPayload>(
        "queue://progress",
        (event) => this.onProgress(event.payload),
      );
      this.unlistenLog = await listen<LogPayload>("queue://log", (event) => {
        const p = event.payload;
        if (p.project_id !== this.selectedProjectId()) return;
        this.taskLogs.update((lines) => {
          const next = [...lines, p.line];
          return next.length > TASK_LOG_MAX_LINES
            ? next.slice(next.length - TASK_LOG_MAX_LINES)
            : next;
        });
      });
    } catch {
      // 非 Tauri 环境（纯浏览器调试）下无事件桥，忽略
    }
    await Promise.all([this.refreshProjects(), this.refreshSettings()]);
  }

  /** 刷新设置视图（init 与设置保存后调用；徽标/预估按钮数据源）。 */
  async refreshSettings(): Promise<void> {
    try {
      this.settingsView.set(await this.backend.getSettings());
    } catch {
      // 设置读取失败不打断页面
    }
  }

  destroy(): void {
    this.unlistenProgress?.();
    this.unlistenLog?.();
    if (this.queuePollTimer) clearTimeout(this.queuePollTimer);
  }

  async refreshProjects(): Promise<void> {
    try {
      const list = await this.backend.listProjects();
      this.projects.set(list);
      // 列表页无选中态：仅清理已失效的选中项目（如被删除）
      const id = this.selectedProjectId();
      if (id != null && !list.some((p) => p.id === id)) {
        this.selectedProjectId.set(null);
        this.files.set([]);
        this.queueStatus.set(null);
        this.extractConfigs.set([]);
        this.extractResults.set([]);
        this.exportTasks.set([]);
        this.taskLogs.set([]);
      }
    } catch (e) {
      this.errorMessage.set(`加载任务列表失败: ${errText(e)}`);
    }
    await this.refreshProjectProgress();
  }

  /** 刷新列表页状态列（每项目的解析/提取进度计数）。 */
  async refreshProjectProgress(): Promise<void> {
    try {
      const list = await this.backend.listProjectProgress();
      this.projectProgress.set(new Map(list.map((p) => [p.project_id, p])));
    } catch {
      // 状态列失败不打断页面
    }
  }

  async selectProject(id: number): Promise<void> {
    this.selectedProjectId.set(id);
    this.exportProgress.set(null);
    this.selectedCell.set(null);
    this.pdfSourceFileId.set(null);
    this.reparseStaleFlag.set(false);
    this.taskLogs.set([]);
    // 项目列表一并刷新：保证 selectedProject().status 不残留过期状态
    //（如全部重置/重跑后项目状态已回 idle）
    await this.refreshProjects();
    if (this.selectedProjectId() !== id) return; // 刷新后发现项目已不存在
    await Promise.all([
      this.refreshFiles(),
      this.tickQueue(),
      this.refreshExtractConfigs(),
      this.refreshExtractResults(),
      this.refreshExportTasks(),
      this.refreshTaskLog(),
    ]);
  }

  /** 回填任务日志历史（读日志文件尾部；之后由 queue://log 事件增量追加）。 */
  async refreshTaskLog(): Promise<void> {
    const id = this.selectedProjectId();
    if (id == null) {
      this.taskLogs.set([]);
      return;
    }
    try {
      const text = await this.backend.readProjectLog(id);
      if (this.selectedProjectId() !== id) return;
      const lines = text.split("\n").filter((l) => l.length > 0);
      this.taskLogs.set(lines.slice(-TASK_LOG_MAX_LINES));
    } catch {
      // 日志读取失败不打断页面
    }
  }

  async refreshFiles(): Promise<void> {
    const id = this.selectedProjectId();
    if (id == null) {
      this.files.set([]);
      return;
    }
    try {
      const files = await this.backend.listFiles(id);
      if (this.selectedProjectId() === id) {
        this.files.set(files);
      }
    } catch (e) {
      this.errorMessage.set(`加载文件列表失败: ${errText(e)}`);
    }
  }

  async createProject(
    basePath: string,
    name: string,
    groupName: string,
    taskType?: string,
  ): Promise<Project | null> {
    try {
      const project = await this.backend.createProject(
        basePath,
        name,
        groupName,
        taskType,
      );
      await this.refreshProjects();
      return project;
    } catch (e) {
      this.errorMessage.set(`创建任务失败: ${errText(e)}`);
      return null;
    }
  }

  async deleteProject(id: number): Promise<boolean> {
    this.busy.set(true);
    try {
      await this.backend.deleteProject(id);
      await this.refreshProjects();
      return true;
    } catch (e) {
      this.errorMessage.set(`删除任务失败: ${errText(e)}`);
      return false;
    } finally {
      this.busy.set(false);
    }
  }

  /** 全部重置：删除解析/提取/导出记录与关键词配置，文件回到待解析。 */
  async resetProject(): Promise<boolean> {
    const id = this.selectedProjectId();
    if (id == null) return false;
    this.busy.set(true);
    try {
      await this.backend.resetProject(id);
      // 全量重拉（selectProject 会重置进度类信号并刷新各列表）
      await this.selectProject(id);
      return true;
    } catch (e) {
      this.errorMessage.set(`全部重置失败: ${errText(e)}`);
      return false;
    } finally {
      this.busy.set(false);
    }
  }

  /** 对选中文件重新解析（级联清除其提取结果）；成功后标记提取过期。 */
  async reparseFiles(fileIds: number[]): Promise<boolean> {
    const id = this.selectedProjectId();
    if (id == null || fileIds.length === 0) return false;
    try {
      await this.backend.reparseFiles(id, fileIds);
      this.reparseStaleFlag.set(true);
      await this.selectProject(id);
      // selectProject 会清标记，重新置位（本次重解析的过期状态持续有效）
      this.reparseStaleFlag.set(true);
      return true;
    } catch (e) {
      this.errorMessage.set(`重新解析失败: ${errText(e)}`);
      return false;
    }
  }

  /** 对选中文件重新提取。 */
  async reextractFiles(fileIds: number[]): Promise<boolean> {
    const id = this.selectedProjectId();
    if (id == null || fileIds.length === 0) return false;
    try {
      await this.backend.reextractFiles(id, fileIds);
      await this.selectProject(id);
      // 乐观更新：明细立即显示提取中、旧错误文本清除
      this.markExtractingOptimistic();
      return true;
    } catch (e) {
      this.errorMessage.set(`重新提取失败: ${errText(e)}`);
      return false;
    }
  }

  /** 删除选中的上传文件（其解析、提取结果一并删除）。 */
  async deleteFiles(fileIds: number[]): Promise<boolean> {
    const id = this.selectedProjectId();
    if (id == null || fileIds.length === 0) return false;
    this.busy.set(true);
    try {
      await this.backend.deleteFiles(id, fileIds);
      // 被删文件可能正在 PDF 预览，清掉
      if (this.pdfSourceFileId() != null && fileIds.includes(this.pdfSourceFileId()!)) {
        this.pdfSourceFileId.set(null);
      }
      await this.selectProject(id);
      return true;
    } catch (e) {
      this.errorMessage.set(`删除文件失败: ${errText(e)}`);
      return false;
    } finally {
      this.busy.set(false);
    }
  }

  async uploadPdfs(paths: string[]): Promise<boolean> {
    const id = this.selectedProjectId();
    if (id == null || paths.length === 0) return false;
    this.busy.set(true);
    try {
      await this.backend.uploadFiles(id, paths);
      await this.refreshFiles();
      await this.tickQueue();
      return true;
    } catch (e) {
      // 后端校验失败时返回逐文件原因串，原样展示
      this.errorMessage.set(`上传失败: ${errText(e)}`);
      return false;
    } finally {
      this.busy.set(false);
    }
  }

  async startParse(): Promise<void> {
    const id = this.selectedProjectId();
    if (id == null) return;
    try {
      await this.backend.enqueueParse(id);
      await this.tickQueue();
    } catch (e) {
      this.errorMessage.set(`启动解析失败: ${errText(e)}`);
    }
  }

  // ---------------------------------------------------------------------------
  // 提取（Phase 5）
  // ---------------------------------------------------------------------------

  async refreshExtractConfigs(): Promise<void> {
    const id = this.selectedProjectId();
    if (id == null) {
      this.extractConfigs.set([]);
      return;
    }
    try {
      const configs = await this.backend.getExtractConfigs(id);
      if (this.selectedProjectId() === id) {
        this.extractConfigs.set(configs);
      }
    } catch (e) {
      this.errorMessage.set(`加载提取配置失败: ${errText(e)}`);
    }
  }

  async refreshExtractResults(): Promise<void> {
    const id = this.selectedProjectId();
    if (id == null) {
      this.extractResults.set([]);
      return;
    }
    try {
      const results = await this.backend.getExtractResults(id);
      if (this.selectedProjectId() === id) {
        this.extractResults.set(results);
      }
    } catch (e) {
      this.errorMessage.set(`加载提取结果失败: ${errText(e)}`);
    }
  }

  /** 整批保存关键词配置；成功返回 true。 */
  async saveKeywords(configs: ExtractConfigInput[]): Promise<boolean> {
    const id = this.selectedProjectId();
    if (id == null) return false;
    try {
      const saved = await this.backend.saveExtractConfigs(id, configs);
      this.extractConfigs.set(saved);
      return true;
    } catch (e) {
      this.errorMessage.set(`保存关键词失败: ${errText(e)}`);
      return false;
    }
  }

  async startExtract(): Promise<void> {
    const id = this.selectedProjectId();
    if (id == null) return;
    try {
      await this.backend.enqueueExtract(id);
      await this.tickQueue();
      // 乐观更新：节点/明细立即进入提取中（真实状态由后续轮询与事件覆盖）
      this.markExtractingOptimistic();
    } catch (e) {
      this.errorMessage.set(`启动提取失败: ${errText(e)}`);
    }
  }

  /** 提取入队成功后的乐观状态：立即显示提取中、清除待重提文件的旧错误文本。 */
  private markExtractingOptimistic(): void {
    this.queueStatus.update((s) => {
      if (!s || s.project_status === "extracting") return s;
      const files = s.files.map((f) =>
        f.parse_status === "done" && f.extract_status !== "done"
          ? { ...f, extract_status: "pending", extract_error: undefined }
          : f,
      );
      const pending = files.filter((f) => f.extract_status === "pending").length;
      const running = files.filter((f) => f.extract_status === "running").length;
      const done = files.filter((f) => f.extract_status === "done").length;
      return {
        ...s,
        project_status: "extracting",
        files,
        summary: { ...s.summary, extract: { pending, running, done, error: 0 } },
      };
    });
  }

  /** 选中单元格：记录选中态并触发一次闪烁序号（PDF 联动由 viewer 组件完成）。 */
  selectCell(row: ExtractResultInfo, colIndex: number): void {
    const cell = row.cell_data[String(colIndex)];
    const sel: SelectedCell = {
      key: `${row.id}:${colIndex}`,
      resultId: row.id,
      sourceFileId: row.source_file_id,
      colIndex,
      value: cell?.value ?? "",
      page_idx: cell?.page_idx ?? null,
      fragments: cell?.fragments ?? [],
    };
    this.selectedCell.set(sel);
    this.flashSeq.update((n) => n + 1);
    if (sel.page_idx != null) {
      // 有坐标：确保右侧加载对应 PDF（跳页由 viewer 处理）
      this.pdfSourceFileId.set(row.source_file_id);
    }
  }

  /** 文件名/行点击：右侧加载指定源文件 PDF（清除单元格选中）。
   *  同文件重复点击也强制触发一次联动（加载失败后的重试入口）。 */
  openPdfForFile(sourceFileId: number): void {
    this.selectedCell.set(null);
    if (this.pdfSourceFileId() === sourceFileId) {
      this.pdfSourceFileId.set(null);
    }
    this.pdfSourceFileId.set(sourceFileId);
  }

  /** 清空单元格选中（点击空白/失焦）。 */
  clearSelectedCell(): void {
    this.selectedCell.set(null);
  }

  /** 编辑保存单元格 value；成功后同步本地行与选中态。 */
  async updateCell(
    resultId: number,
    colIndex: number,
    value: string,
  ): Promise<boolean> {
    try {
      const updated = await this.backend.updateExtractCell(
        resultId,
        colIndex,
        value,
      );
      this.extractResults.update((rows) =>
        rows.map((r) => (r.id === updated.id ? updated : r)),
      );
      const sel = this.selectedCell();
      if (sel && sel.resultId === resultId && sel.colIndex === colIndex) {
        this.selectedCell.set({ ...sel, value });
      }
      return true;
    } catch (e) {
      this.errorMessage.set(`保存单元格失败: ${errText(e)}`);
      return false;
    }
  }

  // ---------------------------------------------------------------------------
  // 导出（Phase 6）
  // ---------------------------------------------------------------------------

  async refreshExportTasks(): Promise<void> {
    const id = this.selectedProjectId();
    if (id == null) {
      this.exportTasks.set([]);
      return;
    }
    try {
      const tasks = await this.backend.getExportTasks(id);
      if (this.selectedProjectId() === id) {
        this.exportTasks.set(tasks);
      }
    } catch (e) {
      this.errorMessage.set(`加载导出任务失败: ${errText(e)}`);
    }
  }

  async startExport(kind: "table" | "full"): Promise<void> {
    const id = this.selectedProjectId();
    if (id == null) return;
    try {
      await this.backend.enqueueExport(id, kind);
      this.exportProgress.set(null);
      await this.tickQueue();
    } catch (e) {
      this.errorMessage.set(
        `启动${kind === "table" ? "表格" : "含附件表格"}导出失败: ${errText(e)}`,
      );
    }
  }

  /** 全部停止：中断当前解析/提取并清空队列；随后全量刷新（节点回到待处理态）。 */
  async stopProject(): Promise<boolean> {
    const id = this.selectedProjectId();
    if (id == null) return false;
    try {
      await this.backend.stopProject(id);
      await this.tickQueue();
      await this.refreshFiles();
      await this.refreshExtractResults();
      return true;
    } catch (e) {
      this.errorMessage.set(`全部停止失败: ${errText(e)}`);
      return false;
    }
  }

  private onProgress(p: ProgressPayload): void {
    if (p.project_id !== this.selectedProjectId()) {
      return;
    }
    if (p.kind === "parse") {
      if (p.status === "done" || p.status === "error") {
        // 任务结束：刷新文件列表与队列状态
        void this.refreshFiles();
      }
    } else if (p.kind === "extract") {
      if (p.status === "done" || p.status === "error") {
        if (p.done === p.total) {
          // 提取整轮结束：重解析导致的过期标记随之清除
          this.reparseStaleFlag.set(false);
        }
        void this.refreshExtractResults();
      }
    } else if (p.kind === "export") {
      this.exportProgress.set(p);
      if (p.status === "done" || p.status === "error") {
        void this.refreshExportTasks();
      }
    }
    // 任何进度事件都驱动一次队列轮询（running 态也要即时反馈到节点与明细）
    void this.tickQueue();
  }

  /** 拉一次选中项目的 queue_status；活跃则 2s 后继续，空闲停止。 */
  private async tickQueue(): Promise<void> {
    if (this.queuePollTimer) {
      clearTimeout(this.queuePollTimer);
      this.queuePollTimer = null;
    }
    const id = this.selectedProjectId();
    if (id == null) return;
    try {
      const s = await this.backend.queueStatus(id);
      if (this.selectedProjectId() !== id) return; // 轮询期间已切换项目
      this.queueStatus.set(s);
      if (projectActive(s)) {
        this.queuePollTimer = setTimeout(
          () => void this.tickQueue(),
          POLL_INTERVAL_MS,
        );
      }
    } catch (e) {
      this.errorMessage.set(`查询队列状态失败: ${errText(e)}`);
    }
  }
}
