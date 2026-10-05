import { useState } from "react";

import { drawCursorCircle, ghostboxResetDevice, ghostboxRightClick } from "../api";
import { remainingSeconds, useCountdown } from "../countdown";
import type { CircleTraceView } from "../types";

/**
 * 画圆自检：点一下按钮，光标以当前所在位置为圆心走满一圈。
 *
 * ## 这是给人看的
 *
 * 光标轨迹本身很难单独验收 —— 混在完整流程里时，它的问题会被「找不到联系人」
 * 「点歪了」之类的现象盖住，排查方向会一路偏掉。这里把它单独拎出来跑一遍：
 * 是不是**走过去的**（而不是跳过去的）、走起来顺不顺、快慢合不合适，
 * 肉眼一看就知道。
 *
 * ## 为什么要点完等 3 秒才动
 *
 * 圆心取的是**倒计时结束时**光标所在的位置，所以这 3 秒是留给操作者
 * 把手从按钮挪到想当圆心的位置的。不挪也行 —— 那就是按钮那一点。
 *
 * 顺带解决另一个问题：这个命令是**同步跑完整圈才返回**的，界面在它返回前
 * 不会有任何反馈。没有倒计时的话，点下去会像"卡住了"。
 *
 * ## 半径是窗口短边的一半
 *
 * 按**本程序窗口**算，不按目标客户端算 —— 这条功能测的是光标轨迹本身，
 * 不该依赖微信开着、也不该依赖窗口标定做没做。取**短边**是为了让圆一定放得下：
 * 宽扁的窗口按宽度算，圆会超出屏幕高度。
 *
 * ⚠️ 所以**窗口拉大拉小，圆就跟着变** —— 想让圆大一点，先把窗口拉大。
 *
 * ## Windows：走幽灵盒
 *
 * 画圆的每一步都经进程级共享会话的 `MoveMouseTo`（与「远程检测」同一条路径）。
 * 「重置幽灵盒」也放在本页：平时移动只 Open 一次，重置才 Close→Reset。
 *
 * ## HID 右键自检
 *
 * 另有「测试 HID 右键」：倒计时 3 秒后在**当前光标位置**触发
 * `PressAndReleaseMouseButton(3)`（官方：3=右键，返回 2=成功），**不**先 MoveMouseTo。
 * 用来单独验证任务里声称调了右击但菜单没出现时，硬件右键本身是否生效。
 */

/**
 * 倒计时的等待时长（秒）。
 *
 * 3 秒够把手从按钮挪到想当圆心的位置（通常就是屏幕中间），又不至于干等到不耐烦。
 *
 * ★ 与延时截图的 8 秒**不是同一个数**，别合并：那边要够切回客户端输完一个
 * 联系人名，这边只是挪一下手。
 *
 * ★ 这是**故意写死**的：延时是纯粹的界面行为，不属于任务配置，要持久化就得
 * 新增一个配置字段，而那个字段在配置里没有任何别的用处。改它只影响等待时长，
 * **不影响任何判据**。
 */
const DELAY_TRACE_SECS = 3;

interface Props {
  /** 有任务正在跑。这时不让点：任务也在动光标，两边会打架。 */
  busy: boolean;
}

