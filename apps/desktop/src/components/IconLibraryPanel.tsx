import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
} from "react";

import {
  clickIcon,
  deleteIcon,
  deleteIconVariant,
  listIcons,
  previewTargetWindow,
  probeNavIcon,
  saveIconFromCrop,
} from "../api";
import { normalizeName } from "../iconNames";
import type {
  IconClickResult,
  IconEntry,
  IconVariant,
  NavIconProbe,
  RuntimeConfig,
  RuntimeInfo,
  WindowPreview,
} from "../types";
import { IconList } from "./IconList";
import { NavResultSections } from "./NavResultSections";

interface Props {
  info: RuntimeInfo;
  draft: RuntimeConfig;
  /** 改配置（草稿）。不会立刻落盘——要用户点「保存配置」。 */
  onPatch: (patch: Partial<RuntimeConfig>) => void;
  onSave: () => void;
  dirty: boolean;
  canSave: boolean;
  /** 有任务在跑：此时**不碰屏幕**（任务正在驱动鼠标）。 */
  busy: boolean;
}

/** 预览图上的一个框（预览图像素坐标）。 */
interface Box {
  x: number;
  y: number;
  width: number;
  height: number;
}

/**
 * 拖拽的最小尺寸（预览图像素）。
 *
 * 比这更小的拖动是**手抖**，不是"想框一块"。把它当成框选的话，用户想取消选择时
 * 随手点一下画面就会留下一个 1×1 的框，然后被"太小"的提示挡住——而他根本没想框。
 */
const MIN_DRAG_PX = 3;

/** 框选结果放大显示的倍数。图标只有二十几像素，不放大看不出框得准不准。 */
const CROP_ZOOM = 4;

const clamp = (value: number, low: number, high: number) =>
  Math.min(high, Math.max(low, value));

/** 由拖拽的两个端点得出规范化矩形。 */
const boxFrom = (a: { x: number; y: number }, b: { x: number; y: number }): Box => ({
  x: Math.min(a.x, b.x),
  y: Math.min(a.y, b.y),
  width: Math.abs(a.x - b.x),
  height: Math.abs(a.y - b.y),
});

/**
 * 把预览图坐标系的框换算成**窗口像素**。
 *
 * 刻意与后端 `window_rect_from_preview` 用**同一套算法**（两条边分别取整再相减），
 * 这样界面上显示的数字就是保存时会真正用的那个。若这里图省事写成
 * `round(宽 × 比例)`，界面上会显示成 27×27、实际存下来 26×26——
 * 而模板大小差一个像素，匹配分数就会掉，且没人会想到是"显示"的问题。
 *
 * 后端仍然是权威：它拿到的框还会再算一遍并做越界校验。
 */
const toWindowBox = (box: Box, preview: WindowPreview): Box => {
  const scale = preview.window.width / preview.width;
  const map = (value: number) => Math.round(value * scale);
  const x = map(box.x);
  const y = map(box.y);
  return {
    x,
    y,
    width: map(box.x + box.width) - x,
    height: map(box.y + box.height) - y,
  };
};

/**
 * 「图标库」页。
 *
 * ## 为什么单独一页
 *
 * 这一步的操作方式与别处**完全不同**：其它标定都是"填个数字、看一眼框"，
 * 而这里是"截一张图、在图上框出一个图标、给它起名字"。塞进那一长列配置里，
 * 会让人找不到它，也会让人以为图标模板是又一个路径输入框。
 *
 * ## 名字是给谁看的
 *
 * 给**人**看的。配置里存的也是名字（`nav_icon_templates`），界面上从不显示路径——
 * 路径是抄不错才怪的东西，而"聊天""通讯录"这种名字一眼就能对上。
 *
 * ## 一个名字 = 一组图
 *
 * 同一个图标在**选中 / 未选中 / 带气泡提醒 / 气泡里数字不一样**时长得都不一样，
 * 而它们指的是**同一个**图标。所以：**名字输入框填同一个名字再存一次，就是给这个
 * 图标补一张变体**，不是重名错误。列表里一行一个图标，那一行下面排着它的全部缩略图。
 *
 * 为什么不能只存一张：点完图标界面会停在这个视图上，图标随之变成选中态；
 * 下一次跑的时候画面上是选中态，而库里只有未选中那张 ⇒ 再也匹配不上。
 */
