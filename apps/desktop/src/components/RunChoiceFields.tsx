import {
  WORKFLOW_LABELS,
  type RunChoice,
  type RuntimeConfig,
  type RuntimeInfo,
  type RuntimeMode,
  type Workflow,
  type WorkflowRequirement,
} from "../types";

interface Props {
  info: RuntimeInfo;
  /**
   * 配置草稿。本组件现在几乎不读它（YOLO 工作流不再依赖演练场景 / 导航图标选择）。
   * 保留 prop 是为了与 `RuntimePanel` 的调用签名兼容。
   */
  draft: RuntimeConfig;
  onPatch: (patch: Partial<RuntimeConfig>) => void;
  /** 本次任务走哪一条路。`null` = 配置还没读回来。 */
  runChoice: RunChoice | null;
  onPatchRunChoice: (patch: Partial<RunChoice>) => void;
  /** 始终为 live（演练已从操作界面移除）。 */
  activeMode: RuntimeMode;
  requirement: WorkflowRequirement | null;
}

const WORKFLOWS: Workflow[] = ["chat_list_send", "contacts_search_send", "forward_to_contact"];

/**
 * 「这一次要跑什么」：工作流选择 + 标定缺口提示。
 *
 * 任务始终真实模式；模式选择器与演练场景已移除。
 */
export function RunChoiceFields({
  info,
  runChoice,
  onPatchRunChoice,
  requirement,
}: Props) {
  const missingMarks = requirement?.required.filter((item) => !item.marked) ?? [];

  return (
    <>
      <p className="notice notice-live">
        任务始终以<strong>真实模式</strong>运行（演练 / dry_run 已从操作界面移除）。
        鼠标点击走幽灵盒（Windows）；UI 定位走远程 YOLO。
        {!info.live_supported && " 当前平台不一定支持真实桌面端口。"}
      </p>

      <label className="field">
        <span className="field-label">工作流</span>
        <select
          value={runChoice?.workflow ?? ""}
          disabled={!runChoice}
          onChange={(event) =>
            onPatchRunChoice({
              workflow: event.target.value as Workflow,
              mode: "live",
            })
          }
        >
          {runChoice === null && <option value="">（正在读取运行配置…）</option>}
          {WORKFLOWS.map((workflow) => (
            <option key={workflow} value={workflow}>
              {WORKFLOW_LABELS[workflow]}
            </option>
          ))}
        </select>
        <span className="field-hint">
          ★ 这是<strong>本次任务</strong>的运行参数：选完直接点「开始任务」就按它跑，
          <strong>不用保存、也不会写进配置文件</strong>。
          <br />
          两条路看的是不同的界面（会话列表 vs 通讯录搜索），失败现象相似，必须显式选。
        </span>
      </label>

      {requirement && requirement.required.length > 0 && (
        <p className={missingMarks.length > 0 ? "notice notice-warn" : "notice"}>
          「{requirement.label}」需要标定：{" "}
          {requirement.required.map((item, index) => (
            <span key={item.key}>
              {index > 0 && "、"}
              <strong className={item.marked ? undefined : "missing-mark"}>
                {item.label}
                {item.marked ? "" : "（还没标）"}
              </strong>
            </span>
          ))}
          。到「界面标定」页把没标的框出来——
          {missingMarks.length > 0 ? (
            <>
              现在点「开始任务」会在<strong>装配期</strong>被直接拒绝
              （不会在任务列表里留下记录）。
            </>
          ) : (
            <>这几块都已经标好了。</>
          )}
        </p>
      )}
    </>
  );
}
