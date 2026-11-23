import { Injectable } from "@angular/core";
import { invoke } from "@tauri-apps/api/core";
import {
  ExportTaskInfo,
  ExtractConfigInfo,
  ExtractConfigInput,
  ExtractResultInfo,
  IcDocLink,
  IcDocument,
  IcEdge,
  IcGroup,
  IcNode,
  IcNodeInput,
  IcNodePatch,
  IcOrgRow,
  IcRosterRow,
  LlmTestResult,
  OcrRegionResult,
  Project,
  ProjectProgress,
  ProjectQueueStatus,
  SettingsInput,
  SettingsView,
  SourceFileInfo,
  TokenEstimate,
  UploadResult,
} from "./backend.models";

/**
 * Tauri command 薄封装：仅做类型化与参数透传。
 * 注意：camelCase ↔ snake_case 自动转换只作用于顶层参数名；
 * 嵌套对象（如 configs 数组元素）的字段必须与 Rust serde 命名一致（snake_case）。
 */
@Injectable({ providedIn: "root" })
export class BackendService {
  listProjects(): Promise<Project[]> {
    return invoke<Project[]>("list_projects");
  }

  listProjectProgress(): Promise<ProjectProgress[]> {
    return invoke<ProjectProgress[]>("list_project_progress");
  }

  createProject(
    basePath: string,
    name: string,
    groupName: string,
    taskType?: string,
  ): Promise<Project> {
    // taskType 缺省时后端按 'parse' 处理（兼容旧调用）
    return invoke<Project>("create_project", { basePath, name, groupName, taskType });
  }

  deleteProject(id: number): Promise<void> {
    return invoke<void>("delete_project", { id });
  }

  resetProject(projectId: number): Promise<void> {
    return invoke<void>("reset_project", { projectId });
  }

  reparseFiles(projectId: number, fileIds: number[]): Promise<void> {
    return invoke<void>("reparse_files", { projectId, fileIds });
  }

  reextractFiles(projectId: number, fileIds: number[]): Promise<void> {
    return invoke<void>("reextract_files", { projectId, fileIds });
  }

  deleteFiles(projectId: number, fileIds: number[]): Promise<void> {
    return invoke<void>("delete_files", { projectId, fileIds });
  }

  uploadFiles(projectId: number, paths: string[]): Promise<UploadResult> {
    return invoke<UploadResult>("upload_files", { projectId, paths });
  }

  listFiles(projectId: number): Promise<SourceFileInfo[]> {
    return invoke<SourceFileInfo[]>("list_files", { projectId });
  }

  enqueueParse(projectId: number): Promise<void> {
    return invoke<void>("enqueue_parse", { projectId });
  }

  queueStatus(projectId: number): Promise<ProjectQueueStatus> {
    return invoke<ProjectQueueStatus>("queue_status", { projectId });
  }

  // ---- 提取（Phase 4/5）----

  getExtractConfigs(projectId: number): Promise<ExtractConfigInfo[]> {
    return invoke<ExtractConfigInfo[]>("get_extract_configs", { projectId });
  }

  saveExtractConfigs(
    projectId: number,
    configs: ExtractConfigInput[],
  ): Promise<ExtractConfigInfo[]> {
    return invoke<ExtractConfigInfo[]>("save_extract_configs", {
      projectId,
      configs,
    });
  }

  enqueueExtract(projectId: number): Promise<void> {
    return invoke<void>("enqueue_extract", { projectId });
  }

  getExtractResults(projectId: number): Promise<ExtractResultInfo[]> {
    return invoke<ExtractResultInfo[]>("get_extract_results", { projectId });
  }

  updateExtractCell(
    id: number,
    colIndex: number,
    value: string,
  ): Promise<ExtractResultInfo> {
    return invoke<ExtractResultInfo>("update_extract_cell", {
      id,
      colIndex,
      value,
    });
  }

  readSourcePdfPath(sourceFileId: number): Promise<string> {
    return invoke<string>("read_source_pdf_path", { sourceFileId });
  }

  readFileBytes(path: string): Promise<number[]> {
    return invoke<number[]>("read_file_bytes", { path });
  }

  // ---- 导出（Phase 6）----

  getExportTasks(projectId: number): Promise<ExportTaskInfo[]> {
    return invoke<ExportTaskInfo[]>("get_export_tasks", { projectId });
  }

  enqueueExport(projectId: number, kind: string): Promise<void> {
    return invoke<void>("enqueue_export", { projectId, kind });
  }

  // ---- 设置（按 provider 分行存取）----

  getSettings(): Promise<SettingsView> {
    return invoke<SettingsView>("get_settings");
  }

  saveSettings(input: SettingsInput): Promise<SettingsView> {
    return invoke<SettingsView>("save_settings", { input });
  }

  /** 以表单草稿配置获取模型清单（不读库、不落库）。 */
  fetchModels(
    provider: string,
    baseUrl: string,
    apiKey: string,
  ): Promise<LlmTestResult> {
    return invoke<LlmTestResult>("fetch_models", { provider, baseUrl, apiKey });
  }

  /** 全部停止：中断当前解析/提取并清空队列中该项目任务。 */
  stopProject(projectId: number): Promise<void> {
    return invoke<void>("stop_project", { projectId });
  }

