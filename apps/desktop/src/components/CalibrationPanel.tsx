import { useCallback, useEffect, useMemo, useState } from "react";

import "../calibration.css";
import {
  listCalibrationPlan,
  previewTargetWindow,
  pruneStaleMarks,
  validateAreaMark,
} from "../api";
import type {
  CalibrationItem,
  CalibrationPlan,
  RuntimeConfig,
  RuntimeInfo,
  WindowPreview,
} from "../types";
import { DEFAULT_REGIONS } from "./RegionCalibration";
import { buildItemLook, CalibrationSteps } from "./CalibrationSteps";
import { CaptureTrigger } from "./CaptureTrigger";
import { RegionCanvas, type CanvasBox, type Rect4 } from "./RegionCanvas";
import { StaleMarksNotice } from "./StaleMarksNotice";

interface Props {
  info: RuntimeInfo;
  draft: RuntimeConfig;
  onPatch: (next: Partial<RuntimeConfig>) => void;
  onSave: () => void;
  dirty: boolean;
  canSave: boolean;
  busy: boolean;
  previews: Record<string, CachedPreview>;
  onCachePreview: (sceneId: string, entry: CachedPreview) => void;
  /**
   * 当前正在标哪个场景、哪一项。
   *
   * ★ 这两个值**由 `App` 持有**，不放在本组件里：切到「任务」或「图标库」
   * 页签时本组件会被卸载，自己的 state 全没了。用户的实际动作是
   * 「标完主界面 → 去别的页签看一眼 → 回来接着标历史对话」——
   * 场景被打回第一个的话，他会发现"刚才那张截图不见了"，
   * 而其实图还在缓存里，只是界面回到了别的场景。
   *
   * `sceneId` 为 `null` = 还没定，读到计划后由本组件落到第一个场景上。
   */
  sceneId: string | null;
  onSceneId: (id: string | null) => void;
  activeKey: string | null;
  onActiveKey: (key: string | null) => void;
}

/**
 * 一张截图的缓存项。
 *
 * ## 为什么要缓存
 *
 * 截图是**运行时**状态，本来活不过一次页面切换。但用户的实际动作是
 * 「把这个界面截好、框好 → 去下一个界面 → 回来改一处」——
 * 让他回来重截，代价是重新把客户端摆成那个样子，非常高。
 * 所以缓存按**场景**存：同一个界面的若干标定项共用一张图。
 *
 * ## 为什么**不落盘**
 *
 * 截图里有联系人姓名和聊天内容。项目的硬约束是「消息正文不落库」，
 * 把画面存到磁盘等于绕开它。所以这份缓存**只活在进程内**，关掉程序就没了
 * —— 这是有意的取舍，不是偷懒。
 *
 * ## 为什么带上窗口标识
 *
 * 缓存的是「**某个窗口**的某一屏」。窗口类名或目标程序换了，这张图就不是
 * 同一个窗口的了，不能拿来框。标识存在缓存项里、在**读取处**校验，
 * 而不是靠一个 effect 去清空——后者要回答「什么时候该清」，
 * 而前者只要回答「这条能不能用」。
 */
export interface CachedPreview {
  preview: WindowPreview;
  windowClass: string;
  /** 跟 `RuntimeConfig.wecom_exe` 一样是可空的——没填就是「按类名找」。 */
  wecomExe: string | null;
  capturedAt: number;
}

/** 给人看的时间：`09-19 04:52:11`。不依赖 locale，免得换个环境显示成别的样子。 */
function clockText(ms: number): string {
  const at = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, "0");
  return (
    `${pad(at.getMonth() + 1)}-${pad(at.getDate())} ` +
    `${pad(at.getHours())}:${pad(at.getMinutes())}:${pad(at.getSeconds())}`
  );
}

