import type {
  RunChoice,
  RuntimeConfig,
  RuntimeInfo,
  RuntimeMode,
  WorkflowRequirement,
} from "../types";
import { AdvancedParamsSection } from "./AdvancedParamsSection";
import { RunChoiceFields } from "./RunChoiceFields";
import { TargetWindowSection } from "./TargetWindowSection";
import { TypingTextSection } from "./TypingTextSection";

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
  /**
   * 本次任务走哪一条路。
   *
   * ★★ 它**不在 `draft` 里**：这是**运行参数**，改它既不置 `dirty`、
   * 也不需要点「保存配置」。提交时由 `App.handleStart` 补进任务请求
   * （见 `RunChoice`）。`null` = 配置还没读回来，三个选择器先禁用。
   */
  runChoice: RunChoice | null;
  /** 改运行参数。**不落盘、不置 `dirty`** —— 它本来就只对这一次任务有效。 */
  onPatchRunChoice: (patch: Partial<RunChoice>) => void;
  /**
   * **这一次真正会跑的模式**（演练 / 真实），由 `App` 算好传下来。
   *
   * ★ 不读 `draft.mode`：那只是配置里的默认值，界面上切了模式、没点保存时
   * 它与真正会跑的不是一回事。而下面那句提示是**安全提示**，
   * 按错了方向（提示说"演练"、实际在动真窗口）比没有提示更糟。
   */
  activeMode: RuntimeMode;
  /**
   * 当前工作流的要求（要不要填联系人 / 正文、还缺哪几块标定）。
   *
   * ★ 由 `App` 算好传下来，**不在这里自己取**：上面那张「新建发送任务」表单
   * 也要用它决定那两个框显不显示。两个消费者各取一次的话，会有两次请求、
   * 两份可能不同步的答案。
   */
  requirement: WorkflowRequirement | null;
  onSave: () => void;
  /** 草稿与已保存的配置是否不一致。 */
  dirty: boolean;
  /** 草稿是否满足保存条件（区域标定合法）。 */
  canSave: boolean;
  busy: boolean;
}

/**
 * 「任务」页的配置面板。
 *
 * ## 这个文件现在只留骨架
 *
 * 它自己的职责收窄成三件事：**页头那几条提示**、**保存按钮**、
 * **页面底部那份数据目录说明**。表单按"归属"拆给了四个同级组件——
 * 判据一句话：
 *
 * | 归属 | 组件 | 改完要不要点保存 |
 * |---|---|---|
 * | 本次怎么跑（模式 / 工作流 / 导航目标） | `RunChoiceFields` | **不用** |
 * | 目标窗口（指认 / 量尺寸 / OCR 路径） | `TargetWindowSection` | 要 |
 * | 超时与滚动查找参数 | `AdvancedParamsSection` | 要 |
 * | 逐字输入间隔与靶标文字 | `TypingTextSection` | 要 |
 *
 * ★ **往这个文件里加东西之前先问一句"它属于上面哪一组"** ——
 * 之前 858 行就是这么长起来的：每一项都有理由加进来，但没人回头分过组。
 * 加不进去的（比如一个新的开关）就留在下面这一段里。
 */
export function RuntimePanel({
  info,
  draft,
  onPatch,
  runChoice,
  onPatchRunChoice,
  activeMode,
  requirement,
  onSave,
  dirty,
  canSave,
  busy,
}: Props) {
  // 改草稿的字段。本地适配器——只为让下面的调用点短一点，判据不在这里。
  const update = <K extends keyof RuntimeConfig>(key: K, value: RuntimeConfig[K]) => {
    onPatch({ [key]: value } as Partial<RuntimeConfig>);
  };

  // 演练模式不碰真实窗口，标定值用不上；真实模式则必须保证四个区域都合法，
  // 否则后端 `RelativeRegion::validate` 会在执行时才报错，白白浪费一次任务。
  // 这个判断由 `App` 做（它同时还要给「图标库」页用），这里只读结果。

  // 界面上显示的是 `draft`（草稿），而 `start_task` 读的是**已保存的** `state.config`
  // （见 `lib.rs::start_task`）。两者不一致时，光看界面会以为改动已经生效——
  // 必须明说，而且要说清**现在真正生效的是什么**。
  //
  // ⚠️ 这里**不逐项列"哪些字段改了"**：那是一份会漂移的清单，漏掉一项就正好漏掉
  // 最要命的那一项。所以判据是**草稿与已保存不一致**（`dirty`）。
  //
  // ★ 模式与工作流**都不在**这条提示的范围里：它们是**运行参数**，选了就生效、
  // 不写配置（见 `RunChoice`）。把它们混进来正是当初那个 bug 的来源——
  // 提示说"用的是已保存的那份"，而人以为界面上选的那条已经生效了。

  return (
    <section className="panel">
      <h2>运行模式</h2>

      {/* 这句描述的是**这一次真正会跑**的模式。刻意用后端下发的文案
          （`info.mode_notices`），不在前端再拼一遍：同一句提示维护两处必然漂移。
          ★ 而取值必须按 `activeMode`——后端发的是**一对**（演练/真实各一句），
          正因为模式现在是运行参数，界面上选的与配置里存的可以不是同一个。 */}
      <p className={activeMode === "dry_run" ? "notice" : "notice notice-live"}>
        {info.mode_notices[activeMode]}
      </p>

      {dirty && (
        <p className="notice notice-warn">
          界面上的配置有改动，<strong>还没保存</strong>——现在点「开始任务」用的仍然是
          已保存的那一份。要让改动生效，点最下面的「保存配置」。
          <br />
          （<strong>模式和「工作流」都不受这条影响</strong>：它们是
          <strong>本次任务</strong>的运行参数，选完直接点「开始任务」就生效，
          既不用保存、也不会写进配置文件。）
        </p>
      )}

      {/* 启动时那次一次性数据搬迁的结果（旧版把配置与图标库放在 AppData 里）。
          有值就一定要显示：搬成功了用户会疑惑"配置怎么突然有值了"，
          搬失败了会以为"数据丢了"——两种都得有答案。 */}
      {info.migration_note && <p className="notice notice-warn">{info.migration_note}</p>}

      <RunChoiceFields
        info={info}
        draft={draft}
        onPatch={onPatch}
        runChoice={runChoice}
        onPatchRunChoice={onPatchRunChoice}
        activeMode={activeMode}
        requirement={requirement}
      />

      <TargetWindowSection draft={draft} onPatch={onPatch} busy={busy} />

      {/* 四个区域（列表区 / 对话标题 / 对话内容 / 输入框）只在「界面标定」页标：
          那里是"截一张图、在图上框一块"，每一块按它所在的界面分场景列出、带编号和提示。
          这里原来还有一份"填个数字、看一眼框"的旧版面板，改的是**同一份配置**，
          留着只会让人不知道该信哪一边，已经删掉。 */}

      {/* 导航图标那一组（开关 / 模板 / 阈值 / 搜索区）在「图标库」页。
          它的操作方式是"截一张图、在图上框一个图标、起名字"，
          和界面标定那种"框一块区域"是两回事，混在一起两边都别扭。 */}

      <AdvancedParamsSection draft={draft} onPatch={onPatch} />

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
            （实测把「李四」读成「0 李四」），而精确匹配对这种噪声零容忍，
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
            关掉它时，三条工作流走的是同一条路：<strong>先过人工确认，确认之后才真的发出去</strong>。
            演练模式下端口整组都是替身，本来就不会发到任何地方，所以这条路在演练模式下也跑得完。
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

      <TypingTextSection draft={draft} onPatch={onPatch} />

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
