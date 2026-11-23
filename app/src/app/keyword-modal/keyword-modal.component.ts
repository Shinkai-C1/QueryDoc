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
import { FormsModule } from "@angular/forms";
import { ClarityIcons } from "@cds/core/icon";
import { checkIcon } from "@cds/core/icon/shapes/check.js";
import "@cds/core/icon/register.js";
import { AppStateService } from "../app-state.service";
import {
  DEFINITION_MAX_CHARS,
  KeywordCol,
  normalize,
} from "../keyword-columns";
import { ModalComponent } from "../modal.component";

ClarityIcons.addIcons(checkIcon);

/** 信息类型下拉选项（选填；空串=未设置）。 */
const INFO_TYPE_OPTIONS: { value: KeywordCol["infoType"]; label: string }[] = [
  { value: "", label: "（未设置）" },
  { value: "text", label: "文本" },
  { value: "number", label: "数字" },
  { value: "date", label: "日期" },
];

/**
 * 关键词编辑弹窗（由时间线「添加关键词」节点打开）：
 * 表格形式编辑（列号 / 关键词），打开时以已保存配置为草稿；
 * 有未保存修改时关闭需二次确认（保存 / 不保存，不保存则维持已保存关键词）。
 */
@Component({
  selector: "app-keyword-modal",
  changeDetection: ChangeDetectionStrategy.OnPush,
  schemas: [CUSTOM_ELEMENTS_SCHEMA],
  imports: [FormsModule, ModalComponent],
  templateUrl: "./keyword-modal.component.html",
  styleUrl: "./keyword-modal.component.css",
})
export class KeywordModalComponent {
  protected readonly state = inject(AppStateService);

  readonly open = input.required<boolean>();
  readonly closed = output<void>();

  /** 关键词草稿（未保存的修改在保存前不影响已保存配置） */
  readonly cols = signal<KeywordCol[]>([]);
  readonly saving = signal(false);
  readonly keywordError = signal<string | null>(null);
  /** 「未保存修改」二次确认弹窗。 */
  readonly confirmOpen = signal(false);
  /** 「批量添加」子弹窗。 */
  readonly batchOpen = signal(false);
  readonly batchText = signal("");
  /** 批量添加结果提示（如跳过重复 N 个）。 */
  readonly batchHint = signal<string | null>(null);
  /** 保存成功提示（再次编辑即消失）。 */
  readonly savedHint = signal(false);
  /** 信息类型下拉选项（模板用）。 */
  readonly infoTypeOptions = INFO_TYPE_OPTIONS;
  /** 定义长度上限（模板 maxlength 与保存校验共用）。 */
  readonly definitionMaxChars = DEFINITION_MAX_CHARS;

  @ViewChild("keywordScroll") keywordScroll?: ElementRef<HTMLDivElement>;

  /** 已保存配置（结果表头同源，C 列起按 column_index 升序）。 */
  readonly savedCols = computed<KeywordCol[]>(() =>
    this.state.extractConfigs().map((c) => ({
      columnIndex: c.column_index,
      keyword: c.keyword,
      definition: c.definition,
      infoType: c.info_type as KeywordCol["infoType"],
    })),
  );

  readonly dirty = computed(
    () => normalize(this.cols()) !== normalize(this.savedCols()),
  );

  private prevOpen = false;

  constructor() {
    // 弹窗打开（false→true）时以已保存配置重置草稿
    effect(() => {
      const o = this.open();
      if (o && !this.prevOpen) {
        this.cols.set(this.savedCols().map((c) => ({ ...c })));
        this.keywordError.set(null);
        this.confirmOpen.set(false);
        this.savedHint.set(false);
      }
      this.prevOpen = o;
    });
    // 再次进入编辑（出现未保存修改）即撤下「保存成功」提示
    effect(() => {
      if (this.dirty()) this.savedHint.set(false);
    });
  }

  /** 新增行后滚动到草稿末尾。双 rAF 等渲染完成后再滚——
   *  否则按渲染前的 scrollHeight 滚，会停在倒数第二行。 */
  private scrollDraftToEnd(): void {
    requestAnimationFrame(() =>
      requestAnimationFrame(() => {
        const el = this.keywordScroll?.nativeElement;
        if (el) el.scrollTop = el.scrollHeight;
      }),
    );
  }

