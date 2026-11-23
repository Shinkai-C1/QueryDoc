import {
  ChangeDetectionStrategy,
  Component,
  computed,
  input,
  output,
} from "@angular/core";

export type NoticeType = "info" | "success" | "warning" | "error";

const LABELS: Record<NoticeType, string> = {
  info: "提示",
  success: "成功",
  warning: "注意",
  error: "错误",
};

/**
 * 通知提示框（《提示框前端设计要点.md》落地，无图标变体）：
 * - 浅色背景 + 左侧强调边框 + 深色文字，色值全走语义 token；
 * - error/warning → role="alert"（assertive）；info/success → role="status"（polite）；
 * - 状态图标按项目「无图标原则」以文字状态标签替代（aria-hidden，语义由 role 传达）；
 * - 关闭按钮为纯文本，带 aria-label，可 Tab 聚焦 Enter 激活。
 */
@Component({
  selector: "app-notice",
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <div
      class="notice"
      [class]="'notice notice--' + type()"
      [attr.role]="assertive() ? 'alert' : 'status'"
      [attr.aria-live]="assertive() ? 'assertive' : 'polite'"
    >
      <span class="notice-label" aria-hidden="true">{{ label() }}</span>
      <span class="notice-text"><ng-content /></span>
      @if (closable()) {
        <button
          type="button"
          class="btn btn-sm btn-link notice-close"
          aria-label="关闭提示"
          (click)="closed.emit()"
        >
          关闭
        </button>
      }
    </div>
  `,
  styles: `
    .notice {
      display: flex;
      align-items: flex-start;
      gap: 0.4rem;
      padding: 0.45rem 0.6rem;
      margin-bottom: 0.5rem;
      border: 1px solid;
      border-left-width: 4px;
      border-radius: var(--clr-base-border-radius-s, 0.15rem);
      font-size: 0.6rem;
      line-height: 1.6;
    }
    .notice-label {
      flex: none;
      font-weight: 700;
    }
    .notice-text {
      flex: 1;
      min-width: 0;
      word-break: break-all;
    }
    .notice-close {
      flex: none;
      margin: 0;
      padding: 0;
    }
    .notice--info {
      color: var(--interactive-primary-bg);
      background: var(--cds-alias-object-interaction-background-highlight, #e8f2fc);
      border-color: var(--interactive-primary-bg);
    }
    .notice--success {
      color: var(--semantic-success-text);
      background: var(--semantic-success-bg);
      border-color: var(--semantic-success-border);
    }
    .notice--warning {
      color: var(--semantic-warning-text);
      background: var(--semantic-warning-bg);
      border-color: var(--semantic-warning-border);
    }
    .notice--error {
      color: var(--semantic-error-text);
      background: var(--semantic-error-bg);
      border-color: var(--semantic-error-border);
    }
    .notice--info .notice-close,
    .notice--success .notice-close,
    .notice--warning .notice-close,
    .notice--error .notice-close {
      color: inherit;
    }
  `,
})
export class NoticeComponent {
  readonly type = input<NoticeType>("info");
  readonly closable = input(true);
  readonly closed = output<void>();

  readonly label = computed(() => LABELS[this.type()]);
  readonly assertive = computed(
    () => this.type() === "error" || this.type() === "warning",
  );
}
