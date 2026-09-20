import { contactLabel } from "../taskDisplay";
import { STATE_LABELS, type TaskState, type TaskView } from "../types";

interface Props {
  tasks: TaskView[];
  activeId: string | null;
  onSelect: (taskId: string) => void;
}

function stateClass(state: TaskState): string {
  if (state === "completed") return "badge is-ok";
  if (state === "needs_human_review") return "badge is-warn";
  if (state === "failed") return "badge is-bad";
  if (state === "cancelled") return "badge is-muted";
  return "badge";
}

function formatTime(ms: number): string {
  if (!ms) return "";
  const date = new Date(ms);
  return date.toLocaleTimeString("zh-CN", { hour12: false });
}

export function TaskHistory({ tasks, activeId, onSelect }: Props) {
  if (tasks.length === 0) {
    return (
      <section className="panel">
        <h2>任务历史</h2>
        <p className="muted-line">还没有任务。</p>
      </section>
    );
  }

  return (
    <section className="panel">
      <h2>任务历史</h2>
      <ul className="history">
        {tasks.map((task) => (
          <li key={task.id}>
            <button
              className={task.id === activeId ? "history-item is-active" : "history-item"}
              onClick={() => onSelect(task.id)}
            >
              <div className="history-head">
                <span className="history-contact">{contactLabel(task.external_contact_name)}</span>
                <span className={stateClass(task.state)}>{STATE_LABELS[task.state]}</span>
              </div>
              <div className="history-meta">
                <span>{task.text_length} 字</span>
                <span>{task.created_by}</span>
                {task.history.length > 0 && (
                  <span>{formatTime(task.history[task.history.length - 1].at_ms)}</span>
                )}
              </div>
              {task.failure && (
                <div className="history-failure">
                  <code>{task.failure.code}</code>
                  <span>{task.failure.reason}</span>
                </div>
              )}
              {task.evidence_artifacts.length > 0 && (
                <div className="history-evidence">
                  脱敏证据 {task.evidence_artifacts.length} 份：
                  {task.evidence_artifacts.map((artifact) => artifact.label).join("、")}
                </div>
              )}
            </button>
          </li>
        ))}
      </ul>
    </section>
  );
}
