import { contactLabel } from "../taskDisplay";
import { STATE_LABELS, type TaskState, type TaskView } from "../types";

interface Props {
  tasks: TaskView[];
  activeId: string | null;
  onSelect: (taskId: string) => void;
  /** 直接跳到这条任务的「过程重放」。 */
  onReplay: (taskId: string) => void;
  /**
   * 重新从磁盘读一遍任务清单。
   *
   * ★ 为什么需要一个**人工**入口：清单只在启动时读一次，之后只靠「新建任务」
   * 与后端推来的更新事件变。别的进程（或上一次没退干净的本程序）往数据目录里
   * 新写的任务，两条路都不经过——它在界面上不会自己出现，而人只会以为
   * "历史记录丢了"。按一下刷新，就是重新问一次后端。
   */
  onRefresh: () => void;
  refreshing: boolean;
  /** 上一次刷新失败的原因（成功时为 null）。 */
  refreshError: string | null;
  /**
   * 后端**实际在读**的数据目录。
   *
   * 显示它是因为"历史怎么空了"最常见的成因就是程序的工作目录不是他以为的那个
   * （数据目录 = 启动时的工作目录 + `/data`，见 `data_dir.rs`）。把路径摆出来，
   * 这个疑问一眼就有答案，不必再去翻配置。`null` = 还没读回来。
   */
  dataDir: string | null;
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

export function TaskHistory({
  tasks,
  activeId,
  onSelect,
  onReplay,
  onRefresh,
  refreshing,
  refreshError,
  dataDir,
}: Props) {
  return (
    <section className="panel">
      <div className="panel-head">
        <h2>任务历史</h2>
        <div className="panel-head-side">
          <span className="muted-line">{tasks.length} 条</span>
          <button
            type="button"
            className="ghost panel-action"
            onClick={onRefresh}
            disabled={refreshing}
          >
            {refreshing ? "读取中…" : "刷新"}
          </button>
        </div>
      </div>

      {dataDir && (
        <p className="field-hint">
          从磁盘读：<code className="mono">{dataDir}</code>
        </p>
      )}

      {refreshError && <p className="notice notice-error">刷新失败：{refreshError}</p>}

      {tasks.length === 0 ? (
        <p className="muted-line">
          还没有任务。若盘上其实有，点「刷新」重新读一次。
        </p>
      ) : (
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
      )}
    </section>
  );
}
