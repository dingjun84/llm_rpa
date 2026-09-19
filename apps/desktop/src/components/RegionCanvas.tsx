import { useRef, type PointerEvent as ReactPointerEvent } from "react";

/**
 * 相对目标窗口的比例矩形 `[x, y, 宽, 高]`，四个分量都在 0–1。
 *
 * 一律用比例而不是像素：窗口挪动、换分辨率都不用重标。
 * 预览图是**等比**缩过的，所以预览图上的相对坐标就是窗口的相对坐标——
 * 拖框不需要任何换算，这也是这个组件能保持简单的原因。
 */
export type Rect4 = [number, number, number, number];

export interface CanvasBox {
  key: string;
  rect: Rect4;
  color: string;
  /**
   * 框左上角那个角标上的字（清单编号，`1.3` 这种）。
   *
   * 由调用方算好传进来，**不在这里按数组下标现算**：编号的权威定义在
   * 后端的标定清单里（`calibration::ITEMS` 的顺序），画布再算一遍就是第二个判据，
   * 而两处不一致的表现只是"角标上的号跟列表里对不上"——很难被当成 bug 报出来。
   */
  badge: string;
  label: string;
}

interface Props {
  /** `data:image/png;base64,...` 的窗口预览图。 */
  image: string;
  /**
   * 预览图的像素尺寸。
   *
   * **只用来定「多小算抓不住」**，不参与比例换算——比例是画布宽高比算出来的。
   * 拿它当换算依据的话，窗口一改尺寸这套就错了。
   */
  width: number;
  height: number;
  boxes: CanvasBox[];
  /** 当前正在标的那一项。只有它会显示手柄，也只有它接受「空白处拖出新框」。 */
  activeKey: string | null;
  /** 拖动过程中持续回调（比例坐标）。拖动结束也会再调一次。 */
  onChange: (key: string, rect: Rect4) => void;
  /**
   * 一次拖动**结束**时回调（松手那一刻）。
   *
   * 为什么与 `onChange` 分开：`onChange` 每帧都触发，拿它做事就得自己防抖；
   * 而"校验这个框"是每次拖动只要做一次的重活（会走一次 IPC）。
   * 分开之后调用方不必猜"这一次是中间帧还是最终值"。
   */
  onCommit?: (key: string, rect: Rect4) => void;
  /** 点了一下别的框——面板据此切换当前项。 */
  onSelect?: (key: string) => void;
  disabled?: boolean;
}

/** 八个缩放手柄。字符串里的方向字母就是它拖动的方向。 */
const HANDLES = ["nw", "n", "ne", "e", "se", "s", "sw", "w"] as const;
type Handle = (typeof HANDLES)[number];

/**
 * 拖框时的最小可抓取尺寸（预览图像素）。
 *
 * 不写成固定的比例：写 0.01 的话，窗口一窄框就细得抓不住，
 * 一宽又显得松。按像素定，换分辨率、换窗口大小手感都一样。
 */
const MIN_GRAB_PX = 6;

type Drag =
  | { mode: "move"; key: string; origin: Rect4; startX: number; startY: number }
  | {
      mode: "resize";
      key: string;
      handle: Handle;
      origin: Rect4;
      startX: number;
      startY: number;
    }
  | { mode: "create"; key: string; startX: number; startY: number };

const clamp01 = (value: number) => Math.min(1, Math.max(0, value));

/** 两个端点之间规范化出一个矩形（允许从右下往左上拖）。 */
function boxFrom(
  a: { x: number; y: number },
  b: { x: number; y: number },
  minX: number,
  minY: number,
): Rect4 {
  const x = Math.min(a.x, b.x);
  const y = Math.min(a.y, b.y);
  const width = Math.max(Math.abs(a.x - b.x), minX);
  const height = Math.max(Math.abs(a.y - b.y), minY);
  return [clamp01(Math.min(x, 1 - width)), clamp01(Math.min(y, 1 - height)), width, height];
}

/**
 * 按手柄方向缩放。
 *
 * `dx` / `dy` 是**指针相对按下点的位移**，不是"指针到矩形中心的距离"——
 * 后者把矩形自身的半宽也算进去了，手柄一碰就跳一大截。
 *
 * 四条边**各自**夹在窗口内、且不小于最小尺寸：夹完再算另一条边，
 * 否则会出现「往左拖过界，框整体翻到窗口外面」这种看起来像 bug 的现象。
 */
function resized(
  origin: Rect4,
  handle: Handle,
  dx: number,
  dy: number,
  minX: number,
  minY: number,
): Rect4 {
  let [x, y, width, height] = origin;
  const right = x + width;
  const bottom = y + height;

  if (handle.includes("w")) {
    x = clamp01(Math.min(x + dx, right - minX));
    width = right - x;
  }
  if (handle.includes("e")) {
    width = Math.min(Math.max(width + dx, minX), 1 - x);
  }
  if (handle.includes("n")) {
    y = clamp01(Math.min(y + dy, bottom - minY));
    height = bottom - y;
  }
  if (handle.includes("s")) {
    height = Math.min(Math.max(height + dy, minY), 1 - y);
  }
  return [x, y, width, height];
}

/**
 * 可拖拽的区域画布。
 *
 * ## 三种拖动
 *
 * - 拖**框体** → 整块移动；
 * - 拖**手柄** → 单向/双向缩放；
 * - 在**空白处**拖 → 给当前选中的那一项拉出一个新框。
 *
 * ## 为什么坐标全是比例
 *
 * 画布里的位置用百分比（`left: 33%`），指针位置按 `getBoundingClientRect`
 * 换算成比例。中间不经过任何像素——预览图是等比缩放的，比例就是窗口比例。
 * 一旦引入像素做中转，就得同时维护"预览缩放比"和"窗口尺寸"两套数，
 * 而它们不一致时框会**看起来很正常**地落在别处。
 *
 * ## 为什么要 pointer capture
 *
 * 拖出画布边界、或者拖到另一个元素上时，指针事件会跑掉，
 * 于是"松手"收不到、框卡在拖动中间状态。capture 之后事件一律回到这里。
 */