  /** 逐文件估算提取输入 Token（仅 KIMI-API）。 */
  estimateTokens(projectId: number): Promise<TokenEstimate[]> {
    return invoke<TokenEstimate[]>("estimate_tokens", { projectId });
  }

  // ---- 任务日志 ----

  readProjectLog(projectId: number): Promise<string> {
    return invoke<string>("read_project_log", { projectId });
  }

  // ---- 内控测试（ic_*，全部为新增命令）----

  icImportDocs(
    projectId: number,
    srcDir: string,
  ): Promise<{ imported: number; updated: number }> {
    return invoke("ic_import_docs", { projectId, srcDir });
  }

  icListDocs(projectId: number): Promise<IcDocument[]> {
    return invoke<IcDocument[]>("ic_list_docs", { projectId });
  }

  /** 删除制度文件（流程图节点与索引保留，跳转失效）。 */
  icDeleteDoc(projectId: number, docId: number): Promise<void> {
    return invoke<void>("ic_delete_doc", { projectId, docId });
  }

  /** 读取制度文件字节（入库 blob）。 */
  icReadDocBytes(projectId: number, docId: number): Promise<number[]> {
    return invoke<number[]>("ic_read_doc_bytes", { projectId, docId });
  }

  /** 生成导入模版 xlsx（roster/org）到指定路径。 */
  icWriteTemplate(kind: string, path: string): Promise<void> {
    return invoke<void>("ic_write_template", { kind, path });
  }

  /** 导入人员名册 xlsx；返回导入行数。 */
  icImportRoster(projectId: number, xlsxPath: string): Promise<number> {
    return invoke<number>("ic_import_roster", { projectId, xlsxPath });
  }

  /** 导入组织结构 xlsx；返回导入行数。 */
  icImportOrg(projectId: number, xlsxPath: string): Promise<number> {
    return invoke<number>("ic_import_org", { projectId, xlsxPath });
  }

  icListRoster(projectId: number): Promise<IcRosterRow[]> {
    return invoke<IcRosterRow[]>("ic_list_roster", { projectId });
  }

  icListOrg(projectId: number): Promise<IcOrgRow[]> {
    return invoke<IcOrgRow[]>("ic_list_org", { projectId });
  }

  /** 新增制度索引（源 pdf 矩形 → 目标 pdf）；返回行 id。 */
  icAddDocLink(
    projectId: number,
    srcDocId: number,
    page: number,
    rect: string,
    targetDocId: number,
  ): Promise<number> {
    return invoke<number>("ic_add_doc_link", {
      projectId,
      srcDocId,
      page,
      rect,
      targetDocId,
    });
  }

  icRemoveDocLink(projectId: number, linkId: number): Promise<void> {
    return invoke<void>("ic_remove_doc_link", { projectId, linkId });
  }

  icListDocLinks(projectId: number): Promise<IcDocLink[]> {
    return invoke<IcDocLink[]>("ic_list_doc_links", { projectId });
  }

  /** 区域 OCR（即用即载；rect 为 0–1000 归一化 [x0,y0,x1,y1]）。 */
  ocrRegion(
    projectId: number,
    docId: number,
    page: number,
    rect: number[],
  ): Promise<OcrRegionResult> {
    return invoke<OcrRegionResult>("ocr_region", { projectId, docId, page, rect });
  }

  /** 创建流程节点；返回完整行（含分配的 code）。 */
  icCreateNode(projectId: number, input: IcNodeInput): Promise<IcNode> {
    return invoke<IcNode>("ic_create_node", { projectId, input });
  }

  icUpdateNode(
    projectId: number,
    nodeId: number,
    patch: IcNodePatch,
  ): Promise<IcNode> {
    return invoke<IcNode>("ic_update_node", { projectId, nodeId, patch });
  }

  icDeleteNode(projectId: number, nodeId: number): Promise<void> {
    return invoke<void>("ic_delete_node", { projectId, nodeId });
  }

  icListNodes(projectId: number): Promise<IcNode[]> {
    return invoke<IcNode[]>("ic_list_nodes", { projectId });
  }

  /** 新增连线；edgeKind: flow|role|data（类型校验在后端）。 */
  icAddEdge(
    projectId: number,
    fromNode: number,
    toNode: number,
    edgeKind: string,
  ): Promise<number> {
    return invoke<number>("ic_add_edge", { projectId, fromNode, toNode, edgeKind });
  }

  icRemoveEdge(projectId: number, edgeId: number): Promise<void> {
    return invoke<void>("ic_remove_edge", { projectId, edgeId });
  }

  icListEdges(projectId: number): Promise<IcEdge[]> {
    return invoke<IcEdge[]>("ic_list_edges", { projectId });
  }

  /** 创建环境组（it/manual）；返回组 id。 */
  icCreateGroup(
    projectId: number,
    envType: string,
    rect: string,
    nodeIds: number[],
  ): Promise<number> {
    return invoke<number>("ic_create_group", { projectId, envType, rect, nodeIds });
  }

  icDeleteGroup(projectId: number, groupId: number): Promise<void> {
    return invoke<void>("ic_delete_group", { projectId, groupId });
  }

  icUpdateGroupRect(projectId: number, groupId: number, rect: string): Promise<void> {
    return invoke<void>("ic_update_group_rect", { projectId, groupId, rect });
  }

  icListGroups(projectId: number): Promise<IcGroup[]> {
    return invoke<IcGroup[]>("ic_list_groups", { projectId });
  }
}
