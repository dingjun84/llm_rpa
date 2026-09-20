import { useEffect, useState } from "react";

import { listIcons, workflowRequirements } from "../api";
import { normalizeName } from "../iconNames";
import {
  SCENARIO_LABELS,
  WORKFLOW_LABELS,
  type DemoScenario,
  type IconEntry,
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

  /**
   * 图标库里的图标（**一级目录名 + 它底下的全部图**）。
   *
   * ## 为什么这一组要读图标库
   *
   * 「要点哪一个图标」的答案**只在图标库里**：`data/icons/` 下的目录名
   * （发现 / 收藏夹 / 聊天历史 / 通讯录…）。早先这里是一份写死的两项清单
   * （「联系人图标」「聊天历史图标」），而操作者手里有四五个图标——
   * 于是**选不出来也对不上**：想测「收藏夹」，下拉里根本没有它。
   *
   * 读的是**已保存的图标库目录**，与「图标库」页的列表同一个命令。
   * 切页签会重新挂载本组件，所以刚在「图标库」页存下的图标，切回来就能看到。
   *
   * `null` = 还没读回来（此时下拉显示"正在读取"），`[]` = 读到了但是空的。
   * 两者必须分开：把"还没读到"显示成"一个图标都没有"，会让人跑去图标库页白看一趟。
   */
  const [icons, setIcons] = useState<IconEntry[] | null>(null);
  const [iconError, setIconError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const list = await listIcons();
        if (!cancelled) setIcons(list);
      } catch (err) {
        // 读不到就**当作空**并把原因说出来，**不要**退回一份写死的清单：
        // 那会让人以为图标库里真有那些图标，然后按着它去选。
        if (!cancelled) {
          setIcons([]);
          setIconError(String(err));
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  /** 本次要点的那一个图标（图标库里的目录名）。空串 = 还没选。 */
  const chosenIcon = runChoice?.nav_target ?? "";
  /**
   * 选中的那个名字**还在不在**图标库里（被删了、或者图标库目录换了）。
   *
   * 不在的话原样把它显示出来并标一句，**绝不自动改选**：悄悄换成别的图标，
   * 等于把"配置坏了"变成"点错了地方"——后者要难查得多。
   */
  const chosenMissing =
    chosenIcon !== "" &&
    icons !== null &&
    !icons.some((entry) => normalizeName(entry.name) === normalizeName(chosenIcon));
  const chosen = icons?.find((entry) => normalizeName(entry.name) === normalizeName(chosenIcon));

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
        <>
          <label className="field">
            <span className="field-label">要点哪一个图标</span>
            <select
              value={chosenIcon}
              disabled={!runChoice}
              onChange={(event) => onPatchRunChoice({ nav_target: event.target.value })}
            >
              {runChoice === null && <option value="">（正在读取运行配置…）</option>}
              {runChoice !== null && icons === null && (
                <option value="">（正在读取图标库…）</option>
              )}
              {runChoice !== null && icons !== null && (
                <>
                  {chosenIcon === "" && <option value="">（还没选）</option>}
                  {/* 被删掉 / 换过图标库目录的那个名字：原样列出来并说明，
                      而不是把它悄悄抹掉——抹掉之后下拉会自动落到第一项，
                      看起来像"已经选好了"。 */}
                  {chosenMissing && (
                    <option value={chosenIcon}>{chosenIcon}（图标库里已没有这个目录）</option>
                  )}
                  {icons.length === 0 && !chosenMissing && (
                    <option value="">（图标库里还没有图标）</option>
                  )}
                  {icons.map((entry) => (
                    <option key={entry.name} value={entry.name} disabled={!entry.usable}>
                      {entry.name}（{entry.variants.length} 张图）
                      {entry.usable ? "" : "　⚠ 有图读不出来"}
                    </option>
                  ))}
                </>
              )}
            </select>
            <span className="field-hint">
              下拉里列的是<strong>图标库里的图标</strong>——<code>data/icons/</code> 下的一级目录，
              就是「图标库」页列表里的那几行。选中一个，程序就拿<strong>那个目录下的全部图</strong>
              一起参与匹配、取最高分：它们本来就是同一个图标的选中 / 未选中 / 带气泡等
              不同状态，不该被当成不同图标比高低。
              <br />
              ⚠️ 列表读的是<strong>已保存的</strong>图标库目录（与「图标库」页一致）——
              刚存下新图标、或刚改了「图标库目录」，要切到「图标库」页看一眼、
              必要时点「保存配置」，回到这里才会出现。
              <br />
              这一项<strong>同样只对本次任务有效</strong>，不用保存。
              这一条工作流<strong>不查找任何人</strong>，只用来看图标匹配准不准——
              混在完整流程里时，点错图标的症状会表现为「找不到联系人」，
              排查方向会一路偏向 OCR。
            </span>
          </label>

          {icons !== null && icons.length === 0 && (
            <p className="notice notice-warn">
              图标库里<strong>一个图标都没有</strong>
              {iconError ? `（读目录时出错：${iconError}）` : ""}。
              先到「图标库」页截一张窗口画面、框住那个图标、起个名字存下来，
              这里才有得选。
            </p>
          )}
          {chosenMissing && (
            <p className="notice notice-warn">
              选中的「{chosenIcon}」<strong>已经不在图标库里了</strong>
              ——它被删掉了，或者图标库目录换了。这样点「开始任务」会在装配期被直接拒绝。
              到「图标库」页确认一下，或者在上面重新选一个。
            </p>
          )}
          {chosen !== undefined && !chosen.usable && (
            <p className="notice notice-warn">
              「{chosen.name}」底下<strong>有图读不出来</strong>（不是 PNG、或者文件损坏）。
              模板少一张的后果是"图标换个状态就认不出来"，所以这一项照样会在装配期被拒绝。
              到「图标库」页把坏的那张删掉、重新截一张。
            </p>
          )}
        </>
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
