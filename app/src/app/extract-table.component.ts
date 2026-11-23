import {
  AfterViewInit,
  CUSTOM_ELEMENTS_SCHEMA,
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  HostListener,
  OnDestroy,
  computed,
  effect,
  inject,
  signal,
} from "@angular/core";
import { FormsModule } from "@angular/forms";
import { ClarityIcons } from "@cds/core/icon";
import { arrowIcon } from "@cds/core/icon/shapes/arrow.js";
import { exclamationTriangleIcon } from "@cds/core/icon/shapes/exclamation-triangle.js";
import { filterIcon } from "@cds/core/icon/shapes/filter.js";
import { twoWayArrowsIcon } from "@cds/core/icon/shapes/two-way-arrows.js";
import "@cds/core/icon/register.js";
import { ClrSignpostModule } from "@clr/angular";
import { AppStateService } from "./app-state.service";
import {
  CellData,
  ExtractResultInfo,
  SourceFileInfo,
} from "./backend.models";
import { KeywordCol } from "./keyword-columns";
import { ModalComponent } from "./modal.component";

// 表头排序/筛选图标 + 未提取关键词列头提示（signpost 锚点）
ClarityIcons.addIcons(arrowIcon, filterIcon, twoWayArrowsIcon, exclamationTriangleIcon);

type SortDir = "asc" | "desc";

/** 列标识：'file' / 关键词列的 columnIndex 字符串。 */
type ColKey = string;

/** 表格行：上传文件 +（可能尚未产生的）提取结果。 */
interface TableRow {
  file: SourceFileInfo;
  result: ExtractResultInfo | null;
}

const CHECK_COL_PX = 32;
const MIN_COL_PX = 48;
/** 测量默认列宽上限（初始铺满用；手动拖宽不限宽度，超出横向滚动）。 */
const MAX_COL_PX = 384;

/**
 * 提取结果表格（行随上传即出现，提取后填充）。
 * datagrid 风格：文件名列冻结、三态排序（双向箭头=未排序）、
 * 表头拖动调列宽、图标筛选、固定高度内部滚动；
 * 多选重跑/删除文件/全部重置、编辑模式（textarea 多行）、PDF 联动。
 */
@Component({
  selector: "app-extract-table",
  changeDetection: ChangeDetectionStrategy.OnPush,
  schemas: [CUSTOM_ELEMENTS_SCHEMA],
  imports: [FormsModule, ModalComponent, ClrSignpostModule],
  templateUrl: "./extract-table.component.html",
  styleUrl: "./extract-table.component.css",
})
export class ExtractTableComponent implements AfterViewInit, OnDestroy {
  protected readonly state = inject(AppStateService);

  /** 编辑态：当前编辑的 cell key 与编辑值 */
  readonly editingKey = signal<string | null>(null);
  readonly editValue = signal("");
  /** 本次会话内编辑过的 cell key（单元格「改」标记只跟踪本会话；
   *  存疑「疑」标记由后端 cell.suspect 驱动，人工编辑后后端自动清除） */
  readonly sessionEdited = signal<ReadonlySet<string>>(new Set());

  // ---- 多选（源文件 id）----
  readonly selectedIds = signal<ReadonlySet<number>>(new Set());

  // ---- 筛选 / 排序 ----
  readonly sortKey = signal<ColKey | null>(null);
  readonly sortDir = signal<SortDir>("asc");
  readonly filters = signal<Partial<Record<ColKey, string>>>({});
  /** 筛选弹层已改用 ClrSignpost（开关自管），不再需要 open 态信号。 */

  // ---- 列宽（用户拖动覆盖 + 内容测量默认）----
  /** 用户拖动后的列宽覆盖。 */
  readonly colWidths = signal<Partial<Record<ColKey, number>>>({});
  /** 内容测量出的默认列宽（随数据变化重算）。 */
  private readonly measured = signal<Partial<Record<ColKey, number>>>({});
  private resizing: { key: ColKey; startX: number; startW: number } | null =
    null;
  private measureCtx: CanvasRenderingContext2D | null = null;
  private readonly host = inject(ElementRef);
  private scrollResizeObs: ResizeObserver | null = null;

  // ---- 重跑 / 全部重置 / 删除文件确认 ----
  readonly rerunKind = signal<"parse" | "extract" | null>(null);
  readonly resetOpen = signal(false);
  readonly deleteOpen = signal(false);
  readonly rerunning = signal(false);

  /** 编辑模式：开启后双击单元格进入 textarea 编辑；关闭后仅保留定位跳转。 */
  readonly editMode = signal(false);

  /** 表格列以「已保存」配置为准（C 列起按 column_index 升序） */
  readonly savedCols = computed<KeywordCol[]>(() =>
    this.state.extractConfigs().map((c) => ({
      columnIndex: c.column_index,
      keyword: c.keyword,
      definition: c.definition,
      infoType: c.info_type as KeywordCol["infoType"],
    })),
  );

