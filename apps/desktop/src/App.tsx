import { useCallback, useEffect, useMemo, useState } from "react";

import * as api from "./api";
import { ConfirmationDialog } from "./components/ConfirmationDialog";
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

export function App() {
  const [tasks, setTasks] = useState<TaskView[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [confirmation, setConfirmation] = useState<ConfirmationRequest | null>(null);
  const [info, setInfo] = useState<RuntimeInfo | null>(null);
  const [error, setError] = useState<string | null>(null);

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

  const activeTask = useMemo(
    () => tasks.find((task) => task.id === activeId) ?? null,
    [tasks, activeId],
  );

  const running = useMemo(
    () => tasks.some((task) => !TERMINAL_STATES.includes(task.state)),
    [tasks],
  );

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

  const handleSaveConfig = async (config: RuntimeConfig) => {
    try {
      await api.setRuntimeConfig(config);
      setInfo(await api.runtimeInfo());
    } catch (err) {
      setError(String(err));
    }
  };

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

      <main className="app-grid">
        <div className="column">
          <TaskForm disabled={running} onStart={handleStart} error={error} />
          {info && <RuntimePanel info={info} onSave={handleSaveConfig} busy={running} />}
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

      {confirmation && (
        <ConfirmationDialog request={confirmation} onDecide={handleDecision} />
      )}
    </div>
  );
}
