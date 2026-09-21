import { contactLabel } from "../taskDisplay";
import { STATE_LABELS, type TaskState, type TaskView } from "../types";

interface Props {
  tasks: TaskView[];
  activeId: string | null;
  onSelect: (taskId: string) => void;
  /** 直接跳到这条任务的「过程重放」。 */
  onReplay: (taskId: string) => void;
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

export function TaskHistory({ tasks, activeId, onSelect, onReplay }: Props) {
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
            {/* 一条任务 = 「选中」+「过程重放」两个动作。
                所以外面这层是普通容器，里面那层才是可点的按钮：
                按钮套按钮在浏览器里会被拆开，反而把两个动作搅在一起。 */}
            <div className={task.id === activeId ? "history-item is-active" : "history-item"}>
              <button
                type="button"
                className="history-main"
                onClick={() => onSelect(task.id)}
              >
                <div className="history-head">
                  <span className="history-contact">{contactLabel(task.external_contact_name)}</span>
                  <span className={stateClass(task.state)}>{STATE_LABELS[task.state]}</span>
                </div>
                <div className="history-meta">
                  <span>{task.text_length} 字</span>
                  <span>操作者 {task.created_by || "（未填）"}</span>
                  {task.history.length > 0 && (
                    <span>{formatTime(task.history[task.history.length - 1].at_ms)}</span>
                  )}
                </div>
                {task.from_log && (
                  <div className="history-meta">来自磁盘日志</div>
                )}
                {task.failure && (
                  <div className="history-failure">
                    <code>{task.failure.code}</code>
                    <span className="failure-detail">{task.failure.reason}</span>
                  </div>
                )}
                {task.evidence_artifacts.length > 0 && (
                  <div className="history-evidence">
                    脱敏证据 {task.evidence_artifacts.length} 份：
                    {task.evidence_artifacts.map((artifact) => artifact.label).join("、")}
                  </div>
                )}
              </button>
              {/* ★ 「过程重放」每条都给。它不挑状态：跑成功的那些也值得回看，
                  而**失败的那一条**才正是要排查的。 */}
              <div className="history-foot">
                <button
                  type="button"
                  className="ghost history-replay"
                  onClick={() => onReplay(task.id)}
                >
                  过程重放
                </button>
              </div>
            </div>
          </li>
        ))}
      </ul>
    </section>
  );
}
