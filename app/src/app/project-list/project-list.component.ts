import {
  ChangeDetectionStrategy,
  Component,
  OnDestroy,
  OnInit,
  computed,
  inject,
  signal,
} from "@angular/core";
import { FormsModule } from "@angular/forms";
import { ClrComboboxModule } from "@clr/angular";
import { Router } from "@angular/router";
import { open } from "@tauri-apps/plugin-dialog";
import { AppStateService, POLL_INTERVAL_MS } from "../app-state.service";
import { Project } from "../backend.models";
import { ModalComponent } from "../modal.component";
import { NoticeComponent } from "../notice.component";

type SortKey = "name" | "group_name" | "created_at" | "updated_at";
type SortDir = "asc" | "desc";

const PAGE_SIZE = 20;

/** 状态列展示模型。 */
interface StatusView {
  running: string | null;
  done: boolean;
  empty: boolean;
  parsePending: number;
  parseError: number;
  extractPending: number;
  extractError: number;
}

/**
 * 项目页（顶部导航「项目」进入）：任务列表表格 + 搜索/排序/组筛选/分页 +
 * 状态列（进行中/待解析/待提取/失败计数/已完成），
 * 左上角「新增任务」，行内「进入」「删除」（删除有确认弹窗）。
 */
@Component({
  selector: "app-project-list",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [FormsModule, ClrComboboxModule, ModalComponent, NoticeComponent],
  templateUrl: "./project-list.component.html",
  styleUrl: "./project-list.component.css",
})
export class ProjectListComponent implements OnInit, OnDestroy {
  protected readonly state = inject(AppStateService);
  private readonly router = inject(Router);

  // ---- 搜索 / 筛选 / 排序 / 分页 ----
  readonly searchText = signal("");
  /** 所属项目筛选；空串 = 全部。 */
  readonly groupFilter = signal("");
  readonly sortKey = signal<SortKey>("created_at");
  readonly sortDir = signal<SortDir>("desc");
  readonly page = signal(1);
  readonly pageSize = PAGE_SIZE;

  // ---- 新增任务 ----
  readonly createOpen = signal(false);
  readonly newName = signal("");
  readonly newGroup = signal("");
  readonly newBasePath = signal("");
  /** 任务类型：parse 解析提取 | audit 内控测试（默认解析提取）。 */
  readonly newTaskType = signal<"parse" | "audit">("parse");
  readonly createError = signal<string | null>(null);
  readonly creating = signal(false);

  // ---- 删除任务 ----
  readonly deleteTarget = signal<Project | null>(null);

  /** 现有项目组名（去重排序），供所属项目下拉与筛选。 */
  readonly groups = computed(() => {
    const set = new Set<string>();
    for (const p of this.state.projects()) {
      if (p.group_name) set.add(p.group_name);
    }
    return [...set].sort();
  });

  /** 搜索 + 组筛选后的记录。 */
  readonly filtered = computed(() => {
    const q = this.searchText().trim().toLowerCase();
    const g = this.groupFilter();
    return this.state
      .projects()
      .filter(
        (p) =>
          (!q || p.name.toLowerCase().includes(q)) &&
          (!g || p.group_name === g),
      );
  });

  /** 排序后的记录。 */
  readonly sorted = computed(() => {
    const key = this.sortKey();
    const dir = this.sortDir() === "asc" ? 1 : -1;
    return [...this.filtered()].sort(
      (a, b) => String(a[key]).localeCompare(String(b[key])) * dir,
    );
  });

  readonly pageCount = computed(() =>
    Math.max(1, Math.ceil(this.sorted().length / PAGE_SIZE)),
  );

  /** 当前页的记录（页码越界时收敛到末页）。 */
  readonly pageRows = computed(() => {
    const p = Math.min(this.page(), this.pageCount());
    return this.sorted().slice((p - 1) * PAGE_SIZE, p * PAGE_SIZE);
  });

  /** 页码窗口：当前页 ±3，最多 7 个。 */
  readonly pageWindow = computed(() => {
    const n = this.pageCount();
    const cur = Math.min(this.page(), n);
    const start = Math.max(1, Math.min(cur - 3, n - 6));
    const end = Math.min(n, start + 6);
    return Array.from({ length: end - start + 1 }, (_, i) => start + i);
  });

  // ---- 表头排序 ----
  toggleSort(key: SortKey): void {
    if (this.sortKey() === key) {
      this.sortDir.update((d) => (d === "asc" ? "desc" : "asc"));
    } else {
      this.sortKey.set(key);
      // 文本列默认升序，时间列默认降序
      this.sortDir.set(key === "created_at" || key === "updated_at" ? "desc" : "asc");
    }
    this.page.set(1);
  }

  sortMark(key: SortKey): string {
    if (this.sortKey() !== key) return "";
    return this.sortDir() === "asc" ? " ▲" : " ▼";
  }

