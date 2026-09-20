import { useCallback, useEffect, useMemo, useState } from "react";

import * as api from "./api";
import { CalibrationPanel, type CachedPreview } from "./components/CalibrationPanel";
import { ConfirmationDialog } from "./components/ConfirmationDialog";
import { CursorMotionPanel } from "./components/CursorMotionPanel";
import { IconLibraryPanel } from "./components/IconLibraryPanel";
import { regionsAreValid } from "./components/RegionCalibration";
import { RuntimePanel } from "./components/RuntimePanel";
import { StateTimeline } from "./components/StateTimeline";
import { TaskForm } from "./components/TaskForm";
import { TaskHistory } from "./components/TaskHistory";
import { contactLabel } from "./taskDisplay";
import {
  TERMINAL_STATES,
  type ConfirmationRequest,
  type RunChoice,
  type RuntimeConfig,
  type RuntimeInfo,
  type RuntimeMode,
  type TaskFormValues,
  type TaskView,
  type WorkflowRequirement,
} from "./types";

type Tab = "tasks" | "calibration" | "icons" | "trace";

export function App() {
  const [tasks, setTasks] = useState<TaskView[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [confirmation, setConfirmation] = useState<ConfirmationRequest | null>(null);
  const [info, setInfo] = useState<RuntimeInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [tab, setTab] = useState<Tab>("tasks");

  /**
   * 配置**草稿**。
   *
   * 由这里持有、而不是交给某个子面板：配置里有一组字段（导航图标）是在
   * 「图标库」页改的，另一组（窗口、区域、超时）在「任务」页改的，
   * 还有一组（界面标定出来的区域）在「界面标定」页改的。
   * 各页各持一份草稿的话，保存时总有一份被覆盖掉——而且被覆盖的那一份
   * 看起来"刚才明明改过"。
   */
  const [draft, setDraft] = useState<RuntimeConfig | null>(null);
  const [dirty, setDirty] = useState(false);

  /**
   * 本次任务走哪一条路 —— **运行参数，不是配置的一部分**。
   *
   * ★★ 它刻意**不放进 `draft`**：放进草稿就会跟着 `dirty` 走，于是"改了工作流"
   * 变成"配置有未保存的改动"，用户还得先去点「保存配置」才生效——而他想要的
   * 只是"这一次按我选的跑"。改它**不置 `dirty`**，因为没有东西要保存。
   *
   * 由 `App` 持有（而不是 `RuntimePanel` 自己 `useState`），理由与 `draft` 相同：
   * 它是"任务怎么跑"的一部分，提交时由 `handleStart` 补进请求里。
   *
   * 初始值取自配置里那几个同名字段——那是"上次用的那条路"，只作为起点；
   * 后端不会再读它们（见 `RunChoice`）。
   *
   * ★ 模式（演练 / 真实）也在这里。它与工作流是同一类东西：界面上选完就该按这个跑。
   * 配置里的 `mode` 只剩一个作用——给这里**设初值**。
   */
  const [runChoice, setRunChoice] = useState<RunChoice | null>(null);

  const patchRunChoice = useCallback((next: Partial<RunChoice>) => {
    setRunChoice((current) => (current ? { ...current, ...next } : current));
  }, []);

  /**
   * 标定页的截图缓存：场景 id → 那次截图。
   *
   * ★ 为什么放在**这里**、而不是标定页自己持有：切到「任务」或「图标库」
   * 页签时标定页会被卸载，它自己的 state 全没了。截图要重截一次，
   * 代价是重新把客户端摆成那个界面——用户明确抱怨过这件事。
   * 跟 `draft` 同一个道理：活得比页签久的状态，就该由 App 持有。
   *
   * ★ 只活在进程内、**不落盘**：截图里有联系人姓名和聊天内容。
   */
  const [calibrationPreviews, setCalibrationPreviews] = useState<
    Record<string, CachedPreview>
  >({});

  const cachePreview = useCallback((sceneId: string, entry: CachedPreview) => {
    setCalibrationPreviews((current) => ({ ...current, [sceneId]: entry }));
  }, []);

  /**
   * 标定页正停在哪个场景、哪一项。
   *
   * ★ 跟截图缓存同一个理由：标定页切页签会被卸载，`sceneId` 留在它自己
   * 身上就会被重置成第一个场景。用户切去「任务」看一眼再回来，
   * 会发现自己刚才在标的是「历史对话」，界面却跳回了「主界面」，
   * 而那张截图看起来就"不见了"——其实一直在缓存里。
   *
   * `null` = 还没定，由标定页读到计划后落到第一个场景上。
   */
  const [calibrationSceneId, setCalibrationSceneId] = useState<string | null>(null);
  const [calibrationActiveKey, setCalibrationActiveKey] = useState<string | null>(null);

  const upsert = useCallback((task: TaskView) => {
    setTasks((current) => {
      const index = current.findIndex((item) => item.id === task.id);
      if (index === -1) return [task, ...current];
      const next = current.slice();
      next[index] = task;
      return next;
    });
  }, []);

  useEffect(() => {
    const unlisteners: Array<() => void> = [];
    let disposed = false;

    void (async () => {
      try {
        const [list, runtime] = await Promise.all([api.listTasks(), api.runtimeInfo()]);
        if (disposed) return;
        setTasks(list);
        setInfo(runtime);
        setActiveId(list[0]?.id ?? null);
      } catch (err) {
        if (!disposed) setError(String(err));
      }
    })();

    void api
      .onTaskUpdated((task) => {
        upsert(task);
        setActiveId((current) => current ?? task.id);
      })
      .then((fn) => {
        if (disposed) fn();
        else unlisteners.push(fn);
      });

    void api
      .onConfirmationRequested((request) => {
        setConfirmation(request);
        setActiveId(request.task_id);
      })
      .then((fn) => {
        if (disposed) fn();
        else unlisteners.push(fn);
      });

    return () => {
      disposed = true;
      unlisteners.forEach((fn) => fn());
    };
  }, [upsert]);

  // 每次从后端拿到配置（首次加载、以及保存之后）都以它为准重置草稿。
  // 保存后 `setInfo(await runtimeInfo())` 会走到这里，于是"已保存"与
  // "草稿"重新对齐——这正是那个 `dirty` 标记能自动清掉的原因。
  useEffect(() => {
    if (!info) return;
    // `info.config` 本来就带着 `area_marks`（后端序列化下来的）。
    setDraft(info.config as RuntimeConfig);
    setDirty(false);
  }, [info]);

  /**
   * 运行参数的初值：只在**第一次**读到配置时定。
   *
   * ⚠️ 不能写成 `useEffect(() => setRunChoice(...), [info])`：`info` 每次
   * 「保存配置」之后都会刷新，那样每保存一次就会把用户当下选的那条路重置回
   * 配置里的默认值——"选好的工作流被保存操作悄悄换掉"，又是一个难查的坑。
   */
  useEffect(() => {
    if (!info) return;
    setRunChoice(
      (current) =>
        current ?? {
          mode: info.config.mode,
          workflow: info.config.workflow,
          nav_target: info.config.nav_target,
        },
    );
  }, [info]);

  /**
   * **这一次真正会跑的模式** —— 判据只有这一处。
   *
   * ★★ 不能用 `draft.mode`：那只是配置里的默认值，界面上切了模式、没点保存时
   * 它与真正会跑的不是一回事。真实模式那句提示是**安全提示**，
   * 按错了方向（提示说"演练"、实际在动真窗口）比没有提示更糟。
   *
   * `runChoice` 还没读回来时退回配置里的值：那种状态下界面上两个选择器都是禁用的，
   * 退回去只是为了首帧别显示空白。
   */
  const activeMode: RuntimeMode = runChoice?.mode ?? info?.config.mode ?? "dry_run";

  /**
   * 当前工作流对**任务输入**的要求（要不要填「外部联系人名称」/「消息正文」）。
   *
   * ★★ 为什么由 `App` 持有、而不是 `RunChoiceFields` 自己取：这个答案有**两个**
   * 消费者——上面那张「新建发送任务」表单（决定那两个框要不要显示、「开始任务」
   * 能不能点）与工作流那一组下面的提示。放在其中一个组件里的话，另一个只能自己
   * 猜一条判据，而猜的两种后果都很难查；猜严了就是**按钮点不动、也不说为什么**
   * （2026-09-20 实测：选了「只做导航」，点开始什么都不发生，连日志都没生成）。
   *
   * `null` = 还没问回来 / 问不到。**这时一律不拦**（见 `TaskForm`）：
   * 界面拿不到判据时宁可放行，让命令层去拒绝并给出原因——
   * **拒绝是看得见的，点不动是看不见的**。
   */
  const [requirement, setRequirement] = useState<WorkflowRequirement | null>(null);

  /**
   * 标定结果的一个**稳定签名**，用来当刷新依赖。
   *
   * 直接依赖 `draft` 会让每次按键都去问一遍后端；而只有「工作流改了」或
   * 「标定结果变了」才会改变答案。用 JSON 串而不是对象本身：
   * `area_marks` 每次 `onPatch` 都会换一个新对象，按引用比会一直不相等。
   */
  const marksSignature = JSON.stringify(draft?.area_marks ?? {});

  useEffect(() => {
    const workflow = runChoice?.workflow ?? null;
    if (!draft || !workflow) {
      setRequirement(null);
      return;
    }
    let cancelled = false;
    void (async () => {
      try {
        const all = await api.workflowRequirements(draft);
        if (cancelled) return;
        setRequirement(all.find((item) => item.workflow === workflow) ?? null);
      } catch {
        // 问不到就**不显示、也不拦**：真正的判据在装配期（后端会拒绝并列出原因）。
        // 为了这条提示弹一个错误，反而会盖住页面上真正的问题。
        if (!cancelled) setRequirement(null);
      }
    })();
    return () => {
      cancelled = true;
    };
    // 只在这两项变了才重问：答案只取决于它们。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [runChoice?.workflow, marksSignature]);

  const activeTask = useMemo(
    () => tasks.find((task) => task.id === activeId) ?? null,
    [tasks, activeId],
  );

  const running = useMemo(
    () => tasks.some((task) => !TERMINAL_STATES.includes(task.state)),
    [tasks],
  );

  const patch = useCallback((next: Partial<RuntimeConfig>) => {
    setDraft((current) => (current ? { ...current, ...next } : current));
    setDirty(true);
  }, []);

  /**
   * 「开始任务」。
   *
   * ★★ `run_choice` **必须由这里补进请求**。工作流 / 导航目标是**运行参数**，
   * 不是配置的一部分——`start_task` 只认请求里的这一份，既不去读配置里那两个
   * 同名字段，也不会把它们写回去（见 `RunChoice`）。
   *
   * 这里就是那个 bug 的修复点：以前命令层读的是**已保存的** `state.config`，
   * 于是"界面上改了工作流、没点保存"就变成"跑的还是旧的那条"。
   * 2026-09-19 实测：连跑三条任务，三条 `task-*.log` 里记的全是 `SearchContact`。
   *
   * `runChoice` 还没定（配置还没读回来）时**不提交**：那种状态下"要跑哪条路"
   * 根本没有值，随便猜一个默认值正是上面那个 bug 的老路。
   */
  const handleStart = async (values: TaskFormValues) => {
    setError(null);
    if (!runChoice) {
      setError("还没读到运行配置，稍等一下再点「开始任务」。");
      return;
    }
    try {
      const id = await api.startTask({ ...values, run_choice: runChoice });
      setActiveId(id);
    } catch (err) {
      setError(String(err));
    }
  };

  const handleDecision = async (approved: boolean, reason?: string) => {
    if (!confirmation) return;
    const taskId = confirmation.task_id;
    setConfirmation(null);
    try {
      await api.confirmTask(taskId, approved, reason);
    } catch (err) {
      setError(String(err));
    }
  };

  const handleCancel = async () => {
    if (!activeTask) return;
    try {
      await api.cancelTask(activeTask.id);
    } catch (err) {
      setError(String(err));
    }
  };

  const handleSaveConfig = async () => {
    if (!draft) return;
    setError(null);
    try {
      // ★ 保存时把**界面上当前选的模式**一并记成下次的默认值。
      //
      // 模式是运行参数（改它不置 `dirty`、也不单独保存），但配置里那个同名字段
      // 仍然要有个来源——否则它永远停在第一次写下的值，"上次用的模式"
      // 就再也不会被记住。所以在这一处（保存配置）顺手对齐。
      await api.setRuntimeConfig({ ...draft, mode: activeMode });
      setInfo(await api.runtimeInfo());
    } catch (err) {
      setError(String(err));
    }
  };

  // 演练模式不碰真实窗口，标定值用不上；真实模式则必须保证四个区域都合法，
  // 否则后端 `RelativeRegion::validate` 会在执行时才报错，白白浪费一次任务。
  // 三个页面共用这一个判断——保存按钮在每页都有，判据只能有一处。
  // 看的是 `activeMode`（这一次会跑的那个），不是配置里那个默认值。
  const canSave = draft ? activeMode === "dry_run" || regionsAreValid(draft.regions) : false;

  return (
    <div className="app">
      <header className="app-header">
        <div>
          <h1>本地消息辅助</h1>
          <p className="app-subtitle">
            企业微信一对一纯文本发送 · 本地视觉 · 每条消息都必须人工确认
          </p>
        </div>
        {info && (
          // 徽标说的是**这一次会跑的模式**，不是配置里那个默认值：
          // 它是个安全指示，必须与真正会发生的事一致。
          <span className={activeMode === "dry_run" ? "mode-pill" : "mode-pill is-live"}>
            {activeMode === "dry_run" ? "演练模式" : "真实模式"}
          </span>
        )}
      </header>

      <nav className="tabs">
        <button
          type="button"
          className={tab === "tasks" ? "tab is-active" : "tab"}
          onClick={() => setTab("tasks")}
        >
          任务
        </button>
        <button
          type="button"
          className={tab === "calibration" ? "tab is-active" : "tab"}
          onClick={() => setTab("calibration")}
        >
          界面标定
        </button>
        <button
          type="button"
          className={tab === "icons" ? "tab is-active" : "tab"}
          onClick={() => setTab("icons")}
        >
          图标库
        </button>
        <button
          type="button"
          className={tab === "trace" ? "tab is-active" : "tab"}
          onClick={() => setTab("trace")}
        >
          轨迹自检
        </button>
        {dirty && <span className="tab-dirty">有未保存的改动</span>}
      </nav>

      {tab === "tasks" && (
        <main className="app-grid">
          <div className="column">
            <TaskForm
              disabled={running}
              onStart={handleStart}
              error={error}
              needsContact={requirement?.needs_contact ?? null}
              needsMessage={requirement?.needs_message ?? null}
            />
            {info && draft && (
              <RuntimePanel
                info={info}
                draft={draft}
                onPatch={patch}
                runChoice={runChoice}
                onPatchRunChoice={patchRunChoice}
                activeMode={activeMode}
                requirement={requirement}
                onSave={handleSaveConfig}
                dirty={dirty}
                canSave={canSave}
                busy={running}
              />
            )}
          </div>

          <div className="column">
            {activeTask ? (
              <>
                <section className="panel">
                  <h2>当前任务</h2>
                  <dl className="meta-list">
                    <dt>收件人</dt>
                    <dd className="strong">{contactLabel(activeTask.external_contact_name)}</dd>
                    <dt>消息</dt>
                    <dd className="message-preview">{activeTask.text}</dd>
                    <dt>状态</dt>
                    <dd>{activeTask.state_label}</dd>
                  </dl>
                  {!TERMINAL_STATES.includes(activeTask.state) && (
                    <button className="ghost" onClick={handleCancel}>
                      取消任务
                    </button>
                  )}
                </section>

                <StateTimeline
                  state={activeTask.state}
                  detail={activeTask.detail}
                  history={activeTask.history}
                />

                {activeTask.failure && (
                  <section className="panel panel-danger">
                    <h2>停止原因</h2>
                    <p className="failure-code">{activeTask.failure.code}</p>
                    <p>{activeTask.failure.reason}</p>
                  </section>
                )}

                {activeTask.evidence.length > 0 && (
                  <section className="panel">
                    <h2>证据引用</h2>
                    <ul className="evidence-list">
                      {activeTask.evidence.map((item) => (
                        <li key={item} className="mono">
                          {item}
                        </li>
                      ))}
                    </ul>
                    <p className="field-hint">
                      这里只保存截图指纹，不含画面、姓名或消息正文。
                    </p>
                  </section>
                )}
              </>
            ) : (
              <section className="panel">
                <h2>当前任务</h2>
                <p className="muted-line">
                  左侧新建一个任务后，这里会实时显示 11 步状态流转。
                </p>
              </section>
            )}
          </div>

          <div className="column">
            <TaskHistory tasks={tasks} activeId={activeId} onSelect={setActiveId} />
          </div>
        </main>
      )}

      {tab === "calibration" && (
        <main className="app-grid is-single">
          <div className="column">
            {info && draft ? (
              <CalibrationPanel
                info={info}
                draft={draft}
                onPatch={patch}
                onSave={handleSaveConfig}
                dirty={dirty}
                canSave={canSave}
                busy={running}
                previews={calibrationPreviews}
                onCachePreview={cachePreview}
                sceneId={calibrationSceneId}
                onSceneId={setCalibrationSceneId}
                activeKey={calibrationActiveKey}
                onActiveKey={setCalibrationActiveKey}
              />
            ) : (
              <section className="panel">
                <h2>界面标定</h2>
                <p className="muted-line">正在读取运行配置…</p>
              </section>
            )}
          </div>
        </main>
      )}

      {tab === "icons" && (
        <main className="app-grid is-single">
          <div className="column">
            {info && draft ? (
              <IconLibraryPanel
                info={info}
                draft={draft}
                onPatch={patch}
                onSave={handleSaveConfig}
                dirty={dirty}
                canSave={canSave}
                busy={running}
              />
            ) : (
              <section className="panel">
                <h2>图标库</h2>
                <p className="muted-line">正在读取运行配置…</p>
              </section>
            )}
          </div>
        </main>
      )}

      {tab === "trace" && (
        <main className="app-grid is-single">
          <div className="column">
            <CursorMotionPanel busy={running} />
          </div>
        </main>
      )}

      {confirmation && (
        <ConfirmationDialog request={confirmation} onDecide={handleDecision} />
      )}
    </div>
  );
}
