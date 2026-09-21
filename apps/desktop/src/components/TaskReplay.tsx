import { useEffect, useMemo, useState } from "react";

import * as api from "../api";
import { defaultStepIndex, stepMark, stepMarkClass, toReplaySteps } from "../replayView";
import { contactLabel } from "../taskDisplay";
import type { ReplayStep, TaskReplayData, ReplayDecisionEvent } from "../types";
import type { TaskView } from "../types";

interface Props {
  task: TaskView | null;
}

/**
 * 「过程重放」：**左边那一步当时的画面，右边它为什么这么判**。
 *
 * ## 它回答的是哪个问题
 *
 * 过程日志（`task.log`）说的是一句结论——「搜索下拉列表里没有找到
 * 「联系人 / 最常使用」中的任何一组，读到 19 块文字」。而 2026-09-21 的现场里，
 * 那 19 块文字**明明有**「联系人」三个字：**它为什么没认出来**？
 * 那句话答不了，图也答不了（图只说明它看到了什么）。答得了的是判定的轨迹：
 * 每一块文字过没过、为什么没过（置信度 0.61 < 阈值 0.85）。
 *
 * 所以这一页的三样东西是配套的：**图**（看到了什么）、**候选表**（怎么判的）、
 * **重放输入**（按什么判的，能拿去 `tools/replay` 重跑一遍验证）。
 *
 * ## 为什么从「任务历史」进来
 *
 * 排查的是**已经跑完的那一次**。所以入口在历史列表里（每条任务一个「过程重放」），
 * 而不是只在当前任务上——当前任务跑完还是失败的那一刻，人早就在看别的了。
 */
export function TaskReplay({ task }: Props) {
  const [data, setData] = useState<TaskReplayData | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [dirError, setDirError] = useState<string | null>(null);
  const [cursor, setCursor] = useState(0);
  const [needle, setNeedle] = useState("");

  const taskId = task?.id ?? null;

  // 与「过程日志」面板同一套依赖：任务状态每推进一次就重读一遍。
  // 于是**正在跑的任务**这一页会跟着往前走，不必手动刷新。
  useEffect(() => {
    if (!taskId) {
      setData(null);
      setError(null);
      return;
    }
    let cancelled = false;
    setError(null);
    void api
      .readTaskEvents(taskId)
      .then((next) => {
        if (cancelled) return;
        setData(next);
        // 每次重读都重新落到"该看的那一步"：任务还在跑时它每次都可能变。
        setCursor(defaultStepIndex(toReplaySteps(next.events)));
        setNeedle("");
      })
      .catch((err) => {
        if (cancelled) return;
        setData(null);
        setError(String(err));
      });
    return () => {
      cancelled = true;
    };
  }, [taskId, task?.state, task?.failure?.reason, task?.detail]);

  const steps = useMemo(() => toReplaySteps(data?.events ?? []), [data]);

  /**
   * 打开任务目录。失败时把后端那句原话显示出来——
   * 「旧布局没有目录」是要说给人听的结论，不是可以吞掉的错误。
   */
  const openDir = () => {
    if (!taskId) return;
    setDirError(null);
    void api.openTaskDir(taskId).catch((err) => setDirError(String(err)));
  };

  if (!task) {
    return (
      <section className="panel">
        <h2>过程重放</h2>
        <p className="muted-line">先在「任务」页或右侧任务历史里选一条任务。</p>
      </section>
    );
  }

  const step = steps[cursor] ?? null;

  return (
    <section className="panel">
      <h2>过程重放</h2>
      <p className="field-hint">
        {contactLabel(task.external_contact_name)} · {task.state_label}
        {task.failure ? ` · ${task.failure.code}` : ""}
      </p>

      {data?.dir && (
        <div className="replay-material">
          <p className="field-hint mono">材料目录：{data.dir}</p>
          <button type="button" className="ghost" onClick={openDir}>
            打开任务目录
          </button>
        </div>
      )}

      {dirError && <p className="notice notice-error">{dirError}</p>}

      {error && <p className="notice notice-error">{error}</p>}

      {!error && data && data.dir === null && (
        <p className="notice notice-warn">
          这次任务没有过程重放材料：它来自旧版本（那时一次任务只留一个日志文件，
          没有每一步的图与判定轨迹）。要看的细节在「过程日志」里。
        </p>
      )}

      {!error && data?.dir && steps.length === 0 && (
        <p className="muted-line">
          这次任务还没有走到任何一步。任务刚起步时就是这样，跑起来这一页会自己更新。
        </p>
      )}

      {!error && steps.length > 0 && (
        <>
          <div className="replay-steps">
            {steps.map((item, index) => (
              <button
                key={`${item.index}-${item.label}-${index}`}
                type="button"
                className={index === cursor ? "replay-step is-active" : "replay-step"}
                onClick={() => setCursor(index)}
                title={item.at}
              >
                <span className="replay-step-no">
                  {String(item.index).padStart(2, "0")}
                </span>
                <span className="replay-step-label">{item.label}</span>
                <span className={stepMarkClass(item)}>{stepMark(item)}</span>
              </button>
            ))}
          </div>

          {step && (
            <div className="replay-body">
              <StepImage step={step} />
              <StepDecision decision={step.decision} needle={needle} onNeedle={setNeedle} />
            </div>
          )}
        </>
      )}
    </section>
  );
}

