import {
  ChangeDetectionStrategy,
  Component,
  input,
  output,
} from "@angular/core";
import { NODE_TYPE_CASCADE } from "../flow/node-types";

/**
 * 框选完成后的 mini 选项窗口（类右键菜单样式，纯文本按钮）：
 * 添加节点（级联选类型/子类型，悬停向右展开）/ 制度索引 / 取消。
 * 定位于框选矩形附近（坐标相对 PDF 画布容器）。
 */
@Component({
  selector: "app-mini-menu",
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <!-- mousedown 必须拦截：菜单挂在覆盖层内，冒泡到覆盖层会先清空菜单再接收不到 click -->
    <div
      class="mini-menu"
      [style.left.px]="x()"
      [style.top.px]="y()"
      [class.menu-left]="openLeft()"
      role="menu"
      aria-label="框选操作菜单"
      (mousedown)="$event.stopPropagation()"
    >
      <div class="menu-item has-sub" role="menuitem" aria-haspopup="true" tabindex="0">
        添加节点 <span class="sub-arrow">▸</span>
        <div class="submenu" [class.sub-left]="openLeft()">
          @for (t of cascade; track t.type) {
            @if (t.subs.length === 0) {
              <div class="menu-item" role="menuitem" tabindex="0" (click)="pick(t.type, null)" (keydown.enter)="pick(t.type, null)">
                {{ t.label }}
              </div>
            } @else {
              <div class="menu-item has-sub" role="menuitem" aria-haspopup="true" tabindex="0">
                {{ t.label }} <span class="sub-arrow">▸</span>
                <div class="submenu" [class.sub-left]="openLeft()">
                  @for (s of t.subs; track s.value) {
                    <div class="menu-item" role="menuitem" tabindex="0" (click)="pick(t.type, s.value)" (keydown.enter)="pick(t.type, s.value)">
                      {{ s.label }}
                    </div>
                  }
                </div>
              </div>
            }
          }
        </div>
      </div>
      <button type="button" class="menu-item" role="menuitem" (click)="addLink.emit()">
        制度索引
      </button>
      <button type="button" class="menu-item menu-cancel" role="menuitem" (click)="dismiss.emit()">
        取消
      </button>
    </div>
  `,
  styles: `
    .mini-menu {
      position: absolute;
      z-index: 30;
      display: flex;
      flex-direction: column;
      min-width: 6.5rem;
      background: var(--cds-alias-object-container-background, #fff);
      border: 1px solid var(--cds-alias-object-border-color, #d9d9d9);
      border-radius: 4px;
      box-shadow: 0 2px 8px rgba(0, 0, 0, 0.18);
      padding: 0.15rem 0;
    }
    .menu-item {
      position: relative;
      border: 0;
      background: none;
      text-align: left;
      padding: 0.25rem 0.6rem;
      font-size: 0.6rem;
      cursor: pointer;
      color: var(--cds-alias-typography-color-500, #333);
      white-space: nowrap;
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 0.6rem;
    }
    .menu-item:hover { background: var(--cds-alias-object-container-background-shade, #f0f0f0); }
    .menu-cancel {
      border-top: 1px solid var(--cds-alias-object-border-color, #eee);
      color: var(--cds-alias-typography-color-400, #888);
    }
    .has-sub .submenu {
      display: none;
      position: absolute;
      left: 100%;
      top: -0.2rem;
      min-width: 6.5rem;
      background: var(--cds-alias-object-container-background, #fff);
      border: 1px solid var(--cds-alias-object-border-color, #d9d9d9);
      border-radius: 4px;
      box-shadow: 0 2px 8px rgba(0, 0, 0, 0.18);
      padding: 0.15rem 0;
      z-index: 31;
    }
    .has-sub:hover > .submenu { display: block; }
    .sub-arrow { font-size: 0.55rem; color: var(--cds-alias-typography-color-400, #888); }
    .submenu.sub-left { left: auto; right: 100%; }
  `,
})
export class MiniMenuComponent {
  /** 相对画布容器的 CSS 坐标。 */
  readonly x = input.required<number>();
  readonly y = input.required<number>();
  /** 宿主（覆盖层）宽度，用于判定级联展开方向（右侧不足 260px 向左展开）。 */
  readonly hostWidth = input.required<number>();

  /** 级联是否向左展开。 */
  openLeft(): boolean {
    return this.hostWidth() - this.x() < 260;
  }

  readonly cascade = NODE_TYPE_CASCADE;

  /** 添加节点：级联选定类型/子类型后立即携带载荷触发。 */
  readonly addNode = output<{ nodeType: string; subtype: string | null }>();
  readonly addLink = output<void>();
  readonly dismiss = output<void>();

  pick(nodeType: string, subtype: string | null): void {
    this.addNode.emit({ nodeType, subtype });
  }
}
