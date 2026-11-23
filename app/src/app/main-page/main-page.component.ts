import {
  CUSTOM_ELEMENTS_SCHEMA,
  ChangeDetectionStrategy,
  Component,
  OnInit,
  inject,
} from "@angular/core";
import { ActivatedRoute, Router } from "@angular/router";
import { ClarityIcons } from "@cds/core/icon";
import { arrowIcon } from "@cds/core/icon/shapes/arrow.js";
import "@cds/core/icon/register.js";
import { AppStateService } from "../app-state.service";
import { ExtractTableComponent } from "../extract-table.component";
import { NoticeComponent } from "../notice.component";
import { PdfViewerComponent } from "../pdf-viewer.component";
import { StageTimelineComponent } from "../stage-timeline/stage-timeline.component";

ClarityIcons.addIcons(arrowIcon);

/**
 * 任务工作区：顶部返回 + 任务名；五阶段时间线（上传/解析/关键词/提取/导出）；
 * 提取结果表格 + 右栏 PDF 预览（可收起）。文件进度明细在时间线右上「查看具体进度」弹窗。
 */
@Component({
  selector: "app-main-page",
  changeDetection: ChangeDetectionStrategy.OnPush,
  schemas: [CUSTOM_ELEMENTS_SCHEMA],
  imports: [
    ExtractTableComponent,
    PdfViewerComponent,
    NoticeComponent,
    StageTimelineComponent,
  ],
  templateUrl: "./main-page.component.html",
  styleUrl: "./main-page.component.css",
})
export class MainPageComponent implements OnInit {
  protected readonly state = inject(AppStateService);
  private readonly route = inject(ActivatedRoute);
  private readonly router = inject(Router);

  // ---- 路由接入：/task/:id ----
  ngOnInit(): void {
    this.route.paramMap.subscribe(async (params) => {
      const id = Number(params.get("id"));
      if (!Number.isFinite(id) || id <= 0) {
        void this.router.navigate(["/"]);
        return;
      }
      if (this.state.projects().length === 0) {
        await this.state.refreshProjects();
      }
      if (this.state.projects().some((p) => p.id === id)) {
        if (this.state.selectedProjectId() !== id) {
          await this.state.selectProject(id);
        }
      } else {
        this.state.errorMessage.set("任务不存在或已删除");
        void this.router.navigate(["/"]);
      }
    });
  }

  /** 返回项目列表页。 */
  goBack(): void {
    void this.router.navigate(["/"]);
  }
}
