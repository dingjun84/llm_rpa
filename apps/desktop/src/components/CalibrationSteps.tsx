import type { CalibrationPlan } from "../types";

/**
 * 编号色板。
 *
 * 颜色是照"叠在截图上还能看清"挑的，不是 UI 主题色。
 * 取色按**场景内**的序号（见 [`buildItemLook`]）：同一张截图上最多同时出现 6 个框，
 * 场景内不重复就够了；全局排下去反而会在同一个场景里撞色，
 * 而撞色的那两个框恰恰是最需要分清的。
 */
const PALETTE = [
  "#e24b4a",
  "#378add",
  "#639922",
  "#ef9f27",
  "#9b59b6",
  "#16a085",
  "#d35400",
  "#34495e",
  "#c2185b",
];

/** 一项在界面上怎么标：清单编号 + 颜色。 */
export interface ItemLook {
  /** 清单编号（`1.3`）。⚠️ **不是**探针编号——见 `types.ts` 的 `probe_index`。 */
  number: string;
  color: string;
}

/**
 * 算每一项的清单编号与颜色。
 *
 * 编号按 `场景序号.项序号`，与用户描述清单时的写法、以及 `docs/` 里的一致。
 *
 * 只算这一次，左栏与画布**共用同一份**。两边各算一次的话，迟早会出现
 * 「列表里是 2.4、画布角标是 2.5」这种对不上的情况——而它看起来只像是自己看错了，
 * 根本不会被当成 bug 报出来。
 */
export function buildItemLook(plan: CalibrationPlan | null): Map<string, ItemLook> {
  const map = new Map<string, ItemLook>();
  (plan?.scenes ?? []).forEach((scene, sceneIndex) => {
    scene.items.forEach((item, itemIndex) => {
      map.set(item.key, {
        number: `${sceneIndex + 1}.${itemIndex + 1}`,
        color: PALETTE[itemIndex % PALETTE.length],
      });
    });
  });
  return map;
}

interface Props {
  plan: CalibrationPlan;
  lookByKey: Map<string, ItemLook>;
  /** 当前场景。`null` 表示还没读到计划。 */
  sceneId: string | null;
  /** 当前正在标的那一项。 */
  activeKey: string | null;
  /**
   * 已经标好的 key，用来显示「已标定 / 未标定」。
   *
   * 传一份**算好的集合**而不是 `rectOf` 函数：状态必须按**草稿**算，
   * 而"怎么从草稿里读一项"是标定页的事（`regions` 与 `area_marks` 两套存法）。
   * 把它当函数传进来，等于让这个纯展示组件也去懂存储结构。
   */
  markedKeys: Set<string>;
  /**
   * 点了某一项。`key` 为 `null` 表示**只切场景**——
   * 那种情况下当前项由调用方按自己的规则定（它会优先落在第一个还没标的项上）。
   */
  onPick: (sceneId: string, key: string | null) => void;
}

/**
 * 标定页的左栏：场景 + 每个场景下的标定项。
 *
 * 抽出来是因为标定页本体已经压着不少事（截图、画布、草稿、失效项清理），
 * 而这一栏是**纯展示**：给它计划、编号颜色、已标集合和回调，它就画得出来。
 */
export function CalibrationSteps({
  plan,
  lookByKey,
  sceneId,
  activeKey,
  markedKeys,
  onPick,
}: Props) {
  return (
    <aside className="guide-steps">
      {plan.scenes.map((scene) => {
        const done = scene.items.filter((item) => markedKeys.has(item.key)).length;
        return (
          <div key={scene.id} className="guide-scene">
            <button
              type="button"
              className={scene.id === sceneId ? "guide-scene-head is-active" : "guide-scene-head"}
              onClick={() => onPick(scene.id, null)}
            >
              <strong>{scene.label}</strong>
              <span className="guide-scene-count">
                {done}/{scene.items.length}
              </span>
            </button>
            <ul className="guide-item-list">
              {scene.items.map((item) => {
                const look = lookByKey.get(item.key);
                const done = markedKeys.has(item.key);
                return (
                  <li key={item.key}>
                    <button
                      type="button"
                      className={item.key === activeKey ? "guide-item is-active" : "guide-item"}
                      onClick={() => onPick(scene.id, item.key)}
                    >
                      <span className="guide-swatch" style={{ background: look?.color }}>
                        {look?.number}
                      </span>
                      <span className="guide-item-label">{item.label}</span>
                      <span className={done ? "guide-state is-done" : "guide-state"}>
                        {done ? "已标定" : "未标定"}
                      </span>
                    </button>
                  </li>
                );
              })}
            </ul>
          </div>
        );
      })}
    </aside>
  );
}
