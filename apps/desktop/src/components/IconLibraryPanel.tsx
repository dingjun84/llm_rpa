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
  listIcons,
  previewTargetWindow,
  probeNavIcon,
  saveIconFromCrop,
} from "../api";
import type {
  IconClickResult,
  IconEntry,
  NavIconHit,
  NavIconProbe,
  Rect,
  RuntimeConfig,
  RuntimeInfo,
  WindowPreview,
} from "../types";

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

/**
 * 路径比较用。Windows 的文件系统不区分大小写，分隔符也可能混用，
 * 直接字符串比较会把"同一个文件"判成两个，于是「用于导航」的勾选状态会莫名跳回去。
 */
const normalizePath = (path: string) => path.trim().replace(/\\/g, "/").toLowerCase();

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

/** 只读的结果预览：整窗画面 + 搜索区（蓝虚线）+ 命中框（绿/红）+ 点击点。 */
function ShotOverlay({
  image,
  width,
  height,
  strip,
  hit,
  clicked,
}: {
  image: string;
  width: number;
  height: number;
  strip?: Rect | null;
  hit?: NavIconHit | null;
  clicked?: { x: number; y: number } | null;
}) {
  return (
    <div className="shot-wrap">
      <img className="shot-image" src={image} alt="目标窗口画面" draggable={false} />
      {/* 用 SVG + viewBox 而不是按比例算像素：坐标直接用**图像坐标系**，
          画布缩放由浏览器负责，界面怎么缩都不会算错。 */}
      <svg
        className="shot-overlay"
        viewBox={`0 0 ${width} ${height}`}
        preserveAspectRatio="none"
      >
        {strip && (
          <rect
            className="shot-strip"
            x={strip.x}
            y={strip.y}
            width={strip.width}
            height={strip.height}
            vectorEffect="non-scaling-stroke"
          />
        )}
        {hit && (
          <rect
            className={hit.accepted ? "shot-hit" : "shot-hit is-low"}
            x={hit.x}
            y={hit.y}
            width={hit.width}
            height={hit.height}
            vectorEffect="non-scaling-stroke"
          />
        )}
        {clicked && (
          <circle
            className="shot-click"
            cx={clicked.x}
            cy={clicked.y}
            r={4}
            vectorEffect="non-scaling-stroke"
          />
        )}
      </svg>
    </div>
  );
}

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
 * 给**人**看的。配置里存的仍然是文件路径（`nav_icon_templates`），
 * 但界面上从不显示路径——路径是抄不错才怪的东西，而"通讯录""聊天-选中"
 * 这种名字一眼就能对上。
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
  /** 已经"上了膛"的那张图标：点一次变成「再点一次确认」，点第二次才真的点。 */
  const [armed, setArmed] = useState<string | null>(null);
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

  const configuredPaths = draft.nav_icon_templates;
  const libraryPaths = useMemo(
    () => new Set(entries.map((entry) => normalizePath(entry.file))),
    [entries],
  );
  /** 配置里引用了、但图标库里已经没有的模板（被删了，或者被手工改过路径）。 */
  const dangling = configuredPaths.filter(
    (path) => path.trim() !== "" && !libraryPaths.has(normalizePath(path)),
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
    try {
      const saved = await saveIconFromCrop(
        name,
        draft.window_class,
        draft.wecom_exe,
        [selection.x, selection.y, selection.width, selection.height],
        [preview.width, preview.height],
      );
      setNotice(
        `已存进图标库：${saved.name}（${saved.width}×${saved.height}）。` +
          `同一个图标在**选中 / 未选中**两种状态下长得不一样——` +
          `想再存一张，就把框挪到切换后的图标上、改个名字再保存。`,
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

  const toggleUse = (entry: IconEntry, on: boolean) => {
    const key = normalizePath(entry.file);
    const rest = configuredPaths.filter((path) => normalizePath(path) !== key);
    onPatch({ nav_icon_templates: on ? [...rest, entry.file] : rest });
  };

  const remove = async (entry: IconEntry) => {
    setError(null);
    setNotice(null);
    try {
      await deleteIcon(entry.name);
      // 文件没了，配置里那一条就是死的——留着它，任务装配时必然报
      // 「图标模板不可用」。顺手一起摘掉，并在提示里说明白。
      const key = normalizePath(entry.file);
      const stillUsed = configuredPaths.some((path) => normalizePath(path) === key);
      if (stillUsed) {
        onPatch({
          nav_icon_templates: configuredPaths.filter(
            (path) => normalizePath(path) !== key,
          ),
        });
      }
      setNotice(
        stillUsed
          ? `已删除「${entry.name}」，并把它从「用于导航」里摘掉了（记得点「保存配置」）。`
          : `已删除「${entry.name}」。`,
      );
      await refresh();
    } catch (err) {
      setError(String(err));
    }
  };

  const runProbe = async (paths: string[], label: string) => {
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
          paths,
          draft.nav_icon_min_score,
        ),
      );
    } catch (err) {
      setError(String(err));
    } finally {
      setProbing(null);
    }
  };

  const runClick = async (paths: string[], label: string) => {
    setClicking(label);
    setArmed(null);
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
          paths,
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

  /** 图上那条蓝虚线对应的位置，用窗口内相对坐标表示（点击结果要减掉窗口原点）。 */
  const clickedInWindow = click
    ? { x: click.clicked.x - click.window.x, y: click.clicked.y - click.window.y }
    : null;

  return (
    <section className="panel">
      <h2>图标库</h2>
      <p className="field-hint">
        左侧导航栏那排图标<strong>没有文字</strong>，OCR 读不到它们。所以「先切到某个视图」
        这件事只能靠<strong>模板匹配</strong>：拿一张图标的小图，在画面里找它最像的位置。
        <br />
        这一页就是造那张小图的地方：<strong>截一张窗口画面 → 在图上框住图标 → 起个名字存下来</strong>。
        存好之后可以立刻「定位并点击」试一下，也可以勾上「用于导航」让任务在查找联系人之前先切视图。
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
                  placeholder="例如：通讯录 / 聊天 / 通讯录-选中"
                  onChange={(event) => setName(event.target.value)}
                />
                <span className="field-hint">
                  名字是给你自己看的，也是引用它的依据。同一个图标建议把
                  <strong>选中 / 未选中</strong>两种状态各存一张，名字区分开。
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
                  {saving ? "保存中…" : "保存到图标库"}
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
        <p className="field-hint mono">{info.icons_dir}</p>

        {listError && <p className="notice notice-error">读取图标库失败：{listError}</p>}
        {entries.length === 0 && !listError && (
          <p className="muted-line">还是空的。上面截一张图、框一个图标就有了。</p>
        )}

        <ul className="icon-list">
          {entries.map((entry) => {
            const used = configuredPaths.some(
              (path) => normalizePath(path) === normalizePath(entry.file),
            );
            const broken = entry.problem !== null;
            const isArmed = armed === entry.name;
            return (
              <li key={entry.name} className={broken ? "icon-row is-broken" : "icon-row"}>
                {entry.image ? (
                  <img className="icon-thumb" src={entry.image} alt={entry.name} />
                ) : (
                  <span className="icon-thumb icon-thumb-empty">?</span>
                )}
                <div className="icon-meta">
                  <span className="icon-name">{entry.name}</span>
                  <span className="field-hint">
                    {broken
                      ? "尺寸读不出来"
                      : `${entry.width}×${entry.height}`}{" "}
                    · {entry.bytes} 字节
                  </span>
                  {broken && (
                    <span className="notice notice-error">
                      这张图现在不能当模板：{entry.problem}
                    </span>
                  )}
                </div>
                <div className="icon-actions">
                  <label className="field-check icon-use">
                    <input
                      type="checkbox"
                      checked={used}
                      disabled={disabled || broken}
                      onChange={(event) => toggleUse(entry, event.target.checked)}
                    />
                    <span>用于导航</span>
                  </label>
                  <button
                    type="button"
                    disabled={disabled || broken || !windowReady || probing !== null}
                    onClick={() => runProbe([entry.file], entry.name)}
                  >
                    {probing === entry.name ? "匹配中…" : "测试匹配"}
                  </button>
                  <button
                    type="button"
                    className={isArmed ? "danger" : undefined}
                    disabled={disabled || broken || !windowReady || clicking !== null}
                    title="会真的在客户端窗口上点一下鼠标"
                    onClick={() => {
                      if (isArmed) void runClick([entry.file], entry.name);
                      else setArmed(entry.name);
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
                    className="ghost"
                    disabled={disabled}
                    onClick={() => void remove(entry)}
                  >
                    删除
                  </button>
                </div>
              </li>
            );
          })}
        </ul>

        {dangling.length > 0 && (
          <p className="notice notice-warn">
            配置里引用了 {dangling.length} 张<strong>不在图标库里</strong>的模板
            （文件被删了、或者路径被手工改过）。任务装配时会直接报错，
            建议在这里重新勾选，或者把它们从配置里去掉。
          </p>
        )}

        <div className="calibration-actions">
          <button
            type="button"
            disabled={disabled || probing !== null || configuredPaths.length === 0 || !windowReady}
            onClick={() => runProbe(configuredPaths, "全部已选模板")}
          >
            {probing === "全部已选模板" ? "匹配中…" : "测试全部已选模板"}
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
              <strong>至少勾一张「用于导航」</strong>，否则任务在装配期就被拒绝
              （不会在任务列表里留下一条注定失败的记录）。
            </span>
          </span>
        </label>

        {draft.navigate_before_search && configuredPaths.length === 0 && (
          <p className="notice notice-warn">
            已经打开，但<strong>一张模板都没勾</strong>——这样保存下去，
            点「开始任务」时会在装配期被直接拒绝。在上面勾一张「用于导航」。
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
      {probe && (
        <section className="calibration">
          <div className="calibration-head">
            <h3>匹配结果{probingLabel ? `（${probingLabel}）` : ""}</h3>
          </div>
          <p className={probe.hit?.accepted ? "notice" : "notice notice-warn"}>
            {probe.notice}
          </p>
          <ShotOverlay
            image={probe.image}
            width={probe.width}
            height={probe.height}
            strip={probe.strip}
            hit={probe.hit}
          />
          <span className="field-hint">
            蓝虚线 = 搜索区，{probe.hit?.accepted ? "绿" : "红"}实线 = 匹配到的位置。
            <strong>红框也要看</strong>——它标的是「它认为最像的地方」，
            那个位置对不对才是判断模板对不对的依据。框住的不是那个图标，
            就说明模板截错了、或者搜索区没盖住它。
          </span>
        </section>
      )}

      {click && (
        <section className="calibration">
          <div className="calibration-head">
            <h3>点击结果</h3>
          </div>
          <p className={click.changed ? "notice" : "notice notice-warn"}>
            {click.notice}
          </p>
          <ShotOverlay
            image={click.image}
            width={click.width}
            height={click.height}
            strip={click.strip}
            hit={click.hit}
            clicked={clickedInWindow}
          />
          <span className="field-hint">
            蓝虚线 = 搜索区，绿实线 = 命中的图标，圆圈 = 鼠标实际落下的位置。
            这张图是<strong>点击之后</strong>截的，所以图标上可能已经带着选中态的高亮。
          </span>
        </section>
      )}
    </section>
  );
}