  /** 提取后新增、尚未参与提取的关键词列（结果已存在但该列无数据）。
   *  列头文字右侧以 signpost 提示，不触发节点级状态反应。 */
  readonly unextractedCols = computed<ReadonlySet<number>>(() => {
    const results = this.state.extractResults();
    if (results.length === 0) return new Set();
    const used = new Set(results.flatMap((r) => Object.keys(r.cell_data)));
    return new Set(
      this.state
        .extractConfigs()
        .filter((c) => !used.has(String(c.column_index)))
        .map((c) => c.column_index),
    );
  });

  /** 行模型：上传即出现；提取结果按源文件 join。 */
  readonly rows = computed<TableRow[]>(() => {
    const byFile = new Map(
      this.state.extractResults().map((r) => [r.source_file_id, r]),
    );
    return this.state.files().map((file) => ({
      file,
      result: byFile.get(file.id) ?? null,
    }));
  });

  /** 筛选 + 排序后的行。 */
  readonly viewRows = computed(() => {
    const fs = this.filters();
    let rows = this.rows().filter((row) =>
      Object.entries(fs).every(([k, q]) => {
        const query = (q ?? "").trim().toLowerCase();
        return !query || this.cellText(row, k).toLowerCase().includes(query);
      }),
    );
    const k = this.sortKey();
    if (k) {
      const dir = this.sortDir() === "asc" ? 1 : -1;
      rows = [...rows].sort(
        (a, b) => this.cellText(a, k).localeCompare(this.cellText(b, k)) * dir,
      );
    }
    return rows;
  });

  /** 选中文件 id（重跑直接可用）。 */
  readonly selectedFileIds = computed(() => [...this.selectedIds()]);

  readonly allChecked = computed(
    () =>
      this.viewRows().length > 0 &&
      this.viewRows().every((r) => this.selectedIds().has(r.file.id)),
  );

  constructor() {
    // 文件集变化时清理失效选择
    effect(() => {
      const alive = new Set(this.state.files().map((f) => f.id));
      const sel = this.selectedIds();
      const next = new Set([...sel].filter((id) => alive.has(id)));
      if (next.size !== sel.size) this.selectedIds.set(next);
    });
    // 数据/列变化时重测默认列宽（未手动拖动的列跟随内容）
    effect(() => {
      this.rows();
      this.savedCols();
      this.recomputeMeasured();
    });
  }

  ngAfterViewInit(): void {
    // 容器宽度变化（窗口/侧栏/缩放）→ 重测默认列宽铺满
    const el = this.host.nativeElement.querySelector(".extract-scroll");
    if (el && typeof ResizeObserver !== "undefined") {
      this.scrollResizeObs = new ResizeObserver(() => this.recomputeMeasured());
      this.scrollResizeObs.observe(el);
    }
  }

  ngOnDestroy(): void {
    this.scrollResizeObs?.disconnect();
  }

  /** 表格总宽（px，= 勾选列 + 各数据列生效宽之和）：显式宽度避免引擎在
   *  列多时对 max-content/fixed 布局的歧义处理导致无法调宽。 */
  readonly tableWidth = computed(() => {
    let w = CHECK_COL_PX + this.colWidthOf("file");
    for (const c of this.savedCols()) {
      w += this.colWidthOf(this.colKey(c));
    }
    return w;
  });

  /** 关键词列 → 列标识字符串（模板排序/筛选用）。 */
  colKey(c: KeywordCol): ColKey {
    return String(c.columnIndex);
  }

  /** 列头悬浮提示：选填的信息类型/关键词定义（均未设置则无提示）。 */
  colTooltip(c: KeywordCol): string | null {
    const parts: string[] = [];
    if (c.infoType) {
      const label =
        c.infoType === "number"
          ? "数字"
          : c.infoType === "date"
            ? "日期"
            : "文本";
      parts.push(`信息类型：${label}`);
    }
    if (c.definition) {
      parts.push(`定义：${c.definition}`);
    }
    return parts.length > 0 ? parts.join("\n") : null;
  }

  /** 列文本取值（排序/筛选/测宽共用）。 */
  cellText(row: TableRow, key: ColKey): string {
    if (key === "file") return row.file.original_name;
    return row.result?.cell_data[key]?.value ?? "";
  }

  // ---------------------------------------------------------------------------
  // 列宽：内容测量默认值 + 拖动覆盖
  // ---------------------------------------------------------------------------

  /** 生效列宽（px）：用户覆盖优先，其次内容测量。 */
  colWidthOf(key: ColKey): number {
    return this.colWidths()[key] ?? this.measured()[key] ?? 96;
  }

