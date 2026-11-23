import { Routes } from "@angular/router";
import { MainPageComponent } from "./main-page/main-page.component";
import { ProjectListComponent } from "./project-list/project-list.component";
import { AuditPageComponent } from "./audit/audit-page.component";

export const routes: Routes = [
  { path: "", component: ProjectListComponent },
  { path: "task/:id", component: MainPageComponent },
  // 内控测试工作区（新增；既有两条路由不动）
  { path: "audit/:id", component: AuditPageComponent },
  { path: "**", redirectTo: "" },
];