export function CursorMotionPanel({ busy }: Props) {
  const [result, setResult] = useState<CircleTraceView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [running, setRunning] = useState(false);
  const [rightClicking, setRightClicking] = useState(false);
  const [resetting, setResetting] = useState(false);
  const [log, setLog] = useState<string[]>([]);

  const pushLog = (line: string) => {
    setLog((prev) => [...prev.slice(-40), line]);
  };

  const run = async () => {
    setRunning(true);
    setError(null);
    try {
      // 这个 await 会一直挂到整圈走完。界面在这期间只是按钮变灰，
      // 不会给出中间反馈 —— 光标自己就是那个反馈。
      const next = await drawCursorCircle();
      setResult(next);
      if (next.notice) {
        pushLog(next.notice);
      }
    } catch (err) {
      setResult(null);
      setError(String(err));
      pushLog(`画圆失败：${String(err)}`);
    } finally {
      setRunning(false);
    }
  };

  const runRightClick = async () => {
    setRightClicking(true);
    setError(null);
    pushLog("正在触发 HID 右键（当前光标位置，不 MoveMouseTo）…");
    try {
      const next = await ghostboxRightClick();
      pushLog(next.notice);
    } catch (err) {
      setError(String(err));
      pushLog(`HID 右键失败：${String(err)}`);
    } finally {
      setRightClicking(false);
    }
  };

  const resetDevice = async () => {
    setResetting(true);
    setError(null);
    try {
      const reset = await ghostboxResetDevice();
      pushLog(reset.notice);
    } catch (err) {
      setError(String(err));
      pushLog(`重置失败：${String(err)}`);
    } finally {
      setResetting(false);
    }
  };

  // `run` 每次渲染都是新函数，但 hook 内部把它放进 ref 了，不会因此重新计时。
  const circleCountdown = useCountdown(DELAY_TRACE_SECS, () => {
    void run();
  });
  const rightClickCountdown = useCountdown(DELAY_TRACE_SECS, () => {
    void runRightClick();
  });

  const circleSeconds = remainingSeconds(circleCountdown.remainingMs);
  const rightClickSeconds = remainingSeconds(rightClickCountdown.remainingMs);
  const anyCounting = circleCountdown.counting || rightClickCountdown.counting;
  const blocked = busy || running || rightClicking || resetting || anyCounting;

  return (
    <section className="panel">
      <h2>鼠标轨迹自检</h2>

      <p className="muted-line">
        点下面的按钮，光标会以<strong>倒计时结束时它所在的位置</strong>为圆心，
        沿圆周匀速走满一圈，<strong>画完停在圆的另一个位置</strong>（不回起点）。
        圆心、半径、以及<strong>走完之后光标实际在哪儿</strong>都会报出来。
        Windows 上每一步经<strong>幽灵盒</strong>{" "}
        <code className="mono">MoveMouseTo</code>
        （进程级共享会话，与远程检测同一条路径）。
      </p>

      <div className="guide-stage-actions">
        {circleCountdown.counting ? (
          <>
            <button type="button" onClick={circleCountdown.stop}>
              取消画圆
            </button>
            <span className="picker-countdown" role="status">
              把鼠标移到想当圆心的位置… {circleSeconds} 秒后开始
            </span>
          </>
        ) : (
          <button
            type="button"
            disabled={blocked}
            onClick={circleCountdown.start}
          >
            {running ? "正在画圆…" : `画一个圆（${DELAY_TRACE_SECS} 秒后开始）`}
          </button>
        )}
        {rightClickCountdown.counting ? (
          <>
            <button type="button" onClick={rightClickCountdown.stop}>
              取消右键
            </button>
            <span className="picker-countdown" role="status">
              将在 {rightClickSeconds}s 后右键（把鼠标移到企微气泡等位置）
            </span>
          </>
        ) : (
          <button
            type="button"
            disabled={blocked}
            onClick={() => {
              pushLog(`将在 ${DELAY_TRACE_SECS}s 后触发 HID 右键（不 MoveMouseTo）`);
              rightClickCountdown.start();
            }}
          >
            {rightClicking
              ? "正在右键…"
              : `测试 HID 右键（${DELAY_TRACE_SECS} 秒后）`}
          </button>
        )}
        <button
          type="button"
          disabled={busy || resetting || running || rightClicking || anyCounting}
          onClick={() => void resetDevice()}
        >
          {resetting ? "重置中…" : "重置幽灵盒"}
        </button>
      </div>

      <p className="field-hint">
        「测试 HID 右键」：倒计时结束后在<strong>当前光标位置</strong>触发幽灵盒{" "}
        <code className="mono">PressAndReleaseMouseButton(3)</code>
        （官方 3=右键；返回码 2=成功 / 0=失败），
        <strong>不会</strong>先 MoveMouseTo。看企微等是否弹出右键菜单；成功/失败与 DLL
        返回码会写进下方状态日志，以及{" "}
        <code className="mono">%TEMP%\ghostbox-replay.log</code>。
        <br />
        「重置幽灵盒」始终可点（任务跑着时除外）：关闭进程级会话（CloseDevice →
        ResetDevice）并等待 2 秒。平时画圆 / 远程移动 / 右键只 Open 一次，不会每次都关。
        <br />
        ⚠️ 轨迹<strong>只有跑起来才看得见</strong> —— 命令返回时已经走完了，
        不会留下任何痕迹。所以点完就别动鼠标，看着屏幕。
        <br />
        ⚠️ 倒计时期间<strong>别最小化本窗口</strong>：窗口最小化时浏览器会把定时器降频，
        开始得会晚一些（晚多少不定，但一定会晚）。
        <br />
        画完光标<strong>不回起点</strong>：它停在圆上的另一个位置，与起点差 90°——
        这样"位置变了"本身就是它走过一圈的证据。
        <br />
        「离圆心」那个数应当与<strong>半径</strong>接近（走对了就在圆上）。
        要是它接近 0，说明整段轨迹<strong>没有生效</strong>：光标还停在原地，
        而按钮照样会正常返回——光看返回值是发现不了的。
      </p>

      {error && (
        <p className="calibration-problem" role="alert">
          {error}
        </p>
      )}

      {result && (
        <dl className="meta-list picker-result">
          <dt>圆心</dt>
          <dd className="mono">
            ({result.center[0]}, {result.center[1]}) · 屏幕坐标
          </dd>
          <dt>半径</dt>
          <dd>
            {result.radius} 像素 · 本窗口 {result.window_width}×{result.window_height} 的短边一半
          </dd>
          <dt>这一圈</dt>
          <dd>
            {result.steps} 步 / 用时 {result.duration_ms} 毫秒 · 按 {result.speed_px_per_sec} 像素每秒走的
          </dd>
          {/* ★ 走完之后**实测**的光标位置。上面那些都是"我让它怎么走"，
              只有这一行是"它最后到底在哪儿"——两者对不上就说明轨迹没生效，
              而那正是"按钮正常返回、屏幕上什么都没发生"的那种情况。 */}
          <dt>实测终点</dt>
          <dd className="mono">
            ({result.end[0]}, {result.end[1]}) · 离圆心 {result.end_distance_px} 像素
          </dd>
          {result.notice ? (
            <>
              <dt>摘要</dt>
              <dd>{result.notice}</dd>
            </>
          ) : null}
        </dl>
      )}

      {log.length > 0 && (
        <div className="calibration">
          <div className="calibration-head">
            <h3>状态日志</h3>
          </div>
          <pre className="mono" style={{ whiteSpace: "pre-wrap", fontSize: 12 }}>
            {log.join("\n")}
          </pre>
        </div>
      )}

      <p className="field-hint">
        「画一个圆」这条自检<strong>只移动光标</strong>：不点击、不输入、不抢前台，
        所以演练模式下也能用，也不会在客户端里留下任何痕迹。
        「测试 HID 右键」会真实发出右键，用于验证菜单能否弹出。
        <br />
        速度<strong>不在这里配</strong> —— 它只在后端有一处定义，也就是任务里用的那个值；
        界面把它报出来只是让你能核对，而不是让你去调它。
        <br />
        逐步日志：<code className="mono">%TEMP%\ghostbox-replay.log</code>
        （以及 exe 旁同名文件）。
      </p>
    </section>
  );
}
