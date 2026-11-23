import {
  ChangeDetectionStrategy,
  Component,
  input,
  output,
} from "@angular/core";

/**
 * 纯文本版模态框：使用 clr-ui 的 .modal/.modal-backdrop 样式类自绘。
 * 说明：ClrModalModule（@clr/angular/modal）内部关闭按钮依赖 @cds/core 的
 * cds-icon；按功能入口纯文本原则改用 CSS 类自绘，关闭/确认一律纯文本按钮
 *（@cds/core 图标仅用于时间线节点状态形状）。
 * closable=true 时标题行右侧渲染纯文本「✕」关闭按钮（对应 Clarity 模态框
 * 右上角关闭叉），点击经 closed 输出通知父组件（父组件决定关闭或二次确认）。
 */
@Component({
  selector: "app-modal",
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    @if (open()) {
      <div class="modal" role="dialog" aria-modal="true" [attr.aria-label]="title()">
        <div
          class="modal-dialog"
          [class.modal-sm]="size() === 'sm'"
          [class.modal-lg]="size() === 'lg'"
          [style.width]="width()"
        >
          <div class="modal-content">
            <div class="modal-header">
              <h3 class="qd-modal-title">{{ title() }}</h3>
              @if (closable()) {
                <div class="modal-title-close-wrapper">
                  <button
                    type="button"
                    class="modal-title-close qd-close"
                    [attr.aria-label]="'关闭' + title()"
                    (click)="closed.emit()"
                  >
                    ✕
                  </button>
                </div>
              }
            </div>
            <div class="modal-body">
              <ng-content />
            </div>
            <div class="modal-footer">
              <ng-content select="[modalFooter]" />
            </div>
          </div>
        </div>
      </div>
      <div class="modal-backdrop" aria-hidden="true"></div>
    }
  `,
  styles: `
    .qd-modal-title {
      margin: 0;
      flex: 1;
      min-width: 0;
      font-size: 0.9rem;
      font-weight: 600;
      color: var(--clr-modal-title-color, inherit);
    }
    .modal-header {
      display: flex;
      align-items: center;
      gap: 0.4rem;
      padding-bottom: 0.3rem;
    }
    .modal-title-close-wrapper {
      flex: none;
      margin-left: auto;
    }
    .modal-body {
      padding-top: 0.2rem;
    }
    /* footer 按钮横排间隔（默认右对齐；含 footer-spacer 时左侧元素靠左） */
    .modal-footer {
      display: flex;
      align-items: center;
      gap: 0.4rem;
      justify-content: flex-end;
    }
    .qd-close {
      border: none;
      background: none;
      padding: 0 0.1rem;
      font-size: 0.8rem;
      line-height: 1;
      cursor: pointer;
      color: var(--cds-alias-typography-color-400, #666);
    }
    .qd-close:hover {
      color: var(--cds-alias-typography-color-500, #333);
    }
  `,
})
export class ModalComponent {
  readonly open = input.required<boolean>();
  readonly title = input.required<string>();
  readonly size = input<"sm" | "md" | "lg">("md");
  /** 自定义弹窗宽度（如 '26.5rem'）；不设则按 size 的默认宽度。 */
  readonly width = input<string | null>(null);
  /** true 时标题行右侧显示「✕」关闭按钮（点击经 closed 通知父组件）。 */
  readonly closable = input(false);
  readonly closed = output<void>();
}
