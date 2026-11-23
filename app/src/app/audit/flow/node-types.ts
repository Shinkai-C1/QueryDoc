/** 节点类型级联菜单数据（画布右键「增加节点」与 PDF 框选「添加节点」共用）。 */
export interface NodeTypeCascade {
  type: string;
  label: string;
  subs: { value: string; label: string }[];
}

/** LOG 无子类型（与或非由名称表达）；ACT 无子类型。 */
export const NODE_TYPE_CASCADE: NodeTypeCascade[] = [
  { type: "ACT", label: "ACT 控制活动", subs: [] },
  {
    type: "EVT",
    label: "EVT 事件",
    subs: [
      { value: "time", label: "时间" },
      { value: "message", label: "消息" },
    ],
  },
  {
    type: "ROL",
    label: "ROL 角色",
    subs: [
      { value: "person", label: "个人" },
      { value: "dept", label: "部门" },
      { value: "it", label: "IT 系统" },
    ],
  },
  {
    type: "DAT",
    label: "DAT 数据资料",
    subs: [
      { value: "electronic", label: "电子" },
      { value: "paper", label: "纸质" },
      { value: "record", label: "记录" },
    ],
  },
  { type: "LOG", label: "LOG 逻辑", subs: [] },
];
