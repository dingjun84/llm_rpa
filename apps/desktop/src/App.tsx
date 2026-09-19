import { useCallback, useEffect, useMemo, useState } from "react";

import * as api from "./api";
import { CalibrationPanel, type CachedPreview } from "./components/CalibrationPanel";
import { ConfirmationDialog } from "./components/ConfirmationDialog";
import { IconLibraryPanel } from "./components/IconLibraryPanel";
import { regionsAreValid } from "./components/RegionCalibration";
import { RuntimePanel } from "./components/RuntimePanel";
import { StateTimeline } from "./components/StateTimeline";
import { TaskForm } from "./components/TaskForm";
import { TaskHistory } from "./components/TaskHistory";
import {
  TERMINAL_STATES,
  type ConfirmationRequest,
  type RuntimeConfig,
  type RuntimeInfo,
  type StartTaskRequest,
  type TaskView,
} from "./types";

type Tab = "tasks" | "calibration" | "icons";

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

  const handleStart = async (request: StartTaskRequest) => {
    setError(null);
    try {
      const id = await api.startTask(request);
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
      await api.setRuntimeConfig(draft);
      setInfo(await api.runtimeInfo());
    } catch (err) {
      setError(String(err));
    }
  };

  // 演练模式不碰真实窗口，标定值用不上；真实模式则必须保证四个区域都合法，
  // 否则后端 `RelativeRegion::validate` 会在执行时才报错，白白浪费一次任务。
  // 三个页面共用这一个判断——保存按钮在每页都有，判据只能有一处。
  const canSave = draft ? draft.mode === "dry_run" || regionsAreValid(draft.regions) : false;

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
          <span className={info.config.mode === "dry_run" ? "mode-pill" : "mode-pill is-live"}>
            {info.config.mode === "dry_run" ? "演练模式" : "真实模式"}
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
        {dirty && <span className="tab-dirty">有未保存的改动</span>}
      </nav>

      {tab === "tasks" && (
        <main className="app-grid">
          <div className="column">
            <TaskForm disabled={running} onStart={handleStart} error={error} />
            {info && draft && (
              <RuntimePanel
                info={info}
                draft={draft}
                onPatch={patch}
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
                    <dd className="strong">{activeTask.external_contact_name}</dd>
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

      {confirmation && (
        <ConfirmationDialog request={confirmation} onDecide={handleDecision} />
      )}
    </div>
  );
}