  /** 冻结列偏移：文件名列 left（勾选列宽）。 */
  leftOfFile(): number {
    return CHECK_COL_PX;
  }

  private measureText(text: string): number {
    if (!this.measureCtx) {
      this.measureCtx = document.createElement("canvas").getContext("2d");
      this.measureCtx!.font = "11px sans-serif";
    }
    return this.measureCtx!.measureText(text).width;
  }

  /** 默认列宽 = 表头与内容样本的最大文本宽 + 内边距，夹在 [MIN, MAX]；
   *  总宽不足容器时把余量按比例分给关键词列（表格始终铺满容器，宽度不固定）。 */
  private recomputeMeasured(): void {
    const rows = this.rows().slice(0, 50);
    const next: Partial<Record<ColKey, number>> = {};
    const cols: [ColKey, string][] = [
      ["file", "文件名"],
      ...this.savedCols().map(
        (c): [ColKey, string] => [this.colKey(c), c.keyword],
      ),
    ];
    for (const [key, header] of cols) {
      let max = this.measureText(header) + 56; // 表头文字 + 排序/筛选图标与内边距
      for (const r of rows) {
        // 单行展示场景取第一行文本估宽
        const firstLine = this.cellText(r, key).split("\n")[0] ?? "";
        max = Math.max(max, this.measureText(firstLine) + 24);
      }
      next[key] = Math.round(Math.min(MAX_COL_PX, Math.max(MIN_COL_PX, max)));
    }
    // 余量分配：总宽 < 容器宽时，按比例放大关键词列铺满容器
    const containerW = this.scrollWidth() - CHECK_COL_PX - 8;
    const kwKeys = cols.slice(1).map(([k]) => k);
    const total = cols.reduce((s, [k]) => s + (next[k] ?? 0), 0);
    if (kwKeys.length > 0 && total < containerW) {
      const kwSum = kwKeys.reduce((s, k) => s + (next[k] ?? 0), 0);
      if (kwSum > 0) {
        const extra = containerW - total;
        for (const k of kwKeys) {
          next[k] = Math.round((next[k] ?? 0) + (extra * (next[k] ?? 0)) / kwSum);
        }
      }
    }
    this.measured.set(next);
  }

  /** 当前 .extract-scroll 容器宽度（测量基准）。 */
  private scrollWidth(): number {
    return (
      this.host.nativeElement.querySelector(".extract-scroll")?.clientWidth ??
      800
    );
  }

  /** 表头右缘拖动调列宽。 */
  startResize(key: ColKey, event: MouseEvent): void {
    event.preventDefault();
    event.stopPropagation();
    this.resizing = {
      key,
      startX: event.clientX,
      startW: this.colWidthOf(key),
    };
    const move = (e: MouseEvent) => {
      if (!this.resizing) return;
      // 手动拖宽不限最大宽度：超出容器时表格横向滚动
      const w = Math.max(
        MIN_COL_PX,
        this.resizing.startW + (e.clientX - this.resizing.startX),
      );
      const k = this.resizing.key;
      this.colWidths.update((m) => ({ ...m, [k]: Math.round(w) }));
    };
    const up = () => {
      this.resizing = null;
      document.removeEventListener("mousemove", move);
      document.removeEventListener("mouseup", up);
    };
    document.addEventListener("mousemove", move);
    document.addEventListener("mouseup", up);
  }

  // ---------------------------------------------------------------------------
  // 多选与重跑 / 全部重置 / 删除文件
  // ---------------------------------------------------------------------------