/**
 * 一项写进 `regions` 时该用的**字段名**。
 *
 * ★★ 必须用后端下发的 `region_field`，**不能用 `item.key`** ——
 * 「列表区」的 key 是 `list_area`，存的却是 `regions.contact_panel`。
 * 拿 key 当字段名的话，框会写进一个配置里不存在的键，后端 serde
 * 反序列化时**静默丢掉**：不报错、不警告，症状只是
 * 「标完、保存，再打开就没了」。
 *
 * 返回 `null` = 后端没下发字段名。**不退回 `key`**：那是个猜出来的字段名，
 * 猜错的代价正是上面那种静默丢失，而"不确定即失败"是这个项目的底线。
 */
function regionsField(item: CalibrationItem): string | null {
  return item.region_field;
}

/**
 * 「界面标定」页。
 *
 * ## 为什么单独一页、而且要按界面分组
 *
 * 一块区域只有在它所在的界面**显示出来**时才能标——搜索框在下拉面板没打开时
 * 根本不在画面上。所以流程只能是「切到某个界面 → 截一张图 → 在这张图上
 * 框出这个界面的全部区域」，而引导语必须说清"现在应该看得见什么"。
 *
 * ## 为什么清单来自后端
 *
 * 标定项的权威定义在 `calibration::ITEMS`，**「哪一项存哪个配置字段」也在那里**。
 * 前端另写一份的话，迟早会出现「界面上有这一项、任务里却读不到」——
 * 而这种错位**不报任何错**，只表现为某个框永远不起作用。
 *
 * ## 框的值以**草稿**为准，不以计划为准
 *
 * 计划是"打开这一页时后端里的值"，拖框之后就不准了。所以清单与说明用计划，
 * 画布与进度一律从 `draft` 现读——两处都当权威的话，拖完框界面会跳回去。
 */
