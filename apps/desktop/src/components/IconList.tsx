import { useState } from "react";

import { normalizeName } from "../iconNames";
import type { IconEntry, IconVariant } from "../types";

interface Props {
  entries: IconEntry[];
  /** 配置里「用于联系人导航」那组名字（原始写法）。 */
  configuredNames: string[];
  /** 配置里引用了、但图标库里已经没有的名字（被删了，或者图标库目录被改过）。 */
  dangling: string[];
  disabled: boolean;
  /** 目标窗口类名填了没有：没填就没法匹配，那几个按钮要禁用。 */
  windowReady: boolean;
  /** 正在匹配的那一组（图标名，或「全部已选图标」）；`null` = 没在跑。 */
  probing: string | null;
  /** 正在点击的那一组；`null` = 没在跑。 */
  clicking: string | null;
  onToggleUse: (entry: IconEntry, on: boolean) => void;
  onProbe: (names: string[], label: string) => void;
  onClick: (names: string[], label: string) => void;
  onRemove: (entry: IconEntry) => void;
  onRemoveVariant: (entry: IconEntry, variant: IconVariant) => void;
}

/**
 * 图标库列表：一行一个图标，那一行下面排着它的全部变体。
 *
 * ## 为什么单独成文件
 *
 * 它是「图标库」页最长的一段，而且**自带两个只有它用的状态**：
 * 「定位并点击」与「删除整个图标」的**两下确认**（`armed` / `armDelete`）。
 * 这两个状态是纯粹的交互细节——主面板不需要知道"现在哪个按钮上了膛"，
 * 它只关心"用户确认了，去点/去删"。搬到这里以后主面板少了一份噪声状态。
 *
 * ★ **名字比较走 [`normalizeName`]**（`Chat` 与 `chat` 是同一个图标），
 * 别在这里写 `===` 直接比。
 */
export function IconList({
  entries,
  configuredNames,
  dangling,
  disabled,
  windowReady,
  probing,
  clicking,
  onToggleUse,
  onProbe,
  onClick,
  onRemove,
  onRemoveVariant,
}: Props) {
  /** 已经"上了膛"的那张图标：点一次变成「再点一次确认」，点第二次才真的点。 */
  const [armed, setArmed] = useState<string | null>(null);
  /**
   * 已经"上了膛"的**删除整组**按钮。
   *
   * 删一整组图标会连带它底下全部变体一起走，而每一张都是人对着屏幕框出来的——
   * 攒一组不容易，所以它要两下。单张的「删除这张」不要二次确认：那一下只丢一张，
   * 重截一张的成本很低，加确认反而让人每次都要点两遍。
   */
  const [armDelete, setArmDelete] = useState<string | null>(null);

  return (
    <>
      <ul className="icon-list">
        {entries.map((entry) => {
          const key = normalizeName(entry.name);
          const usedForContact = configuredNames.some(
            (name) => normalizeName(name) === key,
          );
          const broken = !entry.usable;
          const isArmed = armed === entry.name;
          const isArmedDelete = armDelete === entry.name;
          const sizes = entry.variants
            .filter((variant) => variant.problem === null)
            .map((variant) => `${variant.width}×${variant.height}`);
          const distinct = Array.from(new Set(sizes));
          return (
            <li key={entry.name} className={broken ? "icon-row is-broken" : "icon-row"}>
              <div className="icon-meta">
                <span className="icon-name">{entry.name}</span>
                <span className="field-hint">
                  {entry.variants.length} 张
                  {distinct.length === 1 ? ` · ${distinct[0]}` : ""}
                  {broken ? " · 有读不出来的" : ""}
                </span>
              </div>

              <ul className="icon-variants">
                {entry.variants.map((variant) => (
                  <li
                    key={variant.relative}
                    className={variant.problem ? "icon-variant is-broken" : "icon-variant"}
                  >
                    {variant.image ? (
                      <img
                        className="icon-thumb"
                        src={variant.image}
                        alt={variant.relative}
                        title={variant.relative}
                      />
                    ) : (
                      <span className="icon-thumb icon-thumb-empty">?</span>
                    )}
                    <span className="icon-variant-meta">
                      {variant.problem ? "读不出来" : `${variant.width}×${variant.height}`}
                    </span>
                    <button
                      type="button"
                      className="ghost icon-variant-remove"
                      disabled={disabled}
                      title={`删除 ${variant.relative}（同一个名字下的其他图不动）`}
                      onClick={() => void onRemoveVariant(entry, variant)}
                    >
                      删除这张
                    </button>
                    {variant.problem && (
                      <span className="icon-variant-problem">
                        这张现在不能当模板：{variant.problem}
                      </span>
                    )}
                  </li>
                ))}
              </ul>

              <div className="icon-actions">
                {/* 这一个勾选框回答的是**"联系人视图"是哪个图标**——
                    查找式任务在开始之前会先点它一下把视图切过去。
                    它是一份**配置**（这台机器上的固定事实），不是"本次要点哪个"：
                    后者在「任务」页的「要点哪一个图标」里选，那儿列的是图标库本身。
                    勾错了不会报错，只会拿另一个图标的模板去匹配然后转人工，
                    所以配错的那一组是**空的**时，装配期会直接拒绝并说清是哪一组。 */}
                <label className="field-check icon-use">
                  <input
                    type="checkbox"
                    checked={usedForContact}
                    disabled={disabled || broken}
                    onChange={(event) => onToggleUse(entry, event.target.checked)}
                  />
                  <span>用于联系人导航</span>
                </label>
                <button
                  type="button"
                  disabled={disabled || broken || !windowReady || probing !== null}
                  title={`把「${entry.name}」的 ${entry.variants.length} 张图都拿去匹配一遍`}
                  onClick={() => onProbe([entry.name], entry.name)}
                >
                  {probing === entry.name ? "匹配中…" : "测试匹配"}
                </button>
                <button
                  type="button"
                  className={isArmed ? "danger" : undefined}
                  disabled={disabled || broken || !windowReady || clicking !== null}
                  title="会真的在客户端窗口上点一下鼠标"
                  onClick={() => {
                    if (isArmed) {
                      setArmed(null);
                      void onClick([entry.name], entry.name);
                    } else {
                      setArmed(entry.name);
                    }
                  }}
                >
                  {clicking === entry.name
                    ? "点击中…"
                    : isArmed
                      ? "再点一次＝真的点下去"
                      : "定位并点击"}
                </button>
                <button
                  type="button"
                  className={isArmedDelete ? "danger" : "ghost"}
                  disabled={disabled}
                  title={`删除「${entry.name}」以及它底下的 ${entry.variants.length} 张图`}
                  onClick={() => {
                    if (isArmedDelete) {
                      setArmDelete(null);
                      void onRemove(entry);
                    } else {
                      setArmDelete(entry.name);
                    }
                  }}
                >
                  {isArmedDelete
                    ? `再点一次＝删掉全部 ${entry.variants.length} 张`
                    : "删除整个图标"}
                </button>
              </div>
            </li>
          );
        })}
      </ul>

      {dangling.length > 0 && (
        <p className="notice notice-warn">
          配置里引用了 {dangling.length} 个<strong>不在图标库里</strong>的图标
          （{dangling.join("、")}——被删了，或者图标库目录被改过）。
          任务装配时会直接报错，建议在这里重新勾选，或者把它们从配置里去掉。
        </p>
      )}
    </>
  );
}
