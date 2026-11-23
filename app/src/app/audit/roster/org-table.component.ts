import {
  ChangeDetectionStrategy,
  Component,
  inject,
} from "@angular/core";
import { AuditStateService } from "../audit-state.service";

/**
 * 组织结构页签：浏览已导入的组织数据（部门/职责/上级/下属/备注）。
 * 数据经制度文件树上方「组织」按钮选择 xlsx 导入（calamine 解析，整体替换）。
 */
@Component({
  selector: "app-org-table",
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <div class="table-wrap">
      @if (state.org().length === 0) {
        <div class="placeholder muted">
          尚未导入组织结构：点击右侧制度文件栏上方「组织」按钮选择 xlsx 导入
        </div>
      } @else {
        <table class="table">
          <thead>
            <tr>
              <th class="left">部门组织</th>
              <th class="left">部门职责描述</th>
              <th class="left">上级</th>
              <th class="left">下级</th>
              <th class="left">备注</th>
            </tr>
          </thead>
          <tbody>
            @for (r of state.org(); track r.id) {
              <tr>
                <td class="left">{{ r.dept }}</td>
                <td class="left">{{ r.duty }}</td>
                <td class="left">{{ r.superior }}</td>
                <td class="left">{{ r.subordinate }}</td>
                <td class="left">{{ r.remark }}</td>
              </tr>
            }
          </tbody>
        </table>
      }
    </div>
  `,
  styles: `
    .table-wrap { height: 100%; overflow: auto; padding: 0.3rem; }
    .placeholder { padding: 2rem 1rem; text-align: center; font-size: 0.65rem; }
    .muted { color: var(--cds-alias-typography-color-400, #888); }
  `,
})
export class OrgTableComponent {
  protected readonly state = inject(AuditStateService);
}
