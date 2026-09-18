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
  // `prepared` 是"按配置在发送前停下"，不在主路径上：
  // `HAPPY_PATH.indexOf("prepared")` 会返回 -1，直接拿来当索引会让整条时间线
  // 一格都不高亮。所以它和失败态一样，用"实际推进到了哪一步"来定位。
  const isPrepared = state === "prepared";

  // 失败任务也要如实显示它推进到了哪一步。
  const reachedIndex = history.reduce((max, entry) => {
    const index = HAPPY_PATH.indexOf(entry.to);
    return index > max ? index : max;
  }, 0);

  const activeIndex =
    isStopped || isPrepared ? reachedIndex : HAPPY_PATH.indexOf(state);

  return (
    <section className="panel">
      <h2>任务进度</h2>
      <ol className="timeline">
        {HAPPY_PATH.map((step, index) => {
          const done = isCompleted || index < activeIndex;
          const active = !isCompleted && index === activeIndex;
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
      {detail && <p className="detail-line">最近一步说明：{detail}</p>}
    </section>
  );
}
