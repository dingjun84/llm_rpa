import { useEffect, useState } from "react";

import { launchClient, recordWindowGeometry, workflowRequirements } from "../api";
import {
  NAV_TARGET_LABELS,
  SCENARIO_LABELS,
  WORKFLOW_LABELS,
  type DemoScenario,
  type NavTarget,
  type PickedWindow,
  type RuntimeConfig,
  type RuntimeInfo,
  type RuntimeMode,
  type Workflow,
  type WorkflowRequirement,
} from "../types";
import { WindowPicker } from "./WindowPicker";

interface Props {
  info: RuntimeInfo;
  /**
   * 配置**草稿**。
   *
   * 它由 `App` 持有，而不是这一块自己 `useState`：配置里有一组字段
   * （导航图标）是在「图标库」页里改的，草稿放在子组件里就没法共享——
   * 而两页各自持有一份草稿的话，保存时会有一份被另一份覆盖掉。
   */
  draft: RuntimeConfig;
  /** 改草稿。不落盘——要用户点「保存配置」。 */
  onPatch: (patch: Partial<RuntimeConfig>) => void;
  onSave: () => void;
  /** 草稿与已保存的配置是否不一致。 */
  dirty: boolean;
  /** 草稿是否满足保存条件（区域标定合法）。 */
  canSave: boolean;
  busy: boolean;
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
 * 比例输入（0–1）的取值。
 *
 * 输入框清空或内容不是数字时**保留原值**，不要退回 0：
 * 退回 0 会把「正在删掉重输」这个中间状态变成「落点跑到最左边」，
 * 而 0 恰好是个合法值，看起来就像配置已经生效了。
 */
const ratioFromInput = (raw: string, fallback: number) => {
  if (raw.trim() === "") return fallback;
  const value = Number(raw);
  if (!Number.isFinite(value)) return fallback;
  return Math.min(1, Math.max(0, value));
};

export function RuntimePanel({ info, draft, onPatch, onSave, dirty, canSave, busy }: Props) {
  /** 「启动客户端」的结果提示（成功或失败）。 */
  const [launchNotice, setLaunchNotice] = useState<string | null>(null);
  const [launching, setLaunching] = useState(false);
  /** 「记录窗口尺寸」的结果提示（成功或失败）。 */
  const [measureNotice, setMeasureNotice] = useState<string | null>(null);
  const [measuring, setMeasuring] = useState(false);
  /** 当前工作流需要哪些标定区域（**后端**算好下发，这里只渲染）。 */
  const [requirement, setRequirement] = useState<WorkflowRequirement | null>(null);

  /**
   * 标定结果的一个**稳定签名**，用来当刷新依赖。
   *
   * 直接依赖 `draft` 会让每次按键都去问一遍后端；而只有「工作流改了」
   * 或「标定结果变了」才会改变答案。用 JSON 串而不是对象本身：
   * `area_marks` 每次 `onPatch` 都会换一个新对象，按引用比会一直不相等。
   */
  const marksSignature = JSON.stringify(draft.area_marks ?? {});

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const all = await workflowRequirements(draft);
        if (cancelled) return;
        setRequirement(all.find((item) => item.workflow === draft.workflow) ?? null);
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
  }, [draft.workflow, marksSignature]);

  const missingMarks = requirement?.required.filter((item) => !item.marked) ?? [];

  const update = <K extends keyof RuntimeConfig>(key: K, value: RuntimeConfig[K]) => {
    onPatch({ [key]: value } as Partial<RuntimeConfig>);
  };

  /**
   * 指认窗口后的回填。
   *
   * 类名一定回填（那是匹配的主要依据）；可执行文件路径**只在读到时才覆盖**——
   * 有些窗口（权限受限的系统进程）读不出路径，直接写 null 会把已有配置抹掉。
   */
  const applyPickedWindow = (picked: PickedWindow) => {
    onPatch({
      window_class: picked.class_name,
      wecom_exe: picked.exe_path ?? draft.wecom_exe,
    });
  };

  const startClient = async () => {
    setLaunching(true);
    setLaunchNotice(null);
    try {
      await launchClient(draft.wecom_exe, draft.wecom_exe_sha256);
      setLaunchNotice(
        "已发出启动请求。请确认客户端窗口已经打开并完成登录，再回来开始任务——" +
          "程序不会自己去判断登录有没有完成。",
      );
    } catch (err) {
      setLaunchNotice(`启动失败：${String(err)}`);
    } finally {
      setLaunching(false);
    }
  };

  const measureWindow = async () => {
    setMeasuring(true);
    setMeasureNotice(null);
    try {
      const geometry = await recordWindowGeometry(draft.window_class, draft.wecom_exe);
      onPatch({ calibrated_window: geometry });
      setMeasureNotice(
        `已记录 ${geometry.width}×${geometry.height} @(${geometry.x}, ${geometry.y})，` +
          `缩放 ${geometry.scale_factor}。还要点最下面的「保存配置」才会生效。`,
      );
    } catch (err) {
      setMeasureNotice(`记录失败：${String(err)}`);
    } finally {
      setMeasuring(false);
    }
  };

  // 演练模式不碰真实窗口，标定值用不上；真实模式则必须保证四个区域都合法，
  // 否则后端 `RelativeRegion::validate` 会在执行时才报错，白白浪费一次任务。
  // 这个判断由 `App` 做（它同时还要给「图标库」页用），这里只读结果。

  // 界面上显示的是 `draft`（草稿），而 `start_task` 读的是**已保存的** `state.config`
  // （见 `lib.rs::start_task`）。两者不一致时，光看界面会以为改动已经生效——
  // 必须明说，而且要说清**现在真正生效的是什么**。
  //
  // ⚠️ 这里**不逐项列"哪些字段改了"**：那是一份会漂移的清单，漏掉一项就正好漏掉
  // 最要命的那一项。原来只有「模式」有提示，工作流就没有——于是"我明明把工作流
  // 切到列表式了"变成了三次都在跑搜索式，而日志里当时连工作流都没记。
  // 所以判据是**草稿与已保存不一致**（`dirty`），提示里报出已保存那份的
  // 模式与工作流（这两个决定任务会怎么跑），值一律取自后端下发的 `info.config`。
  const effectiveMode = info.config.mode;

  return (
    <section className="panel">
      <h2>运行模式</h2>

      {/* 这句描述的是**当前生效**的模式，也就是点「开始任务」时真正会用的那份。
          刻意用后端下发的 `info.notice`，不在前端再拼一遍：同一句提示维护两处必然漂移，
          而且后端那份才跟"实际会怎么跑"绑在一起。 */}
      <p className={effectiveMode === "dry_run" ? "notice" : "notice notice-live"}>
        {info.notice}
      </p>

      {dirty && (
        <p className="notice notice-warn">
          界面上的配置有改动，<strong>还没保存</strong>——现在点「开始任务」用的仍然是
          已保存的那一份：模式「{MODE_LABELS[effectiveMode]}」、工作流「
          {WORKFLOW_LABELS[info.config.workflow]}」。
          要让改动生效，点最下面的「保存配置」。
        </p>
      )}

      {/* 启动时那次一次性数据搬迁的结果（旧版把配置与图标库放在 AppData 里）。
          有值就一定要显示：搬成功了用户会疑惑"配置怎么突然有值了"，
          搬失败了会以为"数据丢了"——两种都得有答案。 */}
      {info.migration_note && <p className="notice notice-warn">{info.migration_note}</p>}

      <label className="field">
        <span className="field-label">模式</span>
        <select
          value={draft.mode}
          onChange={(event) => update("mode", event.target.value as RuntimeMode)}
        >
          <option value="dry_run">演练模式</option>
          <option value="live" disabled={!info.is_windows}>
            真实模式{info.is_windows ? "" : "（仅 Windows）"}
          </option>
        </select>
        <span className="field-hint">
          任务用的是<strong>已保存</strong>的配置，不是界面上的草稿——
          改完要点最下面的「保存配置」才生效。
        </span>
      </label>

      {draft.mode === "dry_run" && (
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
          它决定了后面哪些区域是必须的。 */}
      <label className="field">
        <span className="field-label">工作流</span>
        <select
          value={draft.workflow}
          onChange={(event) => update("workflow", event.target.value as Workflow)}
        >
          {WORKFLOWS.map((workflow) => (
            <option key={workflow} value={workflow}>
              {WORKFLOW_LABELS[workflow]}
            </option>
          ))}
        </select>
        <span className="field-hint">
          三条路<strong>看的是不同的界面</strong>，而它们的失败现象一模一样
          （都是「找不到联系人」），所以必须显式选，不能自动判断——
          选错了，现场就分不清是"搜索没生效"还是"列表里真的没有这个人"。
        </span>
      </label>

      {draft.workflow === "navigate_only" && (
        <label className="field">
          <span className="field-label">要点哪一个图标</span>
          <select
            value={draft.nav_target}
            onChange={(event) => update("nav_target", event.target.value as NavTarget)}
          >
            {(Object.keys(NAV_TARGET_LABELS) as NavTarget[]).map((target) => (
              <option key={target} value={target}>
                {NAV_TARGET_LABELS[target]}
              </option>
            ))}
          </select>
          <span className="field-hint">
            两个图标各有各的模板组，在「图标库」页分别勾。
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

      {/* 下面这一组是**配置**，刻意不放在「模式」分支里。
          指认窗口与区域标定都是只读操作（不点击、不输入、不发送），
          而且实际顺序本来就是"先把窗口和四个区域标定好，再决定用哪种模式跑"。
          把它们藏在真实模式后面，等于让人没法在跑真实模式之前做准备。 */}
      <section className="calibration">
        <div className="calibration-head">
          <h3>目标窗口</h3>
        </div>
        <p className="field-hint">
          标定跟运行模式无关：这里只读地看一眼目标窗口，不会点击、不会输入、不会发送，
          所以演练模式下也可以随时做。
        </p>

        <WindowPicker disabled={busy} onApply={applyPickedWindow} />

        <label className="field">
          <span className="field-label">目标程序可执行文件路径</span>
          <input
            type="text"
            value={draft.wecom_exe ?? ""}
            placeholder="点「指认窗口」可自动填入"
            onChange={(event) => update("wecom_exe", event.target.value || null)}
          />
          <span className="field-hint">
            两个用途：一是「启动客户端」按钮按它拉起程序；二是<strong>校验窗口归属</strong>——
            定位时会要求窗口属于这个程序。Qt 系程序（微信 4.x 就是）所有顶层窗口
            共用同一个类名，少了这条就可能选中登录窗。
          </span>
        </label>

        <label className="field">
          <span className="field-label">可执行文件 SHA-256（可选）</span>
          <input
            type="text"
            value={draft.wecom_exe_sha256 ?? ""}
            placeholder="填写后，哈希不一致将拒绝启动"
            onChange={(event) => update("wecom_exe_sha256", event.target.value || null)}
          />
        </label>

        <div className="field">
          <span className="field-label">客户端</span>
          <div className="calibration-actions">
            <button
              type="button"
              disabled={busy || launching || !(draft.wecom_exe ?? "").trim()}
              onClick={startClient}
            >
              {launching ? "启动中…" : "启动客户端"}
            </button>
          </div>
          <span className="field-hint">
            微信 / 企业微信<strong>由你启动并登录</strong>，任务不会自己去拉起程序：
            重复启动会弹出登录窗，而登录窗和主窗口类名相同，定位会选错；
            扫码、验证码这些事程序也插不上手。
          </span>
          {launchNotice && <p className="notice">{launchNotice}</p>}
        </div>

        <label className="field">
          <span className="field-label">窗口类名</span>
          <input
            type="text"
            value={draft.window_class}
            onChange={(event) => update("window_class", event.target.value)}
          />
          <span className="field-hint">
            定位目标窗口按类名匹配；填了上面的可执行文件路径时，会同时校验窗口属于该程序。
            点「指认窗口」会自动填入。「界面标定」页截图用的也是这个值，
            改完不用保存就能直接切过去截图看效果。
          </span>
        </label>

        <div className="field">
          <span className="field-label">标定窗口尺寸</span>
          <div className="calibration-actions">
            <button
              type="button"
              disabled={busy || measuring || !draft.window_class.trim()}
              onClick={measureWindow}
            >
              {measuring ? "读取中…" : "记录窗口尺寸"}
            </button>
          </div>
          {draft.calibrated_window ? (
            <span className="field-hint">
              已记录 <strong>{draft.calibrated_window.width}×{draft.calibrated_window.height}</strong>
              {" "}@({draft.calibrated_window.x}, {draft.calibrated_window.y})，缩放{" "}
              {draft.calibrated_window.scale_factor}。任务只在<strong>这个尺寸</strong>下运行：
              尺寸对不上会先把窗口调回这个尺寸，调不动才转人工；位置不校验，窗口挪到哪儿都行。
            </span>
          ) : (
            <span className="field-hint">
              还没有记录。真实模式必须先点这个按钮——任务只在标定时的窗口尺寸下运行：
              尺寸对不上会先把窗口调回来，客户端的最小尺寸不允许时才会转人工。
            </span>
          )}
          {measureNotice && <p className="notice">{measureNotice}</p>}
        </div>

        <label className="field">
          <span className="field-label">本地 OCR 程序路径</span>
          <input
            type="text"
            value={draft.ocr_command ?? ""}
            placeholder="留空表示未配置，任务会停在人工处理"
            onChange={(event) => update("ocr_command", event.target.value || null)}
          />
          <span className="field-hint">
            OCR 程序从标准输入读取 PNG，向标准输出写 JSON 数组；不允许联网。
            仓库里自带 <code>tools/winocr</code>（用 Windows 内置离线 OCR），
            构建后填 <code>&lt;仓库&gt;/target/debug/winocr.exe</code> 即可。
          </span>
        </label>

      </section>

      {/* 四个区域（列表区 / 对话标题 / 对话内容 / 输入框）只在「界面标定」页标：
          那里是"截一张图、在图上框一块"，每一块按它所在的界面分场景列出、带编号和提示。
          这里原来还有一份"填个数字、看一眼框"的旧版面板，改的是**同一份配置**，
          留着只会让人不知道该信哪一边，已经删掉。 */}

      {/* 导航图标那一组（开关 / 模板 / 阈值 / 搜索区）在「图标库」页。
          它的操作方式是"截一张图、在图上框一个图标、起名字"，
          和界面标定那种"框一块区域"是两回事，混在一起两边都别扭。 */}

      <div className="field">
        <span className="field-label">超时（毫秒 / 秒）</span>
        <div className="calibration-inputs">
          <label>
            <span>单次 OCR 上限</span>
            <input
              type="number"
              min={500}
              step={500}
              value={draft.ocr_timeout_ms}
              onChange={(event) =>
                update("ocr_timeout_ms", Math.max(500, Number(event.target.value) || 500))
              }
            />
          </label>
          <label>
            <span>单步下限</span>
            <input
              type="number"
              min={1}
              max={600}
              value={draft.step_timeout_secs}
              onChange={(event) =>
                update("step_timeout_secs", Math.max(1, Number(event.target.value) || 1))
              }
            />
          </label>
        </div>
        <span className="field-hint">
          机器慢或窗口大时把「单次 OCR 上限」调大即可——单步超时会自动跟着放宽
          （取<code>单步下限</code>与<code>OCR 上限 + 5 秒</code>中的较大者），
          不会出现「调大了 OCR 超时却被单步超时提前掐断」的情况。
        </span>
      </div>

      <div className="field">
        <span className="field-label">滚动查找联系人</span>
        <div className="calibration-inputs">
          <label>
            <span>最多滚动次数</span>
            <input
              type="number"
              min={1}
              max={200}
              value={draft.max_scroll_attempts}
              onChange={(event) =>
                update("max_scroll_attempts", Math.max(1, Number(event.target.value) || 1))
              }
            />
          </label>
          <label>
            <span>每次格数</span>
            <input
              type="number"
              min={1}
              max={20}
              value={draft.scroll_notches_per_step}
              onChange={(event) =>
                update(
                  "scroll_notches_per_step",
                  Math.min(20, Math.max(1, Number(event.target.value) || 1)),
                )
              }
            />
          </label>
          <label>
            <span>完整扫描轮数</span>
            <input
              type="number"
              min={1}
              max={20}
              value={draft.max_search_sweeps}
              onChange={(event) =>
                update("max_search_sweeps", Math.min(20, Math.max(1, Number(event.target.value) || 1)))
              }
            />
          </label>
          <label>
            <span>滚动落点 横向</span>
            <input
              type="number"
              min={0}
              max={1}
              step={0.01}
              value={draft.scroll_anchor.x}
              onChange={(event) =>
                update("scroll_anchor", {
                  ...draft.scroll_anchor,
                  x: ratioFromInput(event.target.value, draft.scroll_anchor.x),
                })
              }
            />
          </label>
          <label>
            <span>滚动落点 纵向</span>
            <input
              type="number"
              min={0}
              max={1}
              step={0.01}
              value={draft.scroll_anchor.y}
              onChange={(event) =>
                update("scroll_anchor", {
                  ...draft.scroll_anchor,
                  y: ratioFromInput(event.target.value, draft.scroll_anchor.y),
                })
              }
            />
          </label>
          <label>
            <span>滚动停稳等待</span>
            <input
              type="number"
              min={0}
              step={50}
              value={draft.scroll_settle_ms}
              onChange={(event) =>
                update("scroll_settle_ms", Math.max(0, Math.round(Number(event.target.value) || 0)))
              }
            />
          </label>
        </div>
        <span className="field-hint">
          <strong>只对「列表扫描式」那条工作流生效</strong>——搜索式看的是顶部
          联想下拉，不滚这个列表。下面这些旋钮就是那条路要用的：
          目标不在当前可见范围时，会在联系人候选区向下滚动继续找。
          列表按「最近有消息」排序，扫描期间到达的新消息会把目标顶到最上面，
          而那一屏早被翻过去了——所以扫完一轮会<strong>回到顶部再扫一轮</strong>。
          完整扫描轮数就是那个上限；配成 1 就退回「只往下扫一遍」的老行为。
          滚完上限、或画面已经不再变化（滚到底了），任务会转人工处理。
        </span>
        <span className="field-hint">
          <strong>滚动落点</strong>是鼠标停在哪里滚（相对联系人候选区的比例），
          默认 <code>0.62 / 0.5</code> 即<strong>上下居中、左右偏右一点</strong>。
          不用正中心是因为候选区左边界把左侧图标栏和头像列一起圈了进来，
          正中心恰好压在头像列上；偏右落到名字那一列更稳。
          两个值都必须在 0–1 之间，填到范围外任务会直接报错而不是凑合着滚。
        </span>
        <span className="field-hint">
          <strong>滚动停稳等待</strong>（毫秒，<code>0</code> = 不等）是滚完之后
          「等画面停下来」的<strong>上限</strong>。客户端的列表滚动是带缓动的动画，
          滚完立刻截图会截到中间帧——文字是糊的、行是错位的，识别出来自然是乱的；
          而且「滚了一下画面没动 ⇒ 到底了」这个判断也会把「还没画完」当成
          「到底了」，于是提前收工、漏掉整段列表。这里是<strong>有界轮询</strong>：
          画面连续两帧一致就立刻继续，只有动画真的还在跑时才会等到上限，
          所以正常情况下开销只是多截一帧。
        </span>
      </div>

      <label className="field field-check">
        <input
          type="checkbox"
          checked={draft.liveness_check}
          onChange={(event) => update("liveness_check", event.target.checked)}
        />
        <span>
          <span className="field-label">卡死检测</span>
          <span className="field-hint">
            打开后，点击/粘贴/发送之前会先确认客户端还在响应（系统级判断），
            并且在「向下滚不动、向上也滚不动」时判定为客户端卡死、转人工处理。
            关掉它等于允许程序继续往一个可能已经卡死的窗口里输入——
            只建议在现场排查误判时临时关掉。
          </span>
        </span>
      </label>

      <label className="field field-check">
        <input
          type="checkbox"
          checked={draft.log_ocr_candidates}
          onChange={(event) => update("log_ocr_candidates", event.target.checked)}
        />
        <span>
          <span className="field-label">记录识别结果</span>
          <span className="field-hint">
            打开后，联系人列表每滚一步都会把这一帧 <strong>OCR 实际读到的文字</strong>
            写进任务日志（<code>task-*.log</code>）与任务详情。
            排查「明明在列表里却找不到」时，只有截图指纹是看不出问题的——
            分不清是 OCR 把名字读错了，还是名字根本不在这屏；前者要调识别，
            后者要改范围，处置完全相反。姓名本来就已经记在日志开头
            （「目标联系人」一行），这里没有新增数据类别；而且只写日志与界面，
            不进审计库。
          </span>
        </span>
      </label>

      <label className="field field-check">
        <input
          type="checkbox"
          checked={draft.relaxed_name_match}
          onChange={(event) => update("relaxed_name_match", event.target.checked)}
        />
        <span>
          <span className="field-label">
            宽松姓名匹配（⚠️ 临时措施）
          </span>
          <span className="field-hint">
            打开后：先按<strong>逐字精确</strong>匹配；精确匹配不到时，退化为
            <strong>候选文字里包含目标名就算命中</strong>。
            它违反架构约定的「逐字精确匹配」，把「找不到人」变成「可能找错人」，
            所以<strong>有真实发送需求时必须关掉</strong>。
            <br />
            存在的理由：OCR 会把列表左侧的头像/未读红点按行并进姓名里
            （实测把「丁俊」读成「0 丁俊」），而精确匹配对这种噪声零容忍，
            于是整条链路卡在「找联系人」这一步，后面的点击、标题核验、
            填入输入框都验证不到。
            <br />
            已放宽：命中多个时取最短的那个（姓名行比「发送者：消息预览」短）；
            最短的并列时仍然按歧义拒绝。
            未放宽：空目标名、低置信度、多个逐字相同、候选太近，一律照旧拒绝。
            <br />
            只对真实模式生效。待优化项记在 <code>docs/todo.md</code>。
          </span>
        </span>
      </label>

      <label className="field field-check">
        <input
          type="checkbox"
          checked={draft.stop_before_send}
          onChange={(event) => update("stop_before_send", event.target.checked)}
        />
        <span>
          <span className="field-label">只填不发</span>
          <span className="field-hint">
            打开后流程走到「准备消息」就结束：把正文填进输入框，<strong>绝不发送</strong>。
            用来验证"定位联系人 + 输入文字"是否准确，不会给对方造成任何影响。
            <br />
            <strong>搜索式工作流不受这个开关控制</strong>：它还没接发送那一段，
            所以<strong>一定</strong>停在「已填入正文，未发送」——不会申请发送台账，
            也不会写消息摘要，审计里不会留下任何"像发过了"的痕迹。
          </span>
        </span>
      </label>

      <label className="field">
        <span className="field-label">人工确认有效期（秒）</span>
        <input
          type="number"
          min={5}
          max={600}
          value={draft.confirmation_ttl_secs}
          onChange={(event) =>
            update("confirmation_ttl_secs", Number(event.target.value) || 60)
          }
        />
      </label>

      <label className="field">
        <span className="field-label">最低 OCR 置信度</span>
        <input
          type="number"
          min={0}
          max={1}
          step={0.01}
          value={draft.min_confidence}
          onChange={(event) =>
            update("min_confidence", Number(event.target.value) || 0)
          }
        />
      </label>

      {/* ── 逐字输入与靶标文字 ──────────────────────────────────────
          这两类值都是"随客户端版本变"的，所以做成配置而不是写死在代码里。
          它们合在一起是因为**改动理由相同**：换一个靶标（或客户端升级）时，
          要动的就是这几个字与这个间隔。 */}
      <section className="calibration">
        <div className="calibration-head">
          <h3>逐字输入与靶标文字</h3>
        </div>

        <label className="field">
          <span className="field-label">逐字输入间隔（毫秒）</span>
          <input
            type="number"
            min={0}
            step={5}
            value={draft.typing_interval_ms}
            onChange={(event) =>
              update("typing_interval_ms", Math.max(0, Math.round(Number(event.target.value) || 0)))
            }
          />
          <span className="field-hint">
            搜索词与消息正文都是<strong>逐字</strong>输入的，这是字符之间的间隔
            （<code>0</code> = 不留间隔）。搜索框是联想式的：一次性灌进去的字符
            会让联想请求互相打断，下拉列表只按第一个字符的结果定格——
            现象是"搜出来的东西不对"，不会让人想到是<strong>输入太快</strong>。
            机器慢就调大，嫌慢就调小。
          </span>
        </label>

        <label className="field">
          <span className="field-label">资料页「进入聊天」入口的文字</span>
          <input
            type="text"
            value={draft.profile_chat_entry_text}
            onChange={(event) => update("profile_chat_entry_text", event.target.value)}
          />
          <span className="field-hint">
            资料页滚到底之后要点的那个入口上写的字（默认「发消息」）。
            这一项<strong>不能留空</strong>：空文字在「包含」判断里会匹配到任何一行，
            结果不是"找不到"而是<strong>找错</strong>。找不到时任务会转人工并说明原因。
          </span>
        </label>

        <label className="field">
          <span className="field-label">搜索下拉里「联系人」分组的标题</span>
          <input
            type="text"
            value={draft.search_contact_group_label}
            onChange={(event) => update("search_contact_group_label", event.target.value)}
          />
          <span className="field-hint">
            下拉是<strong>分组</strong>的（联系人 / 聊天记录 / 群聊…），
            只有「联系人」这一组下面才是人。编排时会先找这个标题，
            再<strong>只往它下方</strong>找匹配输入词的那一行——
            否则会把「聊天记录里提到这个名字」当成联系人，点进去就是别的地方。
            同样不能留空。
          </span>
        </label>
      </section>

      <button className="primary" disabled={busy || !canSave} onClick={onSave}>
        {!canSave ? "标定不合法，无法保存" : dirty ? "保存配置" : "已保存"}
      </button>
      {dirty && (
        <span className="field-hint">
          ⚠️ 有改动还没保存。任务用的是<strong>已保存</strong>的配置，不是这一页上的草稿。
        </span>
      )}

      <dl className="meta-list">
        <dt>数据目录</dt>
        <dd className="mono">{info.data_dir}</dd>
        <dt>审计记录</dt>
        <dd>{info.audit_entry_count} 条</dd>
      </dl>
      <p className="field-hint">
        数据目录是<strong>程序运行当前路径</strong>下的 <code>data/</code>，
        配置、图标库、任务日志、证据图都在里面，整个目录可以整体拷走。
        因为它是相对路径，<strong>换个目录启动程序它就会跟着变</strong>——
        双击 <code>target\debug\desktop.exe</code> 启动的话，数据目录是
        <code>target\debug\data\</code>，而 <code>cargo clean</code> 会把它一起删掉。
        想固定位置，就在你想让数据落地的目录里启动它（仓库里带了
        <code>run.cmd</code>，它先切到仓库根目录再启动）。
      </p>
    </section>
  );
}
