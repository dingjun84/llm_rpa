import { useEffect, useState } from "react";

import { workflowRequirements } from "../api";
import {
  NAV_TARGET_LABELS,
  SCENARIO_LABELS,
  WORKFLOW_LABELS,
  type DemoScenario,
  type NavTarget,
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
   * 配置草稿。
   *
   * ★ 这一组**本该只用运行参数**（`runChoice`），拿 `draft` 只有两个用途：
   * ① 演练场景 —— 它是这一组里唯一一个**配置**字段（要保存才生效）；
   * ② 标定结果的签名（`area_marks`），用来判断"还缺哪几块标定"要不要重问。
   * **别往这里加其它 `draft` 字段** —— 要保存的东西归 `RuntimePanel`。
   */
  draft: RuntimeConfig;
  onPatch: (patch: Partial<RuntimeConfig>) => void;
  /**
   * 本次任务走哪一条路。`null` = 配置还没读回来，三个选择器先禁用。
   *
   * ★★ 它是**运行参数**，不在 `draft` 里：改它既不置 `dirty`、也不用保存。
   */
  runChoice: RunChoice | null;
  onPatchRunChoice: (patch: Partial<RunChoice>) => void;
  /** **这一次真正会跑的模式**，由 `App` 算好传下来（别读 `draft.mode`）。 */
  activeMode: RuntimeMode;
}

const SCENARIOS = Object.keys(SCENARIO_LABELS) as DemoScenario[];

const MODE_LABELS: Record<RuntimeMode, string> = {
  dry_run: "演练模式",
  live: "真实模式",
};

/**
 * 工作流下拉的顺序：**按"离能发消息还有多远"排**，不是按字母序。
 *
 * 第一个是默认值，也是操作者当下要的那条路；「只做导航」放在最后——
 * 它是**排查工具**，不是日常要跑的任务。
 */
const WORKFLOWS: Workflow[] = ["search_contact", "scroll_list_contact", "navigate_only"];

/**
 * 「这一次要跑什么」那一组：模式 / 演练场景 / 工作流 / 要点哪个图标 / 还缺哪块标定。
 *
 * ## 为什么单独成文件
 *
 * 这几项的**归属**与下面那些表单不一样：模式与工作流是**运行参数**（绑
 * `runChoice`，选完直接点「开始任务」就生效），而窗口、超时、开关那些是**配置**
 * （要保存）。混在同一个组件里时，很容易顺手把新加的项绑错一边——而绑错的症状
 * 正是「界面选了 A、跑的是 B」，那是最难查的一类。
 *
 * 边界一句话：**凡是要"保存配置"的留在 `RuntimePanel`，只管"这一次怎么跑"的搬到这里。**
 */
