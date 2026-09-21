import type { IconClickResult, NavIconHit, NavIconProbe, NavIconScore, Rect } from "../types";

interface Props {
  /** 「测试匹配」的结果。`null` = 还没测过。 */
  probe: NavIconProbe | null;
  /** 那一次测试是拿哪一组测的（图标名，或「全部已选图标」）。 */
  probingLabel: string | null;
  /** 「定位并点击」的结果。`null` = 还没点过。 */
  click: IconClickResult | null;
}

/** 只读的结果预览：整窗画面 + 搜索区（蓝虚线）+ 命中框（绿/红）+ 分数标签 + 点击点。 */
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
  const labelY = hit ? Math.max(12, hit.y - 4) : 0;
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
          <>
            <rect
              className={hit.accepted ? "shot-hit" : "shot-hit is-low"}
              x={hit.x}
              y={hit.y}
              width={hit.width}
              height={hit.height}
              vectorEffect="non-scaling-stroke"
            />
            <text
              className={hit.accepted ? "shot-score" : "shot-score is-low"}
              x={hit.x}
              y={labelY}
              vectorEffect="non-scaling-stroke"
            >
              {hit.score.toFixed(3)}
            </text>
          </>
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

function ScoreTable({ scores }: { scores: NavIconScore[] }) {
  if (!scores.length) return null;
  return (
    <div className="score-table-wrap">
      <table className="score-table">
        <thead>
          <tr>
            <th>模板</th>
            <th>分数</th>
            <th>相对阈值</th>
          </tr>
        </thead>
        <tbody>
          {scores.map((row) => (
            <tr key={row.template} className={row.accepted ? "is-ok" : "is-low"}>
              <td>{row.template}</td>
              <td className="score-num">
                {row.score < 0 ? "—" : row.score.toFixed(3)}
              </td>
              <td>
                {row.score < 0
                  ? "放不进搜索区"
                  : row.accepted
                    ? "已过阈值"
                    : "未过阈值"}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

/**
 * 「匹配结果」与「点击结果」两段只读预览。
 *
 * ## 为什么单独成文件
 *
 * 这两段是**纯展示**：入参就是后端回来的两份结果，没有回调、没有本地状态、
 * 也不改配置。`IconLibraryPanel` 里剩下的每一段都要读写一堆共享状态，
 * 只有这一段能整块搬走而不牵动别处。
 *
 * ★ 它**不判断"结果对不对"** —— 那句话（`probe.notice` / `click.notice`）
 * 是后端算好下发的，这里只负责上色和画框。前端自己判一次，两边就会漂移。
 */
export function NavResultSections({ probe, probingLabel, click }: Props) {
  /** 图上那条蓝虚线对应的位置，用窗口内相对坐标表示（点击结果要减掉窗口原点）。 */
  const clickedInWindow = click
    ? { x: click.clicked.x - click.window.x, y: click.clicked.y - click.window.y }
    : null;

  return (
    <>
      {probe && (
        <section className="calibration">
          <div className="calibration-head">
            <h3>匹配结果{probingLabel ? `（${probingLabel}）` : ""}</h3>
          </div>
          {probe.hit ? (
            <p className={probe.hit.accepted ? "notice score-banner" : "notice notice-warn score-banner"}>
              最高分 <strong>{probe.hit.score.toFixed(3)}</strong>
              {" · "}模板「{probe.hit.template}」
              {probe.hit.accepted ? " · 已过阈值" : " · 未过阈值"}
            </p>
          ) : (
            <p className="notice notice-warn score-banner">没有可用命中</p>
          )}
          <p className={probe.hit?.accepted ? "notice" : "notice notice-warn"}>
            {probe.notice}
          </p>
          <ScoreTable scores={probe.scores ?? []} />
          <ShotOverlay
            image={probe.image}
            width={probe.width}
            height={probe.height}
            strip={probe.strip}
            hit={probe.hit}
          />
          <span className="field-hint">
            蓝虚线 = 搜索区，{probe.hit?.accepted ? "绿" : "红"}实线 = 匹配到的位置，
            框旁数字 = 该命中分数。
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
          {click.hit && (
            <p className={click.hit.accepted ? "notice score-banner" : "notice notice-warn score-banner"}>
              点击时分数 <strong>{click.hit.score.toFixed(3)}</strong>
              {" · "}模板「{click.hit.template}」
            </p>
          )}
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
    </>
  );
}
