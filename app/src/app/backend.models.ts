/**
 * 与 app/src-tauri Rust 侧 serde 输出一一对应的类型定义。
 * 字段保持 snake_case（后端 JSON 原样输出）。
 */

export interface Project {
  id: number;
  name: string;
  /** 所属项目组（自由文本分组，新建任务时选取或新建）。 */
  group_name: string;
  /** 任务类型：'parse' 解析提取 | 'audit' 内控测试（旧库数据为 'parse'）。 */
  task_type: string;
  created_at: string;
  /** 最近修改时间（后端跨子表现算：上传/关键词/结果编辑/导出最新时间）。 */
  updated_at: string;
  base_path: string;
  project_dir: string;
  status: string; // idle/parsing/extracting/error
}

/** 列表页状态列：每项目的解析/提取进度计数。 */
export interface ProjectProgress {
  project_id: number;
  status: string; // idle/parsing/extracting/error
  files_total: number;
  parse_done: number;
  parse_error: number;
  extract_done: number;
  extract_error: number;
}

export interface UploadBatchInfo {
  id: number;
  project_id: number;
  folder_name: string;
  uploaded_at: string;
}

export interface SourceFileInfo {
  id: number;
  batch_id: number;
  project_id: number;
  folder_name: string;
  original_name: string;
  stored_name: string;
  relative_path: string;
  file_size: number;
  status: string; // pending/parsing/done/error（解析阶段）
  /** 预估提取输入 Token 数（进度面板「预估Token」列；null=未估算/上次估算失败） */
  estimated_tokens: number | null;
  created_at: string;
}

export interface UploadResult {
  batch: UploadBatchInfo;
  files: SourceFileInfo[];
}

export interface FileQueueStatus {
  file_id: number;
  file_name: string;
  upload_status: string; // completed
  upload_position: number | null;
  upload_files_ahead: number;
  parse_status: string; // pending/running/done/error
  parse_position: number | null;
  parse_files_ahead: number;
  extract_status: string;
  extract_position: number | null;
  extract_files_ahead: number;
  parse_error?: string; // 仅 parse_status=error 时由后端给出
  extract_error?: string; // 提取失败行的错误信息
}

export interface StageCounts {
  pending: number;
  running: number;
  done: number;
  error: number;
}

export interface QueueSummary {
  upload: StageCounts;
  parse: StageCounts;
  extract: StageCounts;
}

export interface ProjectQueueStatus {
  project_id: number;
  project_status: string;
  files: FileQueueStatus[];
  summary: QueueSummary;
}

/** `queue://progress` 事件载荷（可选字段在后端被 skip 时不出现）。 */
export interface ProgressPayload {
  project_id: number;
  kind: string; // parse/extract/export
  file_id?: number;
  status: string; // pending/running/done/error
  done: number;
  total: number;
  current_file?: string;
}

/** `queue://log` 事件载荷（任务日志行，同写任务目录 `任务日志.log`）。 */
export interface LogPayload {
  project_id: number;
  line: string;
}

// ---------------------------------------------------------------------------
// 提取（Phase 4/5）
// ---------------------------------------------------------------------------

export interface ExtractConfigInfo {
  id: number;
  project_id: number;
  column_index: number;
  keyword: string;
  /** 关键词定义（选填；空串=未设置）。 */
  definition: string;
  /** 信息类型（选填：""=未设置 | "text" | "number" | "date"）。 */
  info_type: string;
  created_at: string;
}

export interface ExtractConfigInput {
  column_index: number;
  keyword: string;
  definition?: string;
  info_type?: string;
}

/** cell_data 单元格：坐标字段在未提取到（value=""）时为 null/空数组。 */
export interface CellData {
  value: string;
  quote: string;
  reasoning?: string;
  page_idx: number | null;
  bbox: number[] | null; // 0–1000 归一化 [x0,y0,x1,y1]
  fragments: number[][]; // 跨行分行矩形数组（归一化）
  char_spans?: { c: string; bbox: number[] }[];
  confidence: number;
  /** 存疑标记：quote 未命中原文但照常返回值（召回优先），前端红色「疑」提示。 */
  suspect?: boolean;
}

export interface ExtractResultInfo {
  id: number;
  project_id: number;
  source_file_id: number;
  source_file_name: string;
  /** key 为字符串列号（"2"=C 列…） */
  cell_data: Record<string, CellData>;
  is_edited: boolean;
  created_at: string;
  updated_at: string;
}

/** 当前选中的单元格（PDF 联动高亮的数据源）。 */
export interface SelectedCell {
  /** `${resultId}:${colIndex}`，用于表格选中态标识 */
  key: string;
  resultId: number;
  sourceFileId: number;
  colIndex: number;
  value: string;
  page_idx: number | null;
  fragments: number[][];
}

// ---------------------------------------------------------------------------
// 导出（Phase 6）
// ---------------------------------------------------------------------------

export interface ExportTaskInfo {
  id: number;
  project_id: number;
  kind: string; // table/full
  status: string; // pending/running/done/error
  output_path: string;
  created_at: string;
}

// ---------------------------------------------------------------------------
// 设置（按 provider 分行存取）
// ---------------------------------------------------------------------------

/** 单个 provider 的完整连接配置。 */
export interface ProviderSettings {
  base_url: string;
  api_key: string;
  model: string;
  timeout: number;
}

