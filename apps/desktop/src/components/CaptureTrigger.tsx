import type { UnlistenFn } from "@tauri-apps/api/event";
import { useEffect, useRef, useState } from "react";

import {
  onCaptureHotkey,
  registerCaptureHotkey,
  unregisterCaptureHotkey,
} from "../api";
import { remainingSeconds, useCountdown } from "../countdown";

/**
 * 截图的三种触发方式：立刻截、延时截、按热键截。
 *
 * ## 为什么需要「延时截图」
 *
 * 客户端里有些画面是**失焦即收**的临时弹层（搜索下拉框、右键菜单）。
 * 「截图并标注」这个按钮本身就要求操作者把鼠标点到本程序的窗口上——而这一点下去，
 * 前台就转到了本程序，弹层在**按下的那一刻**已经收起。等截屏真正执行时，
 * 截到的是收起后的画面，而操作者只会看到"下拉框不见了"。
 *
 * 延时截图把顺序倒过来：先点按钮（这时画面还没摆好，无所谓），再切回客户端把画面
 * 摆出来，到点自动截。**截屏本身不抢前台**——它只是一次只读的屏幕抓取——
 * 所以到点时客户端仍然持有那个弹层。
 *
 * ## 为什么还留着热键
 *
 * 延时截图有代价：每次重截都得重新把画面摆一遍。热键没有这个代价——画面已经摆好，
 * 按一下直接截。所以两种都留着：热键为主，延时兜底（热键可能被别的程序占用，
 * 那种情况下注册会失败，界面会把原因说出来）。
 *
 * ⚠️ 热键是**全局**资源。本组件只负责在界面上发起注册，并在卸载时还回去——
 * 详见下面那个 `unregisterCaptureHotkey` 的清理函数。
 */

/**
 * 延时截图的等待时长（秒）。
 *
 * 够切回客户端、输入一个联系人名、等下拉框出来，又不至于干等到不耐烦。
 *
 * ★ 这是**故意写死**的。做成可配要付的代价：延时是纯粹的界面行为，不属于任务配置，
 * 要持久化就得新增一个配置字段，还要额外解决「标定页切页签会被卸载」带来的状态丢失
 * ——那个字段在配置里没有任何别的用处。而改这个常量只影响等待时长，
 * **不影响任何判据**。真要调，改这一个数就够。
 */
const DELAY_CAPTURE_SECS = 8;

/**
 * 倒计时的刷新间隔（毫秒）。
 *
 * 到点判据看的是**截止时刻**，不是这个值累加出来的结果，所以它只决定显示精度：
 * 定时器被系统降频时，最多也只是"晚一个间隔"，不会越走越慢。
 *
 * ★ 这条与「到点判据」一起搬去了 `../countdown`（画圆自检也要用同一份）。
 * 这里不再留副本——**判据只能有一处**。
 */

/** 四个修饰键的勾选项。顺序即界面顺序。 */
const MODIFIERS = [
  { field: "ctrl", label: "Ctrl" },
  { field: "alt", label: "Alt" },
  { field: "shift", label: "Shift" },
  { field: "win", label: "Win" },
] as const;

type ModifierField = (typeof MODIFIERS)[number]["field"];
type ModifierState = Record<ModifierField, boolean>;

interface Props {
  /** 目标窗口已经能定位（类名填了、窗口在）。不满足时三种方式都点不了。 */
  windowReady: boolean;
  /** 别的操作正占着界面（保存配置、清理失效项……）。 */
  busy: boolean;
  /** 正在截图。 */
  capturing: boolean;
  /** 当前界面已经有截图了（按钮文案从「截图并标注」变成「重新截图」）。 */
  hasPreview: boolean;
  /** 立刻截一张，走标定页原有的那条路径。 */
  onCapture: () => Promise<void>;
}