export function CalibrationPanel({
  info,
  draft,
  onPatch,
  onSave,
  dirty,
  canSave,
  busy,
  previews,
  onCachePreview,
  sceneId,
  onSceneId,
  activeKey,
  onActiveKey,
}: Props) {
  const [plan, setPlan] = useState<CalibrationPlan | null>(null);
  const [capturing, setCapturing] = useState(false);
  const [cleaning, setCleaning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const windowClass = draft.window_class.trim();
  const windowReady = windowClass !== "";

  const refresh = useCallback(async () => {
    try {
      const next = await listCalibrationPlan();
      setPlan(next);
    } catch (err) {
      setError(String(err));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  /**
   * 读到计划后，把场景定下来。
   *
   * 两种情况都落回第一个场景：**还没选过**（`null`），以及选的那个
   * **在新计划里已经不存在**（清单改过之后会这样）。后者不兜住的话，
   * `scene` 会一直是 `null`，界面永远停在「正在读取标定计划…」。
   */
  useEffect(() => {
    if (!plan) return;
    if (plan.scenes.some((entry) => entry.id === sceneId)) return;
    onSceneId(plan.scenes[0]?.id ?? null);
  }, [plan, sceneId, onSceneId]);

  const scene = useMemo(
    () => plan?.scenes.find((item) => item.id === sceneId) ?? null,
    [plan, sceneId],
  );

  const itemByKey = useMemo(() => {
    const map = new Map<string, CalibrationItem>();
    for (const entry of plan?.scenes ?? []) {
      for (const item of entry.items) map.set(item.key, item);
    }
    return map;
  }, [plan]);

  /**
   * 清单编号：`场景序号.项序号`（`1.1`、`2.3`）。
   *
   * 与用户描述清单时的写法、以及 `docs/` 里的一致。
   * ⚠️ 这**不是**探针编号——那是 `screen_probe annotate` 画在截图上的 `1`~`4`，
   * 由后端按 `CoreRegion` 的顺序给出（`CalibrationItem.probe_index`）。
   * 两套编号并存，界面上都显示：拿截图沟通时说的"3"指的是探针编号。
   */
  /** 每一项的清单编号与颜色。左栏与画布**共用这一份**（见 `buildItemLook`）。 */
  const lookByKey = useMemo(() => buildItemLook(plan), [plan]);

  /** 配置里存着、但清单里已经没有的标定项。见 `pruneStale`。 */
  const staleKeys = plan?.stale_keys ?? [];

  /**
   * 读一项当前的坐标。
   *
   * `regions` 里那四个是**具名字段**（编排层直接按名字读），只能索引访问，
   * 而且**必须按 `region_field` 索引**——键不是 `key`（见 `regionsField`）。
   * `area_marks` 是字典，键就是标定项的 key。
   *
   * 拿不到字段名时返回 `null`（显示成未标定）而不是退回 `key`：
   * 退回等于用一个猜出来的键去读，读不到就当"没标过"，用户会以为是自己的问题。
   * 写不进去的提示在 `applyRect` 里给——那里才是用户真正动手的地方。
   */
  const rectOf = useCallback(
    (item: CalibrationItem): Rect4 | null => {
      if (item.storage === "regions") {
        const field = regionsField(item);
        if (field === null) return null;
        const fields = draft.regions as unknown as Record<string, Rect4 | undefined>;
        return fields[field] ?? null;
      }
      return (draft.area_marks ?? {})[item.key]?.rect ?? null;
    },
    [draft],
  );

  /**
   * 已经标好的 key。
   *
   * 按**草稿**算（`rectOf` 读的是 `draft`），不按计划——计划是打开这一页
   * 那一刻的快照，拖完框就过期了。左栏只收这一份集合，
   * 不必知道 `regions` 与 `area_marks` 两套存法。
   */
  const markedKeys = useMemo(() => {
    const set = new Set<string>();
    for (const entry of plan?.scenes ?? []) {
      for (const item of entry.items) {
        if (rectOf(item) !== null) set.add(item.key);
      }
    }
    return set;
  }, [plan, rectOf]);

  /** 左栏点了某一项：`key` 为 `null` = 只切场景，当前项由下面那个 effect 定。 */
  const pick = useCallback(
    (nextSceneId: string, key: string | null) => {
      onSceneId(nextSceneId);
      if (key !== null) onActiveKey(key);
    },
    [onSceneId, onActiveKey],
  );

  // 切场景时清掉上一屏留下的校验提示（它说的是上一个界面的事）。
  // ★ 截图**不在这里清** —— 它按场景缓存着，切回来还要用，见 `preview`。
  useEffect(() => {
    setNotice(null);
  }, [sceneId]);

  /**
   * 当前场景能用的截图。
   *
   * 缓存里那条必须**属于同一个窗口**才算数：窗口类名或目标程序一改，
   * 截的就不是同一个窗口了，继续挂着会让人对着 A 窗口的图调 B 窗口的框。
   * 判据放在这里（**读取处**），而不是靠一个 effect 去清缓存——
   * 后者要回答「什么时候该清」，这里只要回答「这条能不能用」。
   */
  const cached = sceneId ? previews[sceneId] ?? null : null;
  const preview =
    cached && cached.windowClass === draft.window_class && cached.wecomExe === draft.wecom_exe
      ? cached.preview
      : null;
  const capturedAt = preview && cached ? cached.capturedAt : null;

  /**
   * 「这个界面明明截过，图却没了」——把原因说出来。
   *
   * 缓存是按窗口存活的：窗口类名或目标程序一改，旧图截的就是**别的窗口**，
   * 继续挂着会让人对着 A 窗口的图调 B 窗口的框，所以判据不认它。
   * 但**不能就这么悄悄消失**——用户会以为是程序把图弄丢了，
   * 然后一遍遍重截。这里把"哪张图、为什么不认"讲清楚。
   */
  const droppedShot =
    cached !== null &&
    (cached.windowClass !== draft.window_class || cached.wecomExe !== draft.wecom_exe)
      ? cached
      : null;

  /**
   * 每个界面的截图状态：`capturedAt` 为 `null` = 这个界面这次运行还没有可用的截图。
   *
   * 这一条回答的是用户反复问的那个问题——「我切过去怎么没有截图」。
   * 把它摆出来之后：哪个界面有图、截于什么时候、哪个还没有，一眼可见，
   * 不必靠猜，也不必一个个点过去看。**这就是唯一的一份判据**，
   * 下面的 `capturedScenes` 与状态条都从它派生。
   */
  const shotByScene = useMemo(
    () =>
      (plan?.scenes ?? []).map((entry) => {
        const shot = previews[entry.id];
        const usable =
          shot !== undefined &&
          shot.windowClass === draft.window_class &&
          shot.wecomExe === draft.wecom_exe;
        return {
          id: entry.id,
          label: entry.label,
          capturedAt: usable && shot ? shot.capturedAt : null,
        };
      }),
    [plan, previews, draft.window_class, draft.wecom_exe],
  );

  /**
   * 已经截过图的场景名（只算**当前窗口**那几张）。
   *
   * 用来回答「我切到别的界面，原来的截图呢」——图没丢，它按场景存着，
   * 只是当前这个界面还没截。不说的话用户会以为缓存整个失效了，
   * 然后重新截一遍所有界面。
   */
  const capturedScenes = useMemo(
    () => shotByScene.filter((shot) => shot.capturedAt !== null).map((shot) => shot.label),
    [shotByScene],
  );

  // 切到某个界面时，默认落在**第一个还没标的项**上——引导的意思就是
  // "下一步该干这个"，让人自己去列表里找是没必要的负担。
  //
  // ★ 已经选中的项**不换**：这个 effect 依赖 `rectOf`（进而依赖草稿），
  // 每拖一下框都会重跑一次。不加这层判断的话，拖完一放手当前项就跳到下一项去了。
  //
  // ★ 用"提前 return"而不是 `setActiveKey(current => …)`：当前项现在由 `App`
  // 持有，这里只拿得到值、拿不到函数式更新。好在语义一样——
  // 选中项仍属于本场景时什么都不做，所以拖框不会把它顶掉。
  useEffect(() => {
    if (!scene) return;
    if (activeKey && scene.items.some((item) => item.key === activeKey)) return;
    const pending = scene.items.find((item) => rectOf(item) === null);
    onActiveKey((pending ?? scene.items[0])?.key ?? null);
  }, [scene, rectOf, activeKey, onActiveKey]);

  const boxes: CanvasBox[] = useMemo(() => {
    if (!scene) return [];
    return scene.items.flatMap((item) => {
      const rect = rectOf(item);
      if (!rect) return [];
      const look = lookByKey.get(item.key);
      return [
        {
          key: item.key,
          rect,
          badge: look?.number ?? "",
          label: item.label,
          color: look?.color ?? "",
        },
      ];
    });
  }, [scene, rectOf, lookByKey]);

  const marked = useMemo(() => {
    if (!plan) return { done: 0, total: 0 };
    const items = plan.scenes.flatMap((entry) => entry.items);
    return {
      done: items.filter((item) => markedKeys.has(item.key)).length,
      total: items.length,
    };
  }, [plan, markedKeys]);

  const activeItem = activeKey ? itemByKey.get(activeKey) ?? null : null;

  const capture = async () => {
    if (!sceneId) return;
    setCapturing(true);
    setError(null);
    setNotice(null);
    try {
      const next = await previewTargetWindow(draft.window_class, draft.wecom_exe);
      onCachePreview(sceneId, {
        preview: next,
        windowClass: draft.window_class,
        wecomExe: draft.wecom_exe,
        capturedAt: Date.now(),
      });
    } catch (err) {
      // 截图失败**不动缓存**：那张图本身没出错，错的是这一次截图动作
      // （窗口没找到、被最小化……）。把它删掉只会让用户再白丢一次已经框好的参考。
      setError(String(err));
    } finally {
      setCapturing(false);
    }
  };

  /**
   * 把拖出来的框写进配置草稿。
   *
   * 存**比例**：预览图是等比缩放的，所以预览图上的相对坐标就是窗口的相对坐标，
   * 中间不需要任何像素换算。
   *
   * ★ `regions` 那几项**按 `region_field` 写，不按 `key`**：
   * 「列表区」的 key 是 `list_area`、字段却是 `contact_panel`。
   * 用 key 写进去的键在配置里不存在，后端 serde 反序列化时**静默丢掉**——
   * 用户看到的就是「标完、保存，再打开就没了」。
   */
  const applyRect = (key: string, rect: Rect4) => {
    const item = itemByKey.get(key);
    if (!item) return;

    if (item.storage === "regions") {
      const field = regionsField(item);
      if (field === null) {
        // 后端没下发字段名 ⇒ 界面与后端版本对不上。**不退回 `key`**：
        // 那正是会静默丢框的写法（见 `regionsField`）。宁可不写、并说清楚。
        setError(
          `「${item.label}」没拿到 regions 的字段名，这个框存不进去——` +
            "界面与后端版本对不上，请重新构建（先 npm run build，再 cargo build）。",
        );
        return;
      }
      setError(null);
      onPatch({ regions: { ...draft.regions, [field]: rect } });
      return;
    }

    if (!preview) return;
    const marks = draft.area_marks ?? {};
    const previous = marks[key];
    onPatch({
      area_marks: {
        ...marks,
        [key]: {
          rect,
          // 标定时间只在**第一次**落下来时记。每拖一下就刷新它的话，
          // 这个字段就从"什么时候标的"变成了"最后一次动过"。
          calibrated_at_ms: previous?.calibrated_at_ms ?? Date.now(),
          // 窗口几何取**这一张预览图**的，而不是当前窗口的实时值——
          // 要回答的是"这个框是相对多大的窗口量的"。
          window: preview.window,
        },
      },
    });
  };

  /**
   * 一次拖动结束后校验这个框。
   *
   * 校验**不改草稿**，只是问服务端"这个框合不合法"——判据（最小尺寸等）
   * 只写在 `automation_core::RelativeRegion::validate` 一处，
   * 界面这边不复制一份。合法就静默通过；不合法只提示，
   * **不把框改回去**——把用户刚拖出来的框弹回原位会让人以为拖动失灵了。
   */
  const commitRect = async (key: string, rect: Rect4) => {
    try {
      await validateAreaMark(key, rect);
      setNotice(null);
    } catch (err) {
      const item = itemByKey.get(key);
      setNotice(`${item?.label ?? key}：${String(err)}`);
    }
  };

  const clearMark = (item: CalibrationItem) => {
    const marks = { ...(draft.area_marks ?? {}) };
    delete marks[item.key];
    onPatch({ area_marks: marks });
  };

  const resetCoreRegions = () => {
    onPatch({ regions: DEFAULT_REGIONS });
    setNotice("四个必填区域已恢复成默认值。默认值是照界面猜的，不是量出来的——请照着截图核对一遍。");
  };

  /**
   * 清掉配置里那些**清单已经没有**的标定项。
   *
   * 两件事都要做，缺一不可：
   *
   * 1. 让**服务端**清掉（`pruneStaleMarks` 直接落盘）——它才是让保存失败的那一份；
   * 2. 让**草稿**也清掉同样的键——只清服务端的话，用户接着点「保存配置」，
   *    草稿里那些键又被原样写回去，报错照旧。
   *
   * 用 `plan.stale_keys` 而不是自己扫一遍草稿：哪几个键算失效由后端定
   * （清单在那边），前端再判一次就是第二个判据。
   */
  const pruneStale = async () => {
    setCleaning(true);
    setError(null);
    try {
      const removed = await pruneStaleMarks();
      const marks = { ...(draft.area_marks ?? {}) };
      for (const key of staleKeys) delete marks[key];
      onPatch({ area_marks: marks });
      setNotice(
        removed > 0
          ? `已清掉 ${removed} 项失效的标定（${staleKeys.join("、")}），配置已经落盘。`
          : "没有需要清理的项。",
      );
      await refresh();
    } catch (err) {
      setError(String(err));
    } finally {
      setCleaning(false);
    }
  };

  return (
    <section className="panel calibration-guide">
      <div className="guide-head">
        <div>
          <h2>界面标定</h2>
          <p className="field-hint">
            照着截图把每个界面里要用到的区域框出来。框的位置存成
            <strong>相对窗口的比例</strong>，所以窗口挪动不用重标；但
            <strong>窗口尺寸改了要重标</strong>——界面元素是固定像素宽的。
            标完记得点「保存配置」，坐标会写进配置文件，供后续编排读取。
          </p>
        </div>
        <div className="guide-head-actions">
          <span className="guide-progress">
            已标定 {marked.done} / {marked.total}
          </span>
          <button type="button" disabled={busy || !canSave} onClick={onSave}>
            保存配置{dirty ? "（有未保存的改动）" : ""}
          </button>
        </div>
      </div>

      {error && <p className="notice notice-error">{error}</p>}
      {notice && <p className="notice">{notice}</p>}

      {/*
        失效项提示。没有它的话用户会卡在「一保存就报错，但界面上找不到那个项」——
        报错说得没错（那个键确实没人读），可没有出口。
      */}
      <StaleMarksNotice
        keys={staleKeys}
        cleaning={cleaning}
        busy={busy}
        onPrune={pruneStale}
      />

      <div className="guide-grid">
        {plan && (
          <CalibrationSteps
            plan={plan}
            lookByKey={lookByKey}
            sceneId={sceneId}
            activeKey={activeKey}
            markedKeys={markedKeys}
            onPick={pick}
          />
        )}

        <div className="guide-stage">
          {scene ? (
            <>
              <h3>{scene.label}</h3>
              <p className="notice">{scene.instruction}</p>

              <CaptureTrigger
                windowReady={windowReady}
                busy={busy}
                capturing={capturing}
                hasPreview={preview !== null}
                onCapture={capture}
              />

              {/*
                「恢复默认」单独一行，不并进上面那排触发按钮：它是把已标好的框**改回去**，
                和"再截一张"不是一类动作。并排摆着容易误点。
              */}
              <div className="guide-stage-actions">
                <button type="button" disabled={busy} onClick={resetCoreRegions}>
                  四个必填区域恢复默认
                </button>
              </div>

              {!windowReady && (
                <p className="notice notice-error">
                  还没填窗口类名——先在「任务」页点「指认窗口」把鼠标停在目标窗口上，
                  或手工填一个类名。截图与任务执行用的是<strong>同一套</strong>定位规则。
                </p>
              )}

              {/*
                本会话的截图状态。用户反复问过「我切过去怎么没有截图」——
                图其实按界面存着，只是当前这个界面还没截。与其让他一个个点过去看，
                不如把四个界面的状态一次摆出来：有图的能一键切回去，没图的写明「还没截」。
              */}
              {shotByScene.length > 0 && (
                <div className="guide-shots">
                  <span className="guide-shots-head">本次运行的截图</span>
                  <ul className="guide-shots-list">
                    {shotByScene.map((shot) => (
                      <li key={shot.id}>
                        <button
                          type="button"
                          className={shot.capturedAt !== null ? "guide-shot is-done" : "guide-shot"}
                          disabled={shot.id === sceneId}
                          title={
                            shot.capturedAt !== null
                              ? "切到这个界面——这张截图还在，可以直接接着改"
                              : "这个界面本次运行还没截过图，切过去点「截图并标注」"
                          }
                          onClick={() => onSceneId(shot.id)}
                        >
                          {shot.label}
                          <span className="guide-shot-time">
                            {shot.capturedAt !== null ? clockText(shot.capturedAt) : "还没截"}
                          </span>
                        </button>
                      </li>
                    ))}
                  </ul>
                  <p className="field-hint">
                    截图只留在<strong>本次运行</strong>里，关掉程序就没了——图里有联系人姓名和
                    聊天内容，按项目约定不写到磁盘上。所以重开程序之后，四个界面都要重新截一次；
                    但同一次运行里切来切去不会丢。
                  </p>
                </div>
              )}

              {preview ? (
                <>
                  <RegionCanvas
                    image={preview.image}
                    width={preview.width}
                    height={preview.height}
                    boxes={boxes}
                    activeKey={activeKey}
                    onChange={applyRect}
                    onCommit={commitRect}
                    onSelect={onActiveKey}
                    disabled={busy}
                  />
                  <p className="field-hint">
                    拖框体移动、拖八个手柄缩放、在空白处拖动给当前项拉一个新框。
                    窗口 {preview.window.width}×{preview.window.height} @({preview.window.x},{" "}
                    {preview.window.y})，截图指纹{" "}
                    <code>{preview.fingerprint.slice(0, 16)}</code>
                    {capturedAt !== null && <>，截于 {clockText(capturedAt)}</>}
                    。切到别的界面再回来，这张图还在（换窗口类名或目标程序才会失效）。
                  </p>
                </>
              ) : (
                <>
                  <p className="guide-placeholder">
                    先把客户端切到「{scene.label}」该有的样子，再点「截图并标注」。
                    这张图只是给人看着框的——不会点击、不会粘贴、也不会把窗口抢到前台。
                    {capturedScenes.length > 0 && (
                      <>
                        {" "}
                        你已经截过：{capturedScenes.join("、")}
                        ——那几张<strong>还在</strong>，切回对应界面就能接着改，不用重截。
                      </>
                    )}
                  </p>
                  {droppedShot && (
                    <p className="notice">
                      这个界面以前截过一张（当时窗口类名「{droppedShot.windowClass}」
                      {droppedShot.wecomExe !== null && `、程序「${droppedShot.wecomExe}」`}
                      ），与现在的设置对不上，所以不再显示——那张图截的是<strong>别的窗口</strong>，
                      拿来框会框错地方。请重新截一张。
                    </p>
                  )}
                </>
              )}

              {activeItem && (
                <div className="guide-current">
                  <div className="guide-current-head">
                    <span
                      className="guide-swatch"
                      style={{ background: lookByKey.get(activeItem.key)?.color }}
                    >
                      {lookByKey.get(activeItem.key)?.number}
                    </span>
                    <strong>{activeItem.label}</strong>
                    {activeItem.required && <span className="guide-required">必填</span>}
                    {/*
                      探针编号只在**当前项**这里显示，不在左边列表里逐条显示：
                      14 行每行再挂一个号会挤得看不清，而它真正用到的场合
                      只有一个——对着命令行截图说"把 3 往左挪"的时候。
                    */}
                    {activeItem.probe_index !== null && (
                      <span
                        className="guide-probe"
                        title="命令行截图（screen_probe annotate）画在区域上的角标编号。与左边列表里的 1.3 那种编号是两套：拿截图沟通时说的编号指的是这个。"
                      >
                        截图角标 {activeItem.probe_index}
                      </span>
                    )}
                    {activeItem.storage === "area_marks" && rectOf(activeItem) !== null && (
                        <button
                          type="button"
                          className="ghost"
                          disabled={busy}
                          onClick={() => clearMark(activeItem)}
                        >
                          清除这一项
                        </button>
                      )}
                  </div>
                  <p className="field-hint">{activeItem.hint}</p>
                </div>
              )}
            </>
          ) : (
            <p className="guide-placeholder">正在读取标定计划…</p>
          )}
        </div>
      </div>

      <p className="field-hint">
        标定结果存在 <code>{info.data_dir}</code> 下的配置文件里（<code>regions</code> 与{" "}
        <code>area_marks</code> 两个字段），跟窗口类名、OCR 路径是同一份配置，
        所以「保存配置」一次就全都写下去了。数据目录是
        <strong>程序运行当前路径</strong>下的 <code>data/</code>——
        换个目录启动程序，这个路径会跟着变，所以上面这行要对着看一眼。
      </p>
    </section>
  );
}