export function RegionCanvas({
  image,
  width,
  height,
  boxes,
  activeKey,
  onChange,
  onCommit,
  onSelect,
  disabled = false,
}: Props) {
  const stageRef = useRef<HTMLDivElement | null>(null);
  const dragRef = useRef<Drag | null>(null);
  /** 本次拖动最后算出的框。松手时用它去 `onCommit`——见 `onPointerMove` 里的说明。 */
  const lastRectRef = useRef<{ key: string; rect: Rect4 } | null>(null);

  /** 最小尺寸换算成比例。按像素定，所以不同窗口尺寸下手感一致。 */
  const minSpan = {
    x: width > 0 ? MIN_GRAB_PX / width : 0.005,
    y: height > 0 ? MIN_GRAB_PX / height : 0.005,
  };

  const ratioAt = (event: ReactPointerEvent<HTMLElement>) => {
    const el = stageRef.current;
    if (!el) return null;
    const rect = el.getBoundingClientRect();
    if (rect.width === 0 || rect.height === 0) return null;
    return {
      x: clamp01((event.clientX - rect.left) / rect.width),
      y: clamp01((event.clientY - rect.top) / rect.height),
    };
  };

  const begin = (event: ReactPointerEvent<HTMLElement>, drag: Drag) => {
    if (disabled || event.button !== 0) return;
    event.stopPropagation();
    event.currentTarget.setPointerCapture(event.pointerId);
    dragRef.current = drag;
  };

  const startMove = (event: ReactPointerEvent<HTMLElement>, box: CanvasBox) => {
    onSelect?.(box.key);
    const point = ratioAt(event);
    if (!point) return;
    begin(event, {
      mode: "move",
      key: box.key,
      origin: box.rect,
      startX: point.x,
      startY: point.y,
    });
  };

  const startResize = (
    event: ReactPointerEvent<HTMLElement>,
    box: CanvasBox,
    handle: Handle,
  ) => {
    const point = ratioAt(event);
    if (!point) return;
    begin(event, {
      mode: "resize",
      key: box.key,
      handle,
      origin: box.rect,
      startX: point.x,
      startY: point.y,
    });
  };

  /** 空白处拖动 = 给当前项拉一个新框。没有选中项就什么也不做。 */
  const startCreate = (event: ReactPointerEvent<HTMLElement>) => {
    if (!activeKey) return;
    const point = ratioAt(event);
    if (!point) return;
    begin(event, { mode: "create", key: activeKey, startX: point.x, startY: point.y });
  };

  const onPointerMove = (event: ReactPointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    if (!drag) return;
    const point = ratioAt(event);
    if (!point) return;

    // 先把这次的结果算出来、存进 `lastRectRef`，再交给 `onChange`。
    // 存的原因是 `onPointerUp` 要拿到**最终值**去校验，而松手时
    // 指针位置已经没意义了（可能已移出画布）。重算一遍会得到错的框。
    let next: Rect4;

    if (drag.mode === "move") {
      const dx = point.x - drag.startX;
      const dy = point.y - drag.startY;
      const [x, y, w, h] = drag.origin;
      next = [clamp01(Math.min(x + dx, 1 - w)), clamp01(Math.min(y + dy, 1 - h)), w, h];
    } else if (drag.mode === "resize") {
      next = resized(
        drag.origin,
        drag.handle,
        point.x - drag.startX,
        point.y - drag.startY,
        minSpan.x,
        minSpan.y,
      );
    } else {
      next = boxFrom({ x: drag.startX, y: drag.startY }, point, minSpan.x, minSpan.y);
    }

    lastRectRef.current = { key: drag.key, rect: next };
    onChange(drag.key, next);
  };

  const onPointerUp = () => {
    const drag = dragRef.current;
    const last = lastRectRef.current;
    dragRef.current = null;
    lastRectRef.current = null;
    // 只有真的动过才提交。`startMove` 里 `onSelect` 已经先跑过，
    // 原地按一下不产生拖动——那种情况不该触发一次校验。
    if (drag && last && last.key === drag.key) {
      onCommit?.(last.key, last.rect);
    }
  };

  return (
    <div
      className="region-canvas"
      ref={stageRef}
      onPointerDown={startCreate}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={onPointerUp}
    >
      <img src={image} alt="目标窗口预览" draggable={false} />

      {boxes.map((box) => {
        const [x, y, w, h] = box.rect;
        const isActive = box.key === activeKey;
        return (
          <div
            key={box.key}
            className={isActive ? "region-box is-active" : "region-box"}
            style={{
              left: `${x * 100}%`,
              top: `${y * 100}%`,
              width: `${w * 100}%`,
              height: `${h * 100}%`,
              borderColor: box.color,
              background: `${box.color}22`,
            }}
            onPointerDown={(event) => startMove(event, box)}
          >
            <span className="region-box-label" style={{ background: box.color }}>
              {box.badge} {box.label}
            </span>
            {isActive &&
              !disabled &&
              HANDLES.map((handle) => (
                <span
                  key={handle}
                  className={`region-handle is-${handle}`}
                  onPointerDown={(event) => startResize(event, box, handle)}
                />
              ))}
          </div>
        );
      })}
    </div>
  );
}