export function CaptureTrigger({
  windowReady,
  busy,
  capturing,
  hasPreview,
  onCapture,
}: Props) {
  const [mods, setMods] = useState<ModifierState>({
    ctrl: true,
    alt: true,
    shift: false,
    win: false,
  });
  const [key, setKey] = useState("S");
  const [registered, setRegistered] = useState<string | null>(null);
  const [hotkeyBusy, setHotkeyBusy] = useState(false);
  const [hotkeyError, setHotkeyError] = useState<string | null>(null);

  // 热键监听只挂一次（见下），拿到的是**首次渲染**那个回调闭包，所以要走 ref
  // 取最新的一份。倒计时那条路不需要这个——`useCountdown` 内部自己兜了。
  const onCaptureRef = useRef(onCapture);
  useEffect(() => {
    onCaptureRef.current = onCapture;
  }, [onCapture]);

  // 倒计时结束后要调的就是这个回调。hook 内部已经把它放进 ref 了，
  // 这里不用再包一层——**别把它写进依赖数组**，否则每次渲染都会重新计时。
  const { counting, remainingMs, start, stop } = useCountdown(DELAY_CAPTURE_SECS, () =>
    void onCapture(),
  );

  // 监听一直挂着，不等"启用热键"时才挂：否则点完启用立刻按组合键会漏掉——
  // 后端已经注册生效了，这边的监听却还没建立。
  useEffect(() => {
    let alive = true;
    let detach: UnlistenFn | null = null;
    void onCaptureHotkey(() => {
      void onCaptureRef.current();
    })
      .then((unlisten) => {
        if (alive) detach = unlisten;
        else void unlisten();
      })
      .catch(() => {
        // 监听建立失败只会让"按了热键没反应"。这里不额外报警：
        // 该报的是注册失败，那条路已经把原因显示出来了。
      });
    return () => {
      alive = false;
      if (detach) void detach();
    };
  }, []);

  // 卸载（切页签、关窗口）时把热键还回去。
  //
  // 热键是**全局**资源：留着不注销会一直占着那个组合，别的程序再也注册不上，
  // 而用户完全不知道是本程序占的。清理函数里没地方报错，也不需要有——
  // 万一没注销掉，进程退出时系统也会回收。
  useEffect(() => {
    return () => {
      void unregisterCaptureHotkey().catch(() => {});
    };
  }, []);

  const enableHotkey = async () => {
    setHotkeyBusy(true);
    setHotkeyError(null);
    try {
      // 后端会先把旧的热键释放掉再注册新的，所以改了组合直接点这个就行，
      // 不必先「停用」。返回值是**后端规范化后**的写法，界面照它显示——
      // 这样"显示的组合"与"实际注册的组合"不可能不一致。
      setRegistered(await registerCaptureHotkey({ ...mods, key }));
    } catch (err) {
      setRegistered(null);
      setHotkeyError(String(err));
    } finally {
      setHotkeyBusy(false);
    }
  };

  const disableHotkey = async () => {
    setHotkeyBusy(true);
    try {
      await unregisterCaptureHotkey();
      setRegistered(null);
      setHotkeyError(null);
    } catch (err) {
      setHotkeyError(String(err));
    } finally {
      setHotkeyBusy(false);
    }
  };

  const seconds = remainingSeconds(remainingMs);
  const blocked = !windowReady || busy;

  return (
    <div className="capture-trigger">
      <div className="guide-stage-actions">
        <button
          type="button"
          disabled={blocked || capturing || counting}
          onClick={() => void onCapture()}
        >
          {capturing ? "截取中…" : hasPreview ? "重新截图" : "截图并标注"}
        </button>

        {counting ? (
          <>
            <button type="button" onClick={stop}>
              取消延时
            </button>
            <span className="picker-countdown" role="status">
              切到客户端把画面摆好… {seconds} 秒后自动截
            </span>
          </>
        ) : (
          <button
            type="button"
            disabled={blocked || capturing}
            onClick={start}
          >
            延时截图（{DELAY_CAPTURE_SECS} 秒）
          </button>
        )}
      </div>

      <p className="field-hint">
        有些画面<strong>一失焦就收</strong>（搜索下拉框、右键菜单）：点「截图并标注」这个动作本身
        会把前台抢过来，弹层在你按下去的那一刻就没了。这类画面用「延时截图」——
        点完立刻切到客户端，把画面摆出来，到点自动截。
        <br />
        ⚠️ 倒计时期间<strong>别最小化本窗口</strong>：窗口最小化时浏览器会把定时器降频，到点会晚一些。
      </p>

      <div className="capture-hotkey">
        <span className="field-label">热键截屏</span>
        <p className="field-hint">
          比延时更省事：画面已经摆好时按一下组合键就截，<strong>不需要把前台让给本程序</strong>，
          所以下拉框、右键菜单这类画面按这个键不会收。
        </p>

        <div className="capture-hotkey-row">
          {MODIFIERS.map((item) => (
            <label key={item.field} className="capture-hotkey-mod">
              <input
                type="checkbox"
                checked={mods[item.field]}
                onChange={(event) =>
                  setMods((prev) => ({ ...prev, [item.field]: event.target.checked }))
                }
              />
              <span>{item.label}</span>
            </label>
          ))}
          <span className="capture-hotkey-plus">+</span>
          <input
            className="capture-hotkey-key"
            value={key}
            // 最长的合法主键是 `F12`，三个字符。限长在这里是**输入便利**，
            // 不是判据——合不合法一律由后端说了算（它会把原因讲清楚）。
            maxLength={3}
            placeholder="S"
            aria-label="主键"
            onChange={(event) => setKey(event.target.value)}
          />
        </div>

        <div className="capture-hotkey-actions">
          <button type="button" disabled={hotkeyBusy} onClick={() => void enableHotkey()}>
            {hotkeyBusy ? "处理中…" : registered ? "重新注册" : "启用热键"}
          </button>
          {registered !== null && (
            <>
              <button
                type="button"
                className="ghost"
                disabled={hotkeyBusy}
                onClick={() => void disableHotkey()}
              >
                停用
              </button>
              <span className="capture-hotkey-state is-on" role="status">
                已生效：{registered}
              </span>
            </>
          )}
        </div>

        <p className="field-hint">
          主键只填<strong>一个键</strong>，修饰键用左边的勾选框。必须勾至少一个修饰键——
          只有主键的组合会把那个键在整个系统里占掉，别的程序就没法输入了。
          切到别的页签会<strong>自动停用</strong>：热键是全局资源，本程序不常驻占用。
        </p>

        {hotkeyError && <p className="notice notice-error">{hotkeyError}</p>}
      </div>
    </div>
  );
}