/** 设置视图：当前 provider + 两个 provider 各自的完整配置。 */
export interface SettingsView {
  active_provider: string; // ollama/kimi
  ollama: ProviderSettings;
  kimi: ProviderSettings;
}

/** save_settings 入参（保存即设为当前 provider）。 */
export interface SettingsInput {
  provider: string;
  base_url: string;
  api_key: string;
  model: string;
  timeout: number;
}

export interface LlmTestResult {
  ok: boolean;
  message: string;
  models: string[];
}

// ---------------------------------------------------------------------------
// Token 预估（进度面板「预估Token」列）
// ---------------------------------------------------------------------------

export interface TokenEstimate {
  file_id: number;
  /** 估算的提取输入 Token 数；失败为 null（error 带原因） */
  tokens: number | null;
  error: string | null;
}

// ---------------------------------------------------------------------------
// 内控测试（迁移 v10；与 Rust serde 结构 1:1，字段 snake_case）
// ---------------------------------------------------------------------------

/** 项目任务类型：'parse' 解析提取 | 'audit' 内控测试。 */
export type TaskType = 'parse' | 'audit';

/** 制度文件（导入的 pdf，字节直接入库；rel_path 为相对导入文件夹的路径，用于树层级）。 */
export interface IcDocument {
  id: number;
  project_id: number;
  name: string;
  rel_path: string;
  /** 遗留字段（v10 磁盘落盘路径）；v11 起入库恒为空串。 */
  file_path: string;
  /** pdf 字节数。 */
  size: number;
  created_at: string;
}

/** ACT 扩展属性（存于 ic_nodes.props json）。 */
export interface IcActProps {
  /** 控制性质：人工/自动化/含人工的自动化 */
  control_nature?: string;
  /** 预防/检查 */
  preventive_or_detective?: string;
  /** 控制频率：年/季/月/周/日/多次/不定期 */
  frequency?: string;
  /** 是否关键控制：Y/N */
  is_key?: string;
  /** 是否应对特别风险：Y/N */
  is_special_risk?: string;
}

/**
 * 流程节点。rect 为 0–1000 归一化矩形坐标 json 字符串（[x0,y0,x1,y1]），
 * props 为扩展属性 json 字符串（ACT 见 IcActProps）；两者均为后端 TEXT 原样输出。
 */
export interface IcNode {
  id: number;
  project_id: number;
  /** 全局唯一编号：前缀（ACT/EVT/ROL/DAT/LOG）+ 序号，删除即废弃不重用 */
  code: string;
  node_type: string; // ACT|EVT|ROL|DAT|LOG
  /** EVT: time|message; ROL: person|dept|it; DAT: electronic|paper|record; LOG: and|or|not */
  subtype: string | null;
  name: string;
  content: string;
  doc_id: number | null;
  page: number | null;
  rect: string | null;
  canvas_x: number;
  canvas_y: number;
  /** 画布尺寸；0 = 内容自适应（用户拖边框后持久化）。 */
  canvas_w: number;
  canvas_h: number;
  group_id: number | null;
  props: string;
  created_at: string;
}

/** 流程连线；edge_kind: flow(上下流)|role(角色)|data(数据)。 */
export interface IcEdge {
  id: number;
  project_id: number;
  from_node: number;
  to_node: number;
  edge_kind: string;
}

/** 环境组（画布包围矩形 json）。env_type: it(IT环境)|manual(人工环境)。 */
export interface IcGroup {
  id: number;
  project_id: number;
  env_type: string;
  rect: string;
}

/** 制度索引：源 pdf 矩形 → 目标 pdf 文件。 */
export interface IcDocLink {
  id: number;
  project_id: number;
  src_doc_id: number;
  page: number;
  rect: string;
  target_doc_id: number;
}

/** 人员名册行（导入自 xlsx，全字符串，空单元格为 null）。 */
export interface IcRosterRow {
  id: number;
  project_id: number;
  name: string | null;
  dept: string | null;
  position: string | null;
  duty: string | null;
  superior: string | null;
  subordinate: string | null;
  remark: string | null;
}

/** 组织结构行。 */
export interface IcOrgRow {
  id: number;
  project_id: number;
  dept: string | null;
  duty: string | null;
  superior: string | null;
  subordinate: string | null;
  remark: string | null;
}

/** 区域 OCR 结果：识别文本 + 各文字行矩形（0–1000 归一化坐标）。 */
export interface OcrRegionResult {
  text: string;
  rects: number[][];
}

/** 创建流程节点入参（ic_create_node；嵌套对象字段与 Rust serde 一致 snake_case）。 */
export interface IcNodeInput {
  node_type: string; // ACT|EVT|ROL|DAT|LOG
  subtype?: string | null;
  name?: string;
  content?: string;
  doc_id?: number | null;
  page?: number | null;
  rect?: string | null;
  canvas_x?: number;
  canvas_y?: number;
  group_id?: number | null;
  props?: string;
}

/**
 * 更新流程节点补丁（ic_update_node）：字段缺省 = 不动该列；
 * group_id 传 null 表示清除所属组。
 */
export interface IcNodePatch {
  name?: string;
  content?: string;
  canvas_x?: number;
  canvas_y?: number;
  canvas_w?: number;
  canvas_h?: number;
  group_id?: number | null;
  props?: string;
}