  addColumn(): void {
    const max = this.cols().reduce((m, c) => Math.max(m, c.columnIndex), 1);
    this.cols.update((cols) => [
      ...cols,
      { columnIndex: max + 1, keyword: "", definition: "", infoType: "" },
    ]);
    this.scrollDraftToEnd();
  }

  /** 批量添加：半角英文逗号分隔，trim 去空、跳过与草稿重复项。 */
  applyBatch(): void {
    const parts = this.batchText()
      .split(",")
      .map((s) => s.trim())
      .filter((s) => s.length > 0);
    if (parts.length === 0) {
      this.batchOpen.set(false);
      this.batchText.set("");
      return;
    }
    const existing = new Set(this.cols().map((c) => c.keyword.trim()));
    const fresh = [...new Set(parts)].filter((k) => !existing.has(k));
    const skipped = new Set(parts).size - fresh.length;
    let max = this.cols().reduce((m, c) => Math.max(m, c.columnIndex), 1);
    this.cols.update((cols) => [
      ...cols,
      // 批量添加只带关键词；定义/信息类型逐行补填
      ...fresh.map((keyword) => ({
        columnIndex: ++max,
        keyword,
        definition: "",
        infoType: "" as const,
      })),
    ]);
    this.batchHint.set(
      skipped > 0 ? `已添加 ${fresh.length} 个，跳过重复 ${skipped} 个` : null,
    );
    this.batchOpen.set(false);
    this.batchText.set("");
    this.scrollDraftToEnd();
  }

  removeColumn(index: number): void {
    this.cols.update((cols) => cols.filter((_, i) => i !== index));
  }

  updateKeyword(index: number, keyword: string): void {
    this.cols.update((cols) =>
      cols.map((c, i) => (i === index ? { ...c, keyword } : c)),
    );
  }

  updateDefinition(index: number, definition: string): void {
    this.cols.update((cols) =>
      cols.map((c, i) => (i === index ? { ...c, definition } : c)),
    );
  }

  updateInfoType(index: number, infoType: KeywordCol["infoType"]): void {
    this.cols.update((cols) =>
      cols.map((c, i) => (i === index ? { ...c, infoType } : c)),
    );
  }

  /** 整批保存；成功返回 true（草稿同步为已保存态，dirty 消除）。 */
  async save(): Promise<boolean> {
    const cols = this.cols();
    for (let i = 0; i < cols.length; i++) {
      if (!cols[i].keyword.trim()) {
        this.keywordError.set(`第 ${i + 1} 行关键词不能为空`);
        return false;
      }
      if (cols[i].definition.trim().length > DEFINITION_MAX_CHARS) {
        this.keywordError.set(
          `第 ${i + 1} 行关键词定义超过 ${DEFINITION_MAX_CHARS} 字上限`,
        );
        return false;
      }
    }
    this.keywordError.set(null);
    this.saving.set(true);
    try {
      const ok = await this.state.saveKeywords(
        cols.map((c) => ({
          column_index: c.columnIndex,
          keyword: c.keyword.trim(),
          definition: c.definition.trim(),
          info_type: c.infoType,
        })),
      );
      if (ok) {
        this.cols.update((list) =>
          list.map((c) => ({
            ...c,
            keyword: c.keyword.trim(),
            definition: c.definition.trim(),
          })),
        );
        this.savedHint.set(true);
      }
      return ok;
    } finally {
      this.saving.set(false);
    }
  }

  /** 取消/关闭入口：有未保存修改时先二次确认。 */
  requestClose(): void {
    if (this.dirty()) {
      this.confirmOpen.set(true);
    } else {
      this.closed.emit();
    }
  }

  /** 确认弹窗-保存：保存成功后一并关闭。 */
  async onConfirmSave(): Promise<void> {
    const ok = await this.save();
    if (ok) {
      this.confirmOpen.set(false);
      this.closed.emit();
    }
  }

  /** 确认弹窗-不保存：放弃草稿，维持当前已保存关键词。 */
  onConfirmDiscard(): void {
    this.confirmOpen.set(false);
    this.closed.emit();
  }

  trackByCol(_index: number, col: KeywordCol): number {
    return col.columnIndex;
  }
}