/** 左边：那一步的画面。框已经由后端画在图上了，这里不再叠一层。 */
function StepImage({ step }: { step: ReplayStep }) {
  const read = step.read;
  if (!read) {
    return (
      <div className="replay-image">
        <p className="muted-line">这一步只有判定、没有留下画面。</p>
      </div>
    );
  }
  return (
    <div className="replay-image">
      <img src={api.assetUrl(read.image_path)} alt={`第 ${read.index} 步：${read.step}`} />
      <p className="field-hint mono">
        {read.t} · {read.step}
      </p>
      <p className="field-hint mono">
        区域 {read.region.x},{read.region.y} {read.region.w}×{read.region.h} · 本帧{" "}
        {read.frame.w}×{read.frame.h} · 读到 {read.boxes.length} 块文字
      </p>
      <p className="field-hint mono">{read.image}</p>
    </div>
  );
}

/** 右边：它是怎么判的 —— 问什么、按什么判、结论，以及每个候选的遭遇。 */
function StepDecision({
  decision,
  needle,
  onNeedle,
}: {
  decision: ReplayDecisionEvent | null;
  needle: string;
  onNeedle: (value: string) => void;
}) {
  if (!decision) {
    return (
      <div className="replay-detail">
        <p className="muted-line">
          这一步只截了图、没有判定（等着画面停稳的那些轮询就是这样）。
        </p>
      </div>
    );
  }

  const key = needle.trim().toLowerCase();
  const shown = key
    ? decision.candidates.filter((candidate) => candidate.text.toLowerCase().includes(key))
    : decision.candidates;

  return (
    <div className="replay-detail">
      <p className={decision.passed ? "replay-outcome is-ok" : "replay-outcome is-bad"}>
        {decision.outcome}
      </p>
      <dl className="meta-list">
        <dt>在问什么</dt>
        <dd>{decision.question}</dd>
        <dt>按什么判</dt>
        <dd>{decision.rule}</dd>
        <dt>判定时刻</dt>
        <dd className="mono">{decision.t}</dd>
        <dt>重放输入</dt>
        <dd className="mono">
          {decision.replay
            ? describeReplay(decision.replay) + ` · 阈值 ${decision.min_confidence}`
            : "这条判据不支持离线重跑"}
        </dd>
      </dl>

      <div className="replay-candidates-head">
        <span>
          候选 {shown.length} / {decision.candidates.length} 块
        </span>
        <input
          type="search"
          value={needle}
          placeholder="按文字过滤（例如联系人）"
          onChange={(event) => onNeedle(event.target.value)}
        />
      </div>

      {decision.candidates.length === 0 ? (
        <p className="muted-line">这一次判定没有候选（画面上一块文字都没读到）。</p>
      ) : (
        <table className="candidate-table">
          <thead>
            <tr>
              <th>文字</th>
              <th>置信度</th>
              <th>结果</th>
              <th>为什么</th>
            </tr>
          </thead>
          <tbody>
            {shown.map((candidate, index) => (
              <tr
                key={`${candidate.text}-${candidate.x}-${candidate.y}-${index}`}
                className={candidate.passed ? "is-passed" : undefined}
              >
                <td className="mono">{candidate.text}</td>
                <td className="mono">{candidate.confidence}</td>
                <td>{candidate.passed ? "通过" : "淘汰"}</td>
                <td className="candidate-reason">{candidate.reason}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}

/**
 * 把重放输入写成一句人话。
 *
 * 措辞与后端那次判定用的**不是同一份**（那边是判据自己的说明）——这里只回答
 * "要重跑这一条，得往 `tools/replay` 里喂什么"。
 */
function describeReplay(replay: ReplayDecisionEvent["replay"]): string {
  if (!replay) return "这条判据不支持离线重跑";
  if (replay.kind === "dropdown") {
    return `搜索关键词「${replay.keyword}」· 分组「${replay.group_labels}」`;
  }
  return `目标联系人「${replay.expected_name}」${
    replay.relaxed ? "· 用的是放宽匹配（包含即可）" : "· 逐字精确匹配"
  }`;
}