  toggleRow(id: number): void {
    this.selectedIds.update((sel) => {
      const next = new Set(sel);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  toggleAll(): void {
    const ids = this.viewRows().map((r) => r.file.id);
    this.selectedIds.update((sel) => {
      const next = new Set(sel);
      if (this.allChecked()) ids.forEach((id) => next.delete(id));
      else ids.forEach((id) => next.add(id));
      return next;
    });
  }

  isChecked(id: number): boolean {
    return this.selectedIds().has(id);
  }

  askRerun(kind: "parse" | "extract"): void {
    if (this.selectedIds().size > 0) this.rerunKind.set(kind);
  }

  async confirmRerun(): Promise<void> {
    const kind = this.rerunKind();
    if (!kind) return;
    this.rerunning.set(true);
    try {
      const fileIds = this.selectedFileIds();
      const ok =
        kind === "parse"
          ? await this.state.reparseFiles(fileIds)
          : await this.state.reextractFiles(fileIds);
      if (ok) {
        this.rerunKind.set(null);
        this.selectedIds.set(new Set());
      }
    } finally {
      this.rerunning.set(false);
    }
  }

  async confirmReset(): Promise<void> {
    const ok = await this.state.resetProject();
    if (ok) {
      this.resetOpen.set(false);
      this.selectedIds.set(new Set());
    }
  }

  async confirmDeleteFiles(): Promise<void> {
    this.rerunning.set(true);
    try {
      const ok = await this.state.deleteFiles(this.selectedFileIds());
      if (ok) {
        this.deleteOpen.set(false);
        this.selectedIds.set(new Set());
      }
    } finally {
      this.rerunning.set(false);
    }
  }

  /** 是否有手动拖动过的列宽（决定「重置列宽」可用性）。 */
  hasCustomWidths(): boolean {
    return Object.keys(this.colWidths()).length > 0;
  }

  /** 重置列宽：清除全部手动拖动覆盖，回到内容测量默认值（铺满容器）。 */
  resetColWidths(): void {
    this.colWidths.set({});
    this.recomputeMeasured();
  }

  /** 编辑模式开关；退出时取消未完成的单元格编辑。 */
  toggleEditMode(): void {
    this.editMode.update((v) => !v);
    if (!this.editMode()) {
      this.editingKey.set(null);
    }
  }

  // ---------------------------------------------------------------------------
  // 筛选 / 排序（三态：未排序 → 升序 → 降序 → 未排序）
  // ---------------------------------------------------------------------------

  toggleSort(key: ColKey): void {
    if (this.sortKey() !== key) {
      this.sortKey.set(key);
      this.sortDir.set("asc");
    } else if (this.sortDir() === "asc") {
      this.sortDir.set("desc");
    } else {
      this.sortKey.set(null); // 回到原始顺序
    }
  }

  sortDirection(key: ColKey): "up" | "down" | null {
    if (this.sortKey() !== key) return null;
    return this.sortDir() === "asc" ? "up" : "down";
  }

  @HostListener("document:click", ["$event"])
  onDocumentClick(event: MouseEvent): void {
    // 编辑中的单元格：点击文本框以外任意位置即退出编辑
    if (this.editingKey() != null) {
      const target = event.target as HTMLElement | null;
      if (!target?.closest("textarea.cell-editor")) {
        void this.commitEdit();
      }
    }
  }

  setFilter(key: ColKey, value: string): void {
    this.filters.update((f) => ({ ...f, [key]: value }));
  }

  hasFilter(key: ColKey): boolean {
    return !!this.filters()[key]?.trim();
  }

  // ---------------------------------------------------------------------------
  // 结果表格
  // ---------------------------------------------------------------------------

  keyOf(row: TableRow, colIndex: number): string {
    return `${row.result?.id ?? 0}:${colIndex}`;
  }

  cellOf(row: TableRow, colIndex: number): CellData | undefined {
    return row.result?.cell_data[String(colIndex)];
  }

  isSelected(row: TableRow, colIndex: number): boolean {
    return (
      row.result != null &&
      this.state.selectedCell()?.key === this.keyOf(row, colIndex)
    );
  }

  // ---------------------------------------------------------------------------
  // 单元格交互
  // ---------------------------------------------------------------------------

  onCellClick(row: TableRow, colIndex: number): void {
    if (!row.result) return;
    this.state.selectCell(row.result, colIndex);
  }

  startEdit(row: TableRow, colIndex: number): void {
    if (!row.result || !this.editMode()) return;
    // 已在编辑其他单元格：先提交（双击其他格 = 退出上一格编辑态）
    const prevKey = this.editingKey();
    if (prevKey === this.keyOf(row, colIndex)) return;
    if (prevKey != null) {
      void this.commitEdit();
    }
    this.editingKey.set(this.keyOf(row, colIndex));
    this.editValue.set(this.cellOf(row, colIndex)?.value ?? "");
  }

  /** Ctrl+Enter 保存（Enter 换行，多行编辑）。 */
  onEditorCtrlEnter(event: Event): void {
    event.preventDefault();
    void this.commitEdit();
  }

  async commitEdit(): Promise<void> {
    const key = this.editingKey();
    if (key == null) return; // blur 与 Ctrl+Enter 重复触发时幂等
    this.editingKey.set(null);
    const [idStr, colStr] = key.split(":");
    // 内容无变化：不保存、不标「改」
    const row = this.state.extractResults().find((r) => r.id === Number(idStr));
    const original = row?.cell_data[colStr]?.value ?? "";
    if (this.editValue() === original) return;
    const ok = await this.state.updateCell(
      Number(idStr),
      Number(colStr),
      this.editValue(),
    );
    if (ok) {
      this.sessionEdited.update((set) => new Set(set).add(key));
    }
  }

  cancelEdit(): void {
    this.editingKey.set(null);
  }

  openRowPdf(row: TableRow): void {
    this.state.openPdfForFile(row.file.id);
  }

  trackByFileId(_index: number, row: TableRow): number {
    return row.file.id;
  }

  trackByCol(_index: number, col: KeywordCol): number {
    return col.columnIndex;
  }
}