export function IconLibraryPanel({
  info,
  draft,
  onPatch,
  onSave,
  dirty,
  canSave,
  busy,
}: Props) {
  const [entries, setEntries] = useState<IconEntry[]>([]);
  const [listError, setListError] = useState<string | null>(null);
  const [loadingList, setLoadingList] = useState(false);

  const [preview, setPreview] = useState<WindowPreview | null>(null);
  const [capturing, setCapturing] = useState(false);
  const [selection, setSelection] = useState<Box | null>(null);
  const [name, setName] = useState("");
  const [saving, setSaving] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);

  const [probing, setProbing] = useState<string | null>(null);
  const [probe, setProbe] = useState<NavIconProbe | null>(null);
  const [probingLabel, setProbingLabel] = useState<string | null>(null);
  const [clicking, setClicking] = useState<string | null>(null);
  const [click, setClick] = useState<IconClickResult | null>(null);
  const [error, setError] = useState<string | null>(null);

  const imgRef = useRef<HTMLImageElement | null>(null);
  const dragRef = useRef<{ x: number; y: number } | null>(null);

  const windowClass = draft.window_class.trim();
  const windowReady = windowClass !== "";
  const disabled = busy;

  const refresh = useCallback(async () => {
    setLoadingList(true);
    setListError(null);
    try {
      setEntries(await listIcons());
    } catch (err) {
      setListError(String(err));
    } finally {
      setLoadingList(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // 窗口变了、或者重新截了一张图，之前那个框就失效了——它量的是上一张图。
  useEffect(() => {
    setSelection(null);
  }, [preview]);

  const configuredNames = draft.nav_icon_templates;
  const historyNames = draft.history_icon_templates;
  const libraryNames = useMemo(
    () => new Set(entries.map((entry) => normalizeName(entry.name))),
    [entries],
  );
  /** 配置里引用了、但图标库里已经没有的名字（被删了，或者被手工改过配置）。 */
  const dangling = [...configuredNames, ...historyNames].filter(
    (name) => name.trim() !== "" && !libraryNames.has(normalizeName(name)),
  );
  /** 输入框里那个名字是不是**已经存在**：决定保存按钮是"新建"还是"补一张"。 */
  const existing = useMemo(
    () => entries.find((entry) => normalizeName(entry.name) === normalizeName(name)) ?? null,
    [entries, name],
  );

  const windowBox = selection && preview ? toWindowBox(selection, preview) : null;

  /** 框的尺寸问题。上限下限来自后端下发的常量，不在前端另写一份。 */
  const sizeIssue = useMemo(() => {
    if (!windowBox) return null;
    const { width, height } = windowBox;
    if (width < info.template_min_side || height < info.template_min_side) {
      return `框太小了：${width}×${height}，图标模板至少 ${info.template_min_side}×${info.template_min_side} 像素。`;
    }
    if (width > info.template_max_side || height > info.template_max_side) {
      return `框太大了：${width}×${height}，上限 ${info.template_max_side}×${info.template_max_side}。只框住图标本身就够了。`;
    }
    return null;
  }, [windowBox, info.template_min_side, info.template_max_side]);

  const capture = async () => {
    setCapturing(true);
    setError(null);
    setNotice(null);
    setProbe(null);
    setClick(null);
    try {
      setPreview(await previewTargetWindow(draft.window_class, draft.wecom_exe));
    } catch (err) {
      setError(String(err));
    } finally {
      setCapturing(false);
    }
  };

  /** 屏幕坐标 → 预览图像素坐标。 */
  const toImagePoint = (event: ReactPointerEvent<HTMLDivElement>) => {
    const el = imgRef.current;
    if (!el || !preview) return null;
    const rect = el.getBoundingClientRect();
    if (rect.width === 0 || rect.height === 0) return null;
    const scale = preview.width / rect.width;
    return {
      x: clamp(Math.round((event.clientX - rect.left) * scale), 0, preview.width),
      y: clamp(Math.round((event.clientY - rect.top) * scale), 0, preview.height),
    };
  };

  const onPointerDown = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (disabled || event.button !== 0) return;
    const point = toImagePoint(event);
    if (!point) return;
    event.currentTarget.setPointerCapture(event.pointerId);
    dragRef.current = point;
    setSelection({ x: point.x, y: point.y, width: 0, height: 0 });
  };

  const onPointerMove = (event: ReactPointerEvent<HTMLDivElement>) => {
    const start = dragRef.current;
    if (!start) return;
    const point = toImagePoint(event);
    if (point) setSelection(boxFrom(start, point));
  };

  const onPointerUp = (event: ReactPointerEvent<HTMLDivElement>) => {
    const start = dragRef.current;
    dragRef.current = null;
    if (!start) return;
    const point = toImagePoint(event) ?? start;
    const box = boxFrom(start, point);
    // 点一下不拖 = 取消选择，而不是留下一个手抖出来的一像素框。
    setSelection(box.width >= MIN_DRAG_PX && box.height >= MIN_DRAG_PX ? box : null);
  };

  const save = async () => {
    if (!preview || !selection) return;
    setSaving(true);
    setError(null);
    setNotice(null);
    // 保存之前先记下"这个名字原来有几张"，好把提示写成"补第几张"。
    const wasExisting = existing !== null;
    try {
      const saved = await saveIconFromCrop(
        name,
        draft.window_class,
        draft.wecom_exe,
        [selection.x, selection.y, selection.width, selection.height],
        [preview.width, preview.height],
      );
      const count = saved.variants.length;
      const last = saved.variants[count - 1];
      setNotice(
        wasExisting
          ? `已给「${saved.name}」补上第 ${count} 张（${last.width}×${last.height}）。` +
              `这一个名字下的 ${count} 张图都会参与匹配——把选中态、带气泡的样子都补上，` +
              `换状态时就不会认不出来了。`
          : `已新建图标「${saved.name}」（第 1 张，${last.width}×${last.height}）。` +
              `接着把它的其他样子也存进来：框住另一个状态、名字填一样的再保存一次，` +
              `就是给它补一张，不会覆盖前面那张。`,
      );
      setName("");
      setSelection(null);
      await refresh();
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  };

  /**
   * 把一个图标勾进 / 摘出某一组导航模板。
   *
   * ## 为什么是**两组**而不是一组
   *
   * 联系人图标与聊天历史图标长得不一样，模板不能通用。混成一个列表时，
   * 「用错了哪一组」不会报错——只会拿聊天历史的模板去匹配联系人图标，
   * 然后以一次分数不高的匹配转人工。分开之后，配错的那一组是**空的**，
   * 装配期就能直接拒绝并说清是哪一组。
   */
  const toggleUse = (group: "nav_icon_templates" | "history_icon_templates", entry: IconEntry, on: boolean) => {
    const key = normalizeName(entry.name);
    const rest = draft[group].filter((name) => normalizeName(name) !== key);
    onPatch({ [group]: on ? [...rest, entry.name] : rest } as Partial<RuntimeConfig>);
  };

  /** 删掉一整组（含全部变体）。 */
  const remove = async (entry: IconEntry) => {
    setError(null);
    setNotice(null);
    try {
      const count = entry.variants.length;
      await deleteIcon(entry.name);
      // 名字没了，配置里那一条就是死的——留着它，任务装配时必然报
      // 「图标库里没有这个图标」。顺手一起摘掉，并在提示里说明白。
      // **两组都要摘**：只摘一组的话，另一组里会留下一个死名字，
      // 而症状是"另一条工作流一跑就在装配期报错"，看起来与这次删除无关。
      const key = normalizeName(entry.name);
      const stillUsed =
        configuredNames.some((name) => normalizeName(name) === key) ||
        historyNames.some((name) => normalizeName(name) === key);
      if (stillUsed) {
        const strip = (names: string[]) =>
          names.filter((name) => normalizeName(name) !== key);
        onPatch({
          nav_icon_templates: strip(configuredNames),
          history_icon_templates: strip(historyNames),
        });
      }
      setNotice(
        `已删除「${entry.name}」${count > 1 ? `（连带 ${count} 张图）` : ""}` +
          (stillUsed ? "，并把它从两组导航模板里都摘掉了（记得点「保存配置」）。" : "。"),
      );
      await refresh();
    } catch (err) {
      setError(String(err));
    }
  };

  /** 只删这一张，同一个名字下的其他图不动。 */
  const removeVariant = async (entry: IconEntry, variant: IconVariant) => {
    setError(null);
    setNotice(null);
    try {
      await deleteIconVariant(entry.name, variant.relative);
      const left = entry.variants.length - 1;
      setNotice(
        left > 0
          ? `已删除「${entry.name}」的 ${variant.relative}，还剩 ${left} 张。`
          : `已删除「${entry.name}」的最后一张，这个图标也从列表里没了。`,
      );
      await refresh();
    } catch (err) {
      setError(String(err));
    }
  };

  const runProbe = async (names: string[], label: string) => {
    setProbing(label);
    setProbingLabel(label);
    setProbe(null);
    setClick(null);
    setError(null);
    try {
      setProbe(
        await probeNavIcon(
          draft.window_class,
          draft.wecom_exe,
          draft.nav_strip,
          names,
          draft.nav_icon_min_score,
        ),
      );
    } catch (err) {
      setError(String(err));
    } finally {
      setProbing(null);
    }
  };

  const runClick = async (names: string[], label: string) => {
    setClicking(label);
    setClick(null);
    setProbe(null);
    setError(null);
    try {
      setClick(
        await clickIcon(
          draft.window_class,
          draft.wecom_exe,
          draft.nav_strip,
          draft.regions.contact_panel,
          names,
          draft.nav_icon_min_score,
          // 复用「滚动停稳等待」：等待的是同一个物理现象（界面动画还没画完）。
          draft.scroll_settle_ms,
        ),
      );
    } catch (err) {
      setError(String(err));
    } finally {
      setClicking(null);
    }
  };

  const ratio = (raw: string, fallback: number) => {
    if (raw.trim() === "") return fallback;
    const value = Number(raw);
    return Number.isFinite(value) ? clamp(value, 0, 1) : fallback;
  };

  return (
    <section className="panel">
      <h2>图标库</h2>
      <p className="field-hint">
        左侧导航栏那排图标<strong>没有文字</strong>，OCR 读不到它们。所以「先切到某个视图」
        这件事只能靠<strong>模板匹配</strong>：拿一张图标的小图，在画面里找它最像的位置。
        <br />
        这一页就是造那张小图的地方：<strong>截一张窗口画面 → 在图上框住图标 → 起个名字存下来</strong>。
        存好之后可以立刻「定位并点击」试一下，也可以勾上「用于联系人导航」或
        「用于聊天历史导航」，让任务在查找之前先切视图（「只做导航」那条工作流
        则一定要勾——它除了点图标什么都不做）。
        <br />
        <strong>一个名字可以存多张图</strong>：同一个图标在选中 / 未选中 / 带气泡提醒 /
        气泡里数字不一样时长得都不一样，而它们指的是同一个图标。名字填一样的再存一次就是
        <strong>给它补一张</strong>，这一个名字下的所有图都会参与匹配。
      </p>

      {!windowReady && (
        <p className="notice notice-warn">
          还没有目标窗口的类名。先到「任务」页的「目标窗口」里点「指认窗口」，
          否则截不了图。
        </p>
      )}

      {/* ── 一、截取 ─────────────────────────────────────────────────── */}
      <section className="calibration">
        <div className="calibration-head">
          <h3>① 截取窗口画面</h3>
        </div>
        <p className="field-hint">
          截的是<strong>整窗画面</strong>，只读：不点击、不聚焦、不把窗口带到前台。
          截图前请把客户端窗口<strong>完整露出来</strong>——它被别的窗口挡住的部分，
          截到的会是挡住它的那个窗口。
        </p>
        <div className="calibration-actions">
          <button
            type="button"
            disabled={disabled || capturing || !windowReady}
            onClick={capture}
          >
            {capturing ? "截取中…" : preview ? "重新截取" : "截取窗口画面"}
          </button>
          {preview && (
            <span className="field-hint">
              窗口 {preview.window.width}×{preview.window.height} @({preview.window.x},{" "}
              {preview.window.y})，预览图 {preview.width}×{preview.height}
              {preview.width !== preview.window.width ? "（已等比缩小）" : ""}
            </span>
          )}
        </div>

        {preview && (
          <>
            <div className="calibration-head">
              <h3>② 在图上按住左键拖出一个框，框住那个图标</h3>
            </div>
            <div
              className="shot-wrap shot-crop"
              onPointerDown={onPointerDown}
              onPointerMove={onPointerMove}
              onPointerUp={onPointerUp}
            >
              <img
                ref={imgRef}
                className="shot-image"
                src={preview.image}
                alt="目标窗口画面"
                draggable={false}
              />
              <svg
                className="shot-overlay"
                viewBox={`0 0 ${preview.width} ${preview.height}`}
                preserveAspectRatio="none"
              >
                {selection && (
                  <rect
                    className="shot-selection"
                    x={selection.x}
                    y={selection.y}
                    width={selection.width}
                    height={selection.height}
                    vectorEffect="non-scaling-stroke"
                  />
                )}
              </svg>
            </div>
            <p className="field-hint">
              只框<strong>图标本身</strong>，不要带上一圈背景——多带一点，匹配分数就低一点；
              少框一点，图标换个状态就认不出来。点一下不拖可以取消当前这个框。
            </p>

            {/* ── 二、命名并保存 ───────────────────────────────────────── */}
            <div className="calibration-head">
              <h3>③ 起个名字存起来</h3>
            </div>
            <div className="crop-save">
              {selection && windowBox ? (
                <div className="crop-zoom-wrap">
                  <div
                    className="crop-zoom"
                    style={{
                      width: windowBox.width * CROP_ZOOM,
                      height: windowBox.height * CROP_ZOOM,
                      backgroundImage: `url(${preview.image})`,
                      backgroundSize: `${preview.width * CROP_ZOOM}px ${preview.height * CROP_ZOOM}px`,
                      backgroundPosition: `-${selection.x * CROP_ZOOM}px -${selection.y * CROP_ZOOM}px`,
                    }}
                  />
                  <span className="field-hint">
                    框：预览 {selection.width}×{selection.height} → 窗口{" "}
                    {windowBox.width}×{windowBox.height} @({windowBox.x}, {windowBox.y})
                    （左边是放大 {CROP_ZOOM} 倍的效果）
                  </span>
                </div>
              ) : (
                <span className="field-hint">还没有框——在图上拖一下。</span>
              )}

              <label className="field">
                <span className="field-label">图标名</span>
                <input
                  type="text"
                  value={name}
                  disabled={disabled}
                  list="icon-library-names"
                  placeholder="例如：聊天 / 通讯录"
                  onChange={(event) => setName(event.target.value)}
                />
                {/* 已存在的名字做成候选：想给同一个图标补一张时**不用手抄名字**，
                    抄错一个字符就会悄悄多出一个新图标，而那个新图标只有一张图。 */}
                <datalist id="icon-library-names">
                  {entries.map((entry) => (
                    <option key={entry.name} value={entry.name} />
                  ))}
                </datalist>
                <span className="field-hint">
                  名字是给你自己看的，也是引用它的依据。
                  {existing ? (
                    <>
                      <strong>「{existing.name}」已经有 {existing.variants.length} 张图了</strong>
                      ——再点保存就是给它<strong>补一张</strong>（框住另一个状态再存），
                      不会覆盖前面那些。
                    </>
                  ) : (
                    <>
                      同一个图标有几种样子就存几张：<strong>名字填一样的</strong>，
                      分别框住未选中 / 选中 / 带气泡的样子各存一次。
                    </>
                  )}
                </span>
              </label>

              <div className="calibration-actions">
                <button
                  type="button"
                  className="primary"
                  disabled={
                    disabled || saving || !selection || !!sizeIssue || name.trim() === ""
                  }
                  onClick={save}
                >
                  {saving
                    ? "保存中…"
                    : existing
                      ? `给「${existing.name}」补第 ${existing.variants.length + 1} 张`
                      : "新建图标并存下这张"}
                </button>
              </div>
              {sizeIssue && <p className="notice notice-warn">{sizeIssue}</p>}
            </div>
          </>
        )}
      </section>

      {/* ── 三、图标库 ───────────────────────────────────────────────── */}
      <section className="calibration">
        <div className="calibration-head">
          <h3>图标库</h3>
          {loadingList && <span className="field-hint">读取中…</span>}
        </div>

        <label className="field">
          <span className="field-label">图标库目录</span>
          <input
            type="text"
            value={draft.icons_dir ?? ""}
            disabled={disabled}
            placeholder={info.icons_dir_default}
            onChange={(event) =>
              onPatch({
                icons_dir: event.target.value.trim() === "" ? null : event.target.value,
              })
            }
          />
          <span className="field-hint">
            现在生效的是 <code className="mono">{info.icons_dir}</code>。
            留空 = 用默认（<strong>项目根</strong>下的 <code>data/icons/</code>）；
            写相对路径按项目根解析，所以换盘符、换机器都不会失效。
            <br />
            一个图标名对应这里的一个子目录，目录里可以放多张图。
            {draft.icons_dir?.trim() ? (
              <>
                {" "}
                ⚠️ 草稿里改过了——<strong>列表读的是已保存的目录</strong>，
                改完要点下面的「保存配置」。
              </>
            ) : null}
          </span>
        </label>

        {listError && <p className="notice notice-error">读取图标库失败：{listError}</p>}
        {entries.length === 0 && !listError && (
          <p className="muted-line">还是空的。上面截一张图、框一个图标就有了。</p>
        )}

        <IconList
          entries={entries}
          configuredNames={configuredNames}
          historyNames={historyNames}
          dangling={dangling}
          disabled={disabled}
          windowReady={windowReady}
          probing={probing}
          clicking={clicking}
          onToggleUse={toggleUse}
          onProbe={runProbe}
          onClick={runClick}
          onRemove={remove}
          onRemoveVariant={removeVariant}
        />

        <div className="calibration-actions">
          <button
            type="button"
            disabled={
              disabled ||
              probing !== null ||
              (configuredNames.length === 0 && historyNames.length === 0) ||
              !windowReady
            }
            onClick={() =>
              runProbe(
                // 两组一起量：操作者点这个按钮时想知道的是"我配的这些到底行不行"，
                // 而两组用的是同一套阈值与同一个搜索区。
                [...configuredNames, ...historyNames],
                "全部已选图标",
              )
            }
          >
            {probing === "全部已选图标" ? "匹配中…" : "测试全部已选图标"}
          </button>
        </div>
      </section>

      {/* ── 四、匹配参数 ─────────────────────────────────────────────── */}
      <section className="calibration">
        <div className="calibration-head">
          <h3>匹配参数</h3>
        </div>

        <label className="field field-check">
          <input
            type="checkbox"
            checked={draft.navigate_before_search}
            disabled={disabled}
            onChange={(event) =>
              onPatch({ navigate_before_search: event.target.checked })
            }
          />
          <span>
            <span className="field-label">任务开始前先点击导航图标跳转</span>
            <span className="field-hint">
              打开后，任务在查找联系人之前会先匹配一次图标、点它一下。打开时必须
              <strong>至少勾一张「用于联系人导航」</strong>，否则任务在装配期就被拒绝
              （不会在任务列表里留下一条注定失败的记录）。
              <br />
              「只做导航」那条工作流<strong>不受这个开关控制</strong>：
              导航就是它的全部内容，所以它一定会去点图标，也就一定要求模板。
            </span>
          </span>
        </label>

        {draft.navigate_before_search && configuredNames.length === 0 && (
          <p className="notice notice-warn">
            已经打开，但<strong>一个图标都没勾</strong>——这样保存下去，
            点「开始任务」时会在装配期被直接拒绝。在上面勾一个「用于联系人导航」。
          </p>
        )}

        <div className="field">
          <span className="field-label">匹配参数</span>
          <div className="calibration-inputs">
            <label>
              <span>最低分数</span>
              <input
                type="number"
                min={0}
                max={1}
                step={0.01}
                value={draft.nav_icon_min_score}
                disabled={disabled}
                onChange={(event) =>
                  onPatch({
                    nav_icon_min_score: ratio(event.target.value, draft.nav_icon_min_score),
                  })
                }
              />
            </label>
            {(["左", "上", "宽", "高"] as const).map((label, index) => (
              <label key={label}>
                <span>搜索区 {label}</span>
                <input
                  type="number"
                  min={0}
                  max={1}
                  step={0.005}
                  value={draft.nav_strip[index]}
                  disabled={disabled}
                  onChange={(event) => {
                    const next = [...draft.nav_strip] as [number, number, number, number];
                    next[index] = ratio(event.target.value, draft.nav_strip[index]);
                    onPatch({ nav_strip: next });
                  }}
                />
              </label>
            ))}
            <label>
              <span>位置先验容差</span>
              <input
                type="number"
                min={0}
                max={1}
                step={0.01}
                value={draft.icon_prior_score_tolerance}
                disabled={disabled}
                onChange={(event) =>
                  onPatch({
                    icon_prior_score_tolerance: ratio(
                      event.target.value,
                      draft.icon_prior_score_tolerance,
                    ),
                  })
                }
              />
            </label>
          </div>
          <span className="field-hint">
            <strong>搜索区</strong>是「去窗口的哪一块里找图标」（相对窗口的比例）。
            默认 <code>0, 0, 0.075, 1</code> = 最左侧那条竖带、整高：实测图标栏是
            0–57px、头像列从 78px 起（974 宽的窗口），0.075 换算过去是 73px，
            正好让开头像列。范围越小匹配越快，但盖不住图标就找不到了。
          </span>
          <span className="field-hint">
            <strong>最低分数</strong>是归一化互相关（1.0 = 完全一致）。
            <strong>不要照抄默认值</strong>——用「测试匹配」对着当前靶标量一次。
            定低了会点错图标，定高了会频繁转人工。
          </span>
          <span className="field-hint">
            <strong>位置先验容差</strong>（<code>0</code> = 关掉先验，默认{" "}
            <code>0.05</code>）：导航栏是一列纵向排列、彼此长得很像的图标，
            逐张模板取最高分时偶尔会出现「旁边那个图标分数略高一点」。
            而这件事有先验可用——<strong>越靠近导航区中心的命中越可信</strong>。
            容差决定「分数差多少以内才允许用位置来取舍」：
            定大了等于用位置替代了识别，定小了先验基本不生效。
          </span>
        </div>

        <div className="calibration-actions">
          <button
            type="button"
            className="primary"
            disabled={busy || !canSave}
            onClick={onSave}
          >
            {!canSave ? "标定不合法，无法保存" : dirty ? "保存配置" : "已保存"}
          </button>
          {dirty && (
            <span className="field-hint">
              ⚠️ 有改动还没保存。任务用的是<strong>已保存</strong>的配置，不是这一页上的草稿。
            </span>
          )}
        </div>
      </section>

      {error && <p className="notice notice-error">{error}</p>}
      {notice && <p className="notice">{notice}</p>}

      {/* ── 五、结果 ─────────────────────────────────────────────────── */}
      <NavResultSections probe={probe} probingLabel={probingLabel} click={click} />
    </section>
  );
}
