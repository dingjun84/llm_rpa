import { useEffect, useRef, useState } from "react";

/**
 * 「先倒数、到点再动手」的共用实现。
 *
 * ## 为什么抽出来
 *
 * 现在有三处用到：延时截图（`CaptureTrigger`）、画圆自检（`CursorMotionPanel`）、
 * 指认窗口（`WindowPicker`）。三处要的**秒数**各不相同——8 秒够切回客户端输完联系人名、
 * 3 秒够把手从按钮挪到想当圆心的位置——所以秒数留在各调用点，共用的只有"怎么数"。
 *
 * ⚠️ `WindowPicker` **没有**改用这个 hook：它那个定时器每跳一次还要顺便采一次
 * "光标下是哪个窗口"，数与采是同一拍。它只共用 [`remainingSeconds`] 的显示口径。
 * 别为了"统一"把它的采样拆出来——采样与倒计时必须同一拍，拆开就会出现
 * "倒计时结束了但最后那一次采样还没回来"。
 *
 * ## 到点判据只能有一处
 *
 * 判据是「现在 ≥ 截止时刻」，**不是**"把间隔累加起来"。写成累加的话，
 * 定时器被系统降频（后台标签页、省电模式）时会越走越慢，倒计时就成了谎话。
 * 抽到这里，这个判据就只有一份实现。
 *
 * 间隔只决定**显示精度**：降频时最多"晚一个间隔"，不会累积偏差。
 */
export const COUNTDOWN_TICK_MS = 100;

export interface Countdown {
  /** 正在倒数。 */
  counting: boolean;
  /** 还剩多少毫秒（没在倒数时是 0）。 */
  remainingMs: number;
  /** 开始倒数；已经在数时是空操作（不会重新计时）。 */
  start: () => void;
  /** 中止倒数（到点不会再触发）。没在数时是空操作。 */
  stop: () => void;
}

/**
 * 从 `start()` 起倒数 `seconds` 秒，到点调一次 `onDone`。
 *
 * `onDone` 每次渲染都会是新函数，内部用 ref 兜住——否则它一变就会把
 * 倒计时重置，表现为"刚点完又重新开始数"。
 */
export function useCountdown(seconds: number, onDone: () => void): Countdown {
  const [counting, setCounting] = useState(false);
  const [remainingMs, setRemainingMs] = useState(0);

  const onDoneRef = useRef(onDone);
  useEffect(() => {
    onDoneRef.current = onDone;
  }, [onDone]);

  useEffect(() => {
    if (!counting) return;

    const deadline = Date.now() + seconds * 1000;
    setRemainingMs(seconds * 1000);

    let fired = false;
    const id = window.setInterval(() => {
      const left = deadline - Date.now();
      if (left > 0) {
        setRemainingMs(left);
        return;
      }
      // 定时器被降频时可能一次跨过整段，所以用 fired 兜住"只触发一次"。
      if (fired) return;
      fired = true;
      window.clearInterval(id);
      setCounting(false);
      setRemainingMs(0);
      onDoneRef.current();
    }, COUNTDOWN_TICK_MS);

    return () => window.clearInterval(id);
  }, [counting, seconds]);

  return {
    counting,
    remainingMs,
    start: () => setCounting(true),
    stop: () => {
      setCounting(false);
      setRemainingMs(0);
    },
  };
}

/** 把剩余毫秒数换成显示用的整秒（**向上取**，免得刚点下去就显示 0）。 */
export function remainingSeconds(remainingMs: number): number {
  return Math.ceil(remainingMs / 1000);
}
