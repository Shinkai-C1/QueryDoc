import { ChangeDetectionStrategy, Component, computed, input } from "@angular/core";

export type StageKind = "project" | "upload" | "parse" | "extract";

const LABELS: Record<StageKind, Record<string, string>> = {
  project: { idle: "空闲", parsing: "解析中", extracting: "提取中", error: "出错" },
  upload: { completed: "已上传", pending: "待上传", running: "上传中", done: "已上传", error: "上传失败" },
  parse: { pending: "待解析", running: "解析中", done: "已完成", error: "解析失败" },
  extract: { pending: "待提取", running: "提取中", done: "已完成", error: "提取失败" },
};

/** 三阶段状态徽标：纯文本 + 语义色 token；pending 带位次时显示「第 N 位」。 */
@Component({
  selector: "app-stage-badge",
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <span class="qd-badge" [class]="'qd-badge ' + kindClass()">
      {{ label() }}
      @if (position() !== null && status() === 'pending') {
        （第 {{ position() }} 位）
      }
    </span>
  `,
  styles: `
    .qd-badge {
      display: inline-block;
      padding: 0 0.4rem;
      border: 1px solid;
      border-radius: 0.5rem;
      font-size: 0.55rem;
      line-height: 0.9rem;
      white-space: nowrap;
    }
    .qd-badge--success {
      color: var(--semantic-success-text);
      background: var(--semantic-success-bg);
      border-color: var(--semantic-success-border);
    }
    .qd-badge--error {
      color: var(--semantic-error-text);
      background: var(--semantic-error-bg);
      border-color: var(--semantic-error-border);
    }
    .qd-badge--info {
      color: var(--interactive-primary-bg);
      background: transparent;
      border-color: var(--interactive-primary-bg);
    }
    .qd-badge--neutral {
      color: var(--cds-alias-typography-color-400, #565656);
      background: var(--cds-alias-object-container-background-shade, #f4f4f4);
      border-color: var(--cds-alias-object-border-color, #cccccc);
    }
  `,
})
export class StageBadgeComponent {
  readonly stage = input.required<StageKind>();
  readonly status = input.required<string>();
  readonly position = input<number | null>(null);

  readonly label = computed(
    () => LABELS[this.stage()][this.status()] ?? this.status(),
  );

  readonly kindClass = computed(() => {
    switch (this.status()) {
      case "completed":
      case "done":
        return "qd-badge--success";
      case "error":
        return "qd-badge--error";
      case "running":
      case "parsing":
      case "extracting":
        return "qd-badge--info";
      default:
        return "qd-badge--neutral";
    }
  });
}