  onSearch(text: string): void {
    this.searchText.set(text);
    this.page.set(1);
  }

  onGroupFilter(group: string): void {
    this.groupFilter.set(group);
    this.page.set(1);
  }

  gotoPage(n: number): void {
    this.page.set(Math.min(Math.max(1, n), this.pageCount()));
  }

  // ---- 行操作 ----
  enter(p: Project): void {
    // 按任务类型分流：解析提取 → task/:id；内控测试 → audit/:id
    void this.router.navigate([p.task_type === "audit" ? "/audit" : "/task", p.id]);
  }

  /** 任务类型列展示文案。 */
  taskTypeLabel(p: Project): string {
    return p.task_type === "audit" ? "内控测试" : "解析提取";
  }

  askDelete(p: Project): void {
    this.deleteTarget.set(p);
  }

  async confirmDelete(): Promise<void> {
    const target = this.deleteTarget();
    if (!target) return;
    const ok = await this.state.deleteProject(target.id);
    if (ok) {
      this.deleteTarget.set(null);
    }
  }

  // ---- 新增任务 ----
  openCreate(): void {
    this.newName.set("");
    this.newGroup.set("");
    this.newBasePath.set("");
    this.newTaskType.set("parse");
    this.createError.set(null);
    this.createOpen.set(true);
  }

  async pickBasePath(): Promise<void> {
    const dir = await open({ directory: true });
    if (typeof dir === "string") {
      this.newBasePath.set(dir);
    }
  }

  async submitCreate(): Promise<void> {
    const name = this.newName().trim();
    const group = this.newGroup().trim();
    // 内控测试不落本地文件夹：无需存放目录，传空串
    const basePath = this.newTaskType() === "audit" ? "" : this.newBasePath();
    if (!name) {
      this.createError.set("任务名称不能为空");
      return;
    }
    if (!group) {
      this.createError.set("请选择或输入所属项目");
      return;
    }
    if (this.newTaskType() === "parse" && !basePath) {
      this.createError.set("请选择任务存放目录");
      return;
    }
    this.creating.set(true);
    try {
      const project = await this.state.createProject(
        basePath,
        name,
        group,
        this.newTaskType(),
      );
      if (project) {
        this.createOpen.set(false);
      } else {
        this.createError.set(this.state.errorMessage() ?? "创建失败");
      }
    } finally {
      this.creating.set(false);
    }
  }

  trackByProjectId(_index: number, project: Project): number {
    return project.id;
  }

  /** 所属项目可编辑下拉的自由输入解析（原样取输入文本）。 */
  readonly groupResolver = (text: string): string => text;

  // ---------------------------------------------------------------------------
  // 状态列（进行中轮询刷新）
  // ---------------------------------------------------------------------------

  private progressTimer: ReturnType<typeof setInterval> | null = null;

  ngOnInit(): void {
    void this.refreshProgress();
  }

  ngOnDestroy(): void {
    if (this.progressTimer) clearInterval(this.progressTimer);
  }

  /** 有任务进行中时每 POLL_INTERVAL_MS 轮询一次状态列，空闲即停。 */
  private async refreshProgress(): Promise<void> {
    await this.state.refreshProjectProgress();
    const active = [...this.state.projectProgress().values()].some(
      (p) => p.status === "parsing" || p.status === "extracting",
    );
    if (active && !this.progressTimer) {
      this.progressTimer = setInterval(() => void this.refreshProgress(), POLL_INTERVAL_MS);
    } else if (!active && this.progressTimer) {
      clearInterval(this.progressTimer);
      this.progressTimer = null;
    }
  }

  /** 状态列视图：进行中 > 已完成 > 待处理/失败计数 > 无文件。 */
  statusOf(p: Project): StatusView {
    const pr = this.state.projectProgress().get(p.id);
    const status = pr?.status ?? p.status;
    const zero: StatusView = {
      running: null,
      done: false,
      empty: false,
      parsePending: 0,
      parseError: 0,
      extractPending: 0,
      extractError: 0,
    };
    if (status === "parsing") return { ...zero, running: "正在解析中" };
    if (status === "extracting") return { ...zero, running: "正在提取中" };
    if (!pr || pr.files_total === 0) return { ...zero, empty: true };
    const parsePending = Math.max(
      0,
      pr.files_total - pr.parse_done - pr.parse_error,
    );
    const extractPending = Math.max(
      0,
      pr.parse_done - pr.extract_done - pr.extract_error,
    );
    const done =
      parsePending === 0 &&
      pr.parse_error === 0 &&
      extractPending === 0 &&
      pr.extract_error === 0;
    return {
      ...zero,
      done,
      parsePending,
      parseError: pr.parse_error,
      extractPending,
      extractError: pr.extract_error,
    };
  }
}