export function RunChoiceFields({
  info,
  draft,
  onPatch,
  runChoice,
  onPatchRunChoice,
  activeMode,
}: Props) {
  /** 当前工作流需要哪些标定区域（**后端**算好下发，这里只渲染）。 */
  const [requirement, setRequirement] = useState<WorkflowRequirement | null>(null);

  // 改草稿的字段。本地适配器——只为让下面的调用点短一点，判据不在这里。
  const update = <K extends keyof RuntimeConfig>(key: K, value: RuntimeConfig[K]) => {
    onPatch({ [key]: value } as Partial<RuntimeConfig>);
  };

  /**
   * 标定结果的一个**稳定签名**，用来当刷新依赖。
   *
   * 直接依赖 `draft` 会让每次按键都去问一遍后端；而只有「工作流改了」
   * 或「标定结果变了」才会改变答案。用 JSON 串而不是对象本身：
   * `area_marks` 每次 `onPatch` 都会换一个新对象，按引用比会一直不相等。
   */
  const marksSignature = JSON.stringify(draft.area_marks ?? {});

  /**
   * 本次**真正要跑**的那条工作流。
   *
   * ★ 取自 `runChoice`（运行参数），**不是** `draft.workflow`——后者只是界面
   * 打开时的初始值，跟这次要跑什么没有关系。下面"还缺哪几块标定"必须按
   * 前者算，否则会出现「界面说齐了、点开始却被拒」。
   */
  const activeWorkflow = runChoice?.workflow ?? null;

  useEffect(() => {
    if (!activeWorkflow) {
      // 运行参数还没定（配置没读回来）。这时不显示提示，而不是按草稿猜一条。
      setRequirement(null);
      return;
    }
    let cancelled = false;
    void (async () => {
      try {
        const all = await workflowRequirements(draft);
        if (cancelled) return;
        setRequirement(all.find((item) => item.workflow === activeWorkflow) ?? null);
      } catch {
        // 问不到就**不显示**这条提示。
        //
        // 它只是"点开始之前先提醒一句"，真正的判据在装配期（后端会拒绝并列出
        // 缺哪几块）。为了它弹一个错误，反而会盖住页面上真正的问题。
        if (!cancelled) setRequirement(null);
      }
    })();
    return () => {
      cancelled = true;
    };
    // 只在这两项变了才重问：答案只取决于它们。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeWorkflow, marksSignature]);

  const missingMarks = requirement?.required.filter((item) => !item.marked) ?? [];

  return (
    <>
      {/* ★★ 它绑的是 `runChoice`（**运行参数**），不是 `draft`（配置草稿）：
          选完直接点「开始任务」就按它跑，不需要保存。
          配置里那个同名字段只剩一个作用——给这里**设初值**，
          所以下面的提示不能再说"要保存才生效"（那是改之前的行为）。
          保存配置时会把它一并记成下次的默认值。 */}
      <label className="field">
        <span className="field-label">模式</span>
        <select
          value={runChoice?.mode ?? ""}
          disabled={!runChoice}
          onChange={(event) => onPatchRunChoice({ mode: event.target.value as RuntimeMode })}
        >
          {runChoice === null && <option value="">（正在读取运行配置…）</option>}
          <option value="dry_run">{MODE_LABELS.dry_run}</option>
          <option value="live" disabled={!info.is_windows}>
            {MODE_LABELS.live}
            {info.is_windows ? "" : "（仅 Windows）"}
          </option>
        </select>
        <span className="field-hint">
          ★ 这是<strong>本次任务</strong>的运行参数：选完直接点「开始任务」就按它跑，
          <strong>不用先保存</strong>。（点「保存配置」会把它记成下次启动的默认值。）
          <br />
          真实模式会操作本机客户端窗口，而且<strong>必须有已记录的窗口尺寸</strong>，
          否则任务在装配期就会被拒——切过去之前先确认「记录窗口尺寸」那一栏有值。
        </span>
      </label>

      {activeMode === "dry_run" && (
        <label className="field">
          <span className="field-label">演练场景</span>
          <select
            value={draft.demo_scenario}
            onChange={(event) => update("demo_scenario", event.target.value as DemoScenario)}
          >
            {SCENARIOS.map((scenario) => (
              <option key={scenario} value={scenario}>
                {SCENARIO_LABELS[scenario]}
              </option>
            ))}
          </select>
          <span className="field-hint">
            选一个失败场景，可以直观看到"不确定就不发送"的收敛结果。
          </span>
        </label>
      )}

      {/* ── 工作流 ──────────────────────────────────────────────────
          它和「运行模式」是两件事：模式决定"怎么执行"（替身还是真桌面），
          工作流决定"做哪一件事"。所以放在模式下面、标定之前——
          它决定了后面哪些区域是必须的。

          ★★ 它绑的是 `runChoice`（**运行参数**），不是 `draft`（配置草稿）：
          选完直接点「开始任务」就按它跑，不需要保存、也不写进配置文件。
          这就是"选了 A 却跑了 B"那个 bug 的修复点——以前它绑在配置上，
          而命令层读的是**已保存**的那份。 */}
      <label className="field">
        <span className="field-label">工作流</span>
        <select
          value={runChoice?.workflow ?? ""}
          disabled={!runChoice}
          onChange={(event) =>
            onPatchRunChoice({ workflow: event.target.value as Workflow })
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
          三条路<strong>看的是不同的界面</strong>，而它们的失败现象一模一样
          （都是「找不到联系人」），所以必须显式选，不能自动判断——
          选错了，现场就分不清是"搜索没生效"还是"列表里真的没有这个人"。
        </span>
      </label>

      {activeWorkflow === "navigate_only" && (
        <label className="field">
          <span className="field-label">要点哪一个图标</span>
          <select
            value={runChoice?.nav_target ?? ""}
            disabled={!runChoice}
            onChange={(event) =>
              onPatchRunChoice({ nav_target: event.target.value as NavTarget })
            }
          >
            {runChoice === null && <option value="">（正在读取运行配置…）</option>}
            {(Object.keys(NAV_TARGET_LABELS) as NavTarget[]).map((target) => (
              <option key={target} value={target}>
                {NAV_TARGET_LABELS[target]}
              </option>
            ))}
          </select>
          <span className="field-hint">
            两个图标各有各的模板组，在「图标库」页分别勾。这一项
            <strong>同样只对本次任务有效</strong>。
            这一条工作流<strong>不查找任何人</strong>，只用来看图标匹配准不准——
            混在完整流程里时，点错图标的症状会表现为「找不到联系人」，
            排查方向会一路偏向 OCR。
          </span>
        </label>
      )}

      {/* 这条工作流还缺哪几块标定。判据在后端，这里只渲染它算出来的结果——
          前端自己列一张表的话，两边不一致时的表现是
          「界面说齐了、点开始却被拒」，而人只会去怀疑标定本身。 */}
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
