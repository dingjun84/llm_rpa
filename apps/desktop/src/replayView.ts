/**
 * 把事件流整理成界面上能看的一步一步。
 *
 * ## 为什么配对逻辑单独一处
 *
 * 盘上落的是**一条一条的事件**（`events.jsonl`：一次"看图"一条、一次"判定"一条），
 * 而人要看的是**一步**：左边是那一步当时的画面，右边是它为什么这么判。
 * 这个配对规则只写在这里——组件里再写一遍的话，两处迟早对不上，
 * 而症状是"图上这一步、表里是另一步的结论"，两边都是**真实数据**，
 * 看不出是错的。
 */

import type { ReplayEvent, ReplayStep } from "./types";

/**
 * 一步 = 一次「看图」+ 紧随其后、同一个步骤名的一次「判定」。
 *
 * ## 三个决定
 *
 * - 判定挂到**它前面最近的一次同名看图**上：重试会让同一个步骤名出现多次，
 *   真正做判断的是最后一帧。
 * - 同一个步骤名下**已经配过一次判定**的，不再复用：第二次判定另起一步
 *   （宁可多一步、也不要让前一条结论被覆盖掉）。
 * - 找不到对应的图时也照样建一步（`read` 为 `null`）：实现方只上报了判定
 *   这种情形是可能的，**少一条结论**比"步骤看起来少了一截"更糟。
 *
 * 未知的 `kind`（将来加了新事件）直接跳过：读不懂的东西不该被当成坏数据画出来。
 */
export function toReplaySteps(events: ReplayEvent[]): ReplayStep[] {
  const steps: ReplayStep[] = [];
  for (const event of events) {
    if (event.kind === "read") {
      steps.push({
        index: event.index,
        label: event.step,
        at: event.t,
        read: event,
        decision: null,
      });
      continue;
    }
    if (event.kind === "decision") {
      const at = lastUnjudgedIndex(steps, event.step);
      if (at >= 0) {
        steps[at].decision = event;
      } else {
        steps.push({
          index: steps.length + 1,
          label: event.step,
          at: event.t,
          read: null,
          decision: event,
        });
      }
    }
  }
  return steps;
}

function lastUnjudgedIndex(steps: ReplayStep[], label: string): number {
  for (let i = steps.length - 1; i >= 0; i -= 1) {
    if (steps[i].label === label && steps[i].decision === null) return i;
  }
  return -1;
}

/**
 * 打开时默认停在哪一步：**最后一条没通过的判定**。
 *
 * ## 为什么是"最后一条没通过的"、而不是"第一条"或"最后一步"
 *
 * 流程是在最后一次失败那里停下的，那一步才是要看的。而"第一条没通过的"
 * 会把人带到很远的地方去：列表扫描式**每一屏**都会判一次、没找到就继续往下滚，
 * 于是"第一条失败"可能只是第一屏没这个人——那是正常的。
 *
 * 全都通过了（任务正常跑完）时落在最后一步：那时没有"出事的那一步"可看。
 */
export function defaultStepIndex(steps: ReplayStep[]): number {
  if (steps.length === 0) return 0;
  for (let i = steps.length - 1; i >= 0; i -= 1) {
    const decision = steps[i].decision;
    if (decision && !decision.passed) return i;
  }
  return steps.length - 1;
}

/**
 * 步骤选择条上那个记号：**过了 / 没过 / 只看了图**。
 *
 * "只看了图"不是"过了"——等着画面停稳的那些轮询只截不判，
 * 把它显示成通过，会让人以为那一步也判过一遍。
 */
export function stepMark(step: ReplayStep): string {
  if (!step.decision) return "只看图";
  return step.decision.passed ? "通过" : "未通过";
}

/** 步骤选择条上那个记号的样式类名。 */
export function stepMarkClass(step: ReplayStep): string {
  if (!step.decision) return "step-mark is-none";
  return step.decision.passed ? "step-mark is-ok" : "step-mark is-bad";
}