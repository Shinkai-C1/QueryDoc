import {
  ChangeDetectionStrategy,
  Component,
  OnDestroy,
  OnInit,
  inject,
} from "@angular/core";
import { ActivatedRoute, Router } from "@angular/router";
import { ClrTabsModule } from "@clr/angular";
import { AppStateService } from "../app-state.service";
import { NoticeComponent } from "../notice.component";
import { AuditStateService, AuditTab } from "./audit-state.service";
import { FlowCanvasComponent } from "./flow/flow-canvas.component";
import { FlowDetailComponent } from "./detail/flow-detail.component";
import { RosterTableComponent } from "./roster/roster-table.component";
import { OrgTableComponent } from "./roster/org-table.component";
import { DocRelationComponent } from "./relation/doc-relation.component";
import { DocTreeComponent } from "./docs/doc-tree.component";
import { AuditPdfComponent } from "./pdf/audit-pdf.component";

/**
 * 内控测试主界面：三栏布局（流程图 50% / PDF 预览 40% / 制度文件树 10%），
 * PDF 与树均可向右收缩为窄条；流程图区域上方五个页签
 *（流程图/流程明细/人员名册/组织结构/制度关系）。
 * 状态全部走 AuditStateService（与 AppStateService 隔离，原则 6）。
 */
@Component({
  selector: "app-audit-page",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    ClrTabsModule,
    NoticeComponent,
    FlowCanvasComponent,
    FlowDetailComponent,
    RosterTableComponent,
    OrgTableComponent,
    DocRelationComponent,
    DocTreeComponent,
    AuditPdfComponent,
  ],
  templateUrl: "./audit-page.component.html",
  styleUrl: "./audit-page.component.css",
})
export class AuditPageComponent implements OnInit, OnDestroy {
  protected readonly state = inject(AuditStateService);
  private readonly appState = inject(AppStateService);
  private readonly route = inject(ActivatedRoute);
  private readonly router = inject(Router);

  ngOnInit(): void {
    this.route.paramMap.subscribe(async (params) => {
      const id = Number(params.get("id"));
      if (!Number.isFinite(id) || id <= 0) {
        void this.router.navigate(["/"]);
        return;
      }
      if (this.appState.projects().length === 0) {
        await this.appState.refreshProjects();
      }
      const p = this.appState.projects().find((x) => x.id === id) ?? null;
      if (!p) {
        this.appState.errorMessage.set("任务不存在或已删除");
        void this.router.navigate(["/"]);
        return;
      }
      if (p.task_type !== "audit") {
        // 解析提取任务误入内控界面：回其自有工作区
        void this.router.navigate(["/task", id]);
        return;
      }
      this.state.load(p);
    });
  }

  ngOnDestroy(): void {
    this.state.reset();
  }

  setTab(tab: AuditTab): void {
    this.state.activeTab.set(tab);
  }

  // ---- 左右分栏拖拽（flow 区 25%–75%） ----
  private dividerDrag: { startX: number; startPct: number; hostWidth: number } | null = null;

  onDividerDown(e: MouseEvent, host: HTMLElement): void {
    e.preventDefault();
    this.dividerDrag = {
      startX: e.clientX,
      startPct: this.state.leftPct(),
      hostWidth: host.getBoundingClientRect().width,
    };
    const move = (ev: MouseEvent) => {
      const d = this.dividerDrag;
      if (!d) return;
      const pct = d.startPct + ((ev.clientX - d.startX) / d.hostWidth) * 100;
      this.state.leftPct.set(Math.min(75, Math.max(25, pct)));
    };
    const up = () => {
      this.dividerDrag = null;
      document.removeEventListener("mousemove", move);
      document.removeEventListener("mouseup", up);
    };
    document.addEventListener("mousemove", move);
    document.addEventListener("mouseup", up);
  }

  goBack(): void {
    void this.router.navigate(["/"]);
  }
}
