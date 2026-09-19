import { HAPPY_PATH, STATE_LABELS, type HistoryEntry, type TaskState } from "../types";

interface Props {
  state: TaskState;
  detail: string | null;
  history: HistoryEntry[];
}

const TERMINAL_FAILURE: TaskState[] = ["needs_human_review", "failed", "cancelled"];

export function StateTimeline({ state, detail, history }: Props) {
  const isStopped = TERMINAL_FAILURE.includes(state);
  const isCompleted = state === "completed";
  // `prepared` / `navigated` 是**别的终点**，不在"查找并发送"这条主路径上：
  // `HAPPY_PATH.indexOf` 会返回 -1，直接拿来当索引会让整条时间线一格都不高亮。
  // 所以它们和失败态一样，用"实际推进到了哪一步"来定位。
  const isPrepared = state === "prepared";
  const isNavigated = state === "navigated";
  const usesReachedIndex = isStopped || isPrepared || isNavigated;

  // 失败任务也要如实显示它推进到了哪一步。
  const reachedIndex = history.reduce((max, entry) => {
    const index = HAPPY_PATH.indexOf(entry.to);
    return index > max ? index : max;
  }, 0);

  const activeIndex = usesReachedIndex ? reachedIndex : HAPPY_PATH.indexOf(state);

  /**
   * 走过的状态。
   *
   * 取 `from` 与 `to` 两端：第一条记录的 `from` 是 `draft`，
   * 而 `draft` 永远不会作为 `to` 出现——只取 `to` 的话，第一格永远不亮。
   *
   * ★ 为什么按**实际轨迹**判断"走过"，而不是按"下标小于当前"：
   * 搜索式会多走资料页那两步，列表扫描式不经过它们。按下标推的话，
   * 列表式会把那两步也标成"已完成"——界面显示的和实际发生的对不上，
   * 而这一栏的全部价值就是"如实显示推进到哪儿了"。
   */
  const visited = new Set(history.flatMap((entry) => [entry.from, entry.to]));

  return (
    <section className="panel">
      <h2>任务进度</h2>
      <ol className="timeline">
        {HAPPY_PATH.map((step, index) => {
          const active = !isCompleted && index === activeIndex;
          const done = !active && visited.has(step);
          const className = [
            "timeline-step",
            done ? "is-done" : "",
            active && isStopped ? "is-stopped" : "",
            active && !isStopped ? "is-active" : "",
          ]
            .filter(Boolean)
            .join(" ");
          return (
            <li key={step} className={className}>
              <span className="timeline-marker" aria-hidden="true" />
              <span className="timeline-label">{STATE_LABELS[step]}</span>
            </li>
          );
        })}
      </ol>

      {isStopped && (
        <p className="outcome outcome-stop">
          任务已停在「{STATE_LABELS[state]}」，没有继续执行。
        </p>
      )}
      {isPrepared && (
        <p className="outcome outcome-prepared">
          正文已填入输入框，按「只填不发」配置停在这里，<strong>没有发送</strong>。
          核对无误后可以关掉该开关重新执行，或在企业微信里手动处理。
        </p>
      )}
      {isNavigated && (
        <p className="outcome outcome-prepared">
          已找到并点击导航图标，停在<strong>「已切换视图」</strong>——
          「只做导航」这条工作流不查找任何人，也不发送任何消息。
        </p>
      )}
      {detail && <p className="detail-line">最近一步说明：{detail}</p>}
    </section>
  );
}
