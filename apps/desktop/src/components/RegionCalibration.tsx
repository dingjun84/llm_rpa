/**
 * 区域数据与校验。
 *
 * ## 这个文件现在只做两件事
 *
 * 1. [`DEFAULT_REGIONS`]：四块区域的默认比例（后端是单一来源，这里只能手抄）；
 * 2. [`regionsAreValid`]：保存配置前的合法性校验。
 *
 * ## 为什么不再有 `RegionCalibration` 组件
 *
 * 这里原来还有一个"填百分比、看一眼框"的区域标定面板，挂在「任务」页上。
 * 现在四个区域**只在「界面标定」页标**：那里是"截一张图、在图上框一块"，
 * 每一块按它所在的界面分场景列出、带编号和提示，而且和后端 `calibration::ITEMS`
 * 是同一份清单。
 *
 * 两处改的是**同一份配置**（`regions` 的四个具名字段），留着只会让人不知道该信
 * 哪一边；而且旧面板是手填数字，填错了不会报错、只会在运行时点偏。所以那个面板
 * 删掉了，标定入口统一到「界面标定」页。
 *
 * ⚠️ 「界面标定」页的**键名**和这里的**字段名**不是一回事：那边叫 `list_area`、
 * `chat_header` 这种标定项 key，写回配置时才通过后端的 `region_field` 映射到
 * `contact_panel` 这些字段名。两边别混（写错了 serde 会静默丢弃）。
 */

import type { RegionConfig } from "../types";

/**
 * 默认标定，与后端 `RegionConfig::default()` 保持一致。
 *
 * ⚠️ 后端那份从 `automation_core::DEFAULT_REGIONS` 派生（单一来源），
 * 而这里**只能手抄**——TypeScript 读不到 Rust 常量。改后端那四个常量时，
 * 这个对象必须一起改；两边不一致不会报错，只会让「恢复默认」恢复出一套
 * 和后端不一样的区域。
 *
 * `contact_panel` 的左边界是 0.14 而不是 0.0：微信会话列表左侧的导航图标栏
 * 与头像列会被 OCR 按行并进联系人姓名里（实测把「丁俊」读成「0 丁俊」），
 * 而姓名匹配是**逐字精确**的，多一个字就永远匹配不上。
 * 详细实测数据见 `automation_core::DEFAULT_CONTACT_PANEL` 的文档。
 */
export const DEFAULT_REGIONS: RegionConfig = {
  contact_panel: [0.14, 0.12, 0.28, 0.88],
  chat_header: [0.28, 0.0, 0.72, 0.1],
  chat_body: [0.28, 0.1, 0.72, 0.72],
  composer: [0.28, 0.82, 0.72, 0.18],
};

/**
 * 保存配置前必须全部合法的四块区域。
 *
 * ⚠️ **手抄**的。权威清单在后端 `calibration::ITEMS`（每一项的 `region_field`
 * 决定它写回 `regions` 的哪个字段）。这里只留四个键，是因为校验只需要知道
 * "有哪几块"；它们的中文名、编号、所属场景都在后端，界面标定页直接用后端下发的值，
 * 不再另抄一份——抄两份就会出现同一条区域在两处叫两个名字。
 */
const REGION_KEYS: (keyof RegionConfig)[] = [
  "contact_panel",
  "chat_header",
  "chat_body",
  "composer",
];

/**
 * 一条相对区域是否合法。
 *
 * 判据必须与后端 `RelativeRegion::validate` 完全一致：四个分量都在 0..=1、
 * `x+w<=1`、`y+h<=1`、宽高大于 0。前端先拦一道，免得用户保存了一个后端会拒绝的配置。
 */
function isValidRegion(region: [number, number, number, number]): boolean {
  const [x, y, w, h] = region;
  if (![x, y, w, h].every(Number.isFinite)) {
    return false;
  }
  if (x < 0 || y < 0 || w <= 0 || h <= 0) {
    return false;
  }
  // 留一点浮点余量：比例是界面上按百分比填的，0.1+0.9 这类加法可能落到 1.0000001。
  return x + w <= 1.000001 && y + h <= 1.000001;
}

/** 四块区域是否全部合法。保存前用它拦一道。 */
export function regionsAreValid(regions: RegionConfig): boolean {
  return REGION_KEYS.every((key) => isValidRegion(regions[key]));
}
