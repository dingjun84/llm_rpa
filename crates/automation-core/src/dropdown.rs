//! **搜索下拉里挑人**这条判据：结论与轨迹**由同一个函数一次交出**。
//!
//! ## 为什么它不留在 `runner/search.rs` 里
//!
//! 两个原因，都不是"文件太长"这种形式问题：
//!
//! 1. **判据要被离线重放**（`docs/todo.md` T29）。重放工具要拿盘上记下来的那一帧
//!    重跑一次判定，改个阈值看结论变不变。判据藏在编排器的私有方法里，重放就只能
//!    自己再写一遍——那正是"两套判据"的开始。
//! 2. **轨迹必须与结论同源**（`CONVENTIONS.md` §1.3）。这里一次返回
//!    「挑中了谁」+「每个候选为什么」，编排层只负责把结论用起来、把轨迹交出去。
//!
//! ## 判据（操作者 2026-09-19 指定；2026-09-21 扩成多分组 + 多命中取最上面）
//!
//! 下拉列表是**分组**的：先一行分组标题，标题下面才是匹配到的人；
//! 再往下可能还有「聊天记录」「群聊」之类的分组。所以不能整块找
//! "文字包含输入词"——那样会把聊天记录里提到这个名字的消息也算进来，
//! 点下去就点进了别的地方。
//!
//! Mac 微信上同一个人有时只出现在「最常使用」底下，不在「联系人」里。
//! 所以配置里可以写多项（默认「联系人 / 最常使用」）：对每一项各自
//! 找标题、在该标题与**下一个已知分区标题**之间收集关键词命中，再并集。
//!
//! 「群聊」「聊天记录」这些分区**不一定出现**——某类结果为空时客户端
//! 整个分区都不画。所以截断取的是"标题下方**第一个出现过**的已知标题"，
//! 不要求某个特定分区存在。
//!
//! - 配置的标题一个都找不到 ⇒ 转人工（这一屏根本没有联系人分组）
//! - 各分组下方一个都没匹配上 ⇒ 转人工
//! - 匹配上多个 ⇒ 取**最上面**的那一行（不再转人工，见下）
//!
//! ## 为什么是"包含"而不是逐字相等
//!
//! 下拉里的行常带着附加信息（备注名、微信号），逐字相等会一个都匹配不上。
//! 这是操作者明确要求的判据，代价是**可能**选中一行只是"备注里含这个名字"
//! 的记录——这一条风险由「取最上面」兜着（见下）。
//!
//! ## 多命中为什么取最上面（2026-09-21 操作者两次修正）
//!
//! 实测（`/Users/admin/data/task-a537432a.log`，Mac 微信）：搜「李小明」时
//! 「联系人」分组下方同时有 `李小明` 和 `Jerry-李小明同学` 两行，
//! 原来的"多个就转人工"在这里停下不动——而操作者要的正是那个叫 `李小明` 的人。
//!
//! 先按"取最短"改过一版，操作者随即改成**取最上面**：客户端自己就把
//! 最匹配的那一行排在最前面（`李小明` 在 `Jerry-李小明同学` 之上），
//! 比我们拿文字长短去猜可信。所以取 y 最小的那一行，
//! y 相同（同一行被 OCR 拆成两块）时取靠左的。

use crate::candidates::describe_candidates;
use crate::diagnostics::{Decision, ReplayInput, Verdict};
use crate::ports::{AutomationError, TextBox};

/// 把一行识别结果压成"只留可见字符"的形式，用于**包含**判断。
///
/// ## 为什么必须归一化
///
/// `Windows.Media.Ocr` 经常在字与字之间塞进空格（实测把「外部测试联系人」
/// 读成「外部 测试 联系人」），全角与半角也会混。不归一化的话，
/// "下拉里那一行是否包含输入的关键词"就会因为一个空格而判否——
/// 而现象是"搜出来的联系人一个都没匹配上"，看起来像搜索没生效。
///
/// 只去掉空白与控制字符，**不做同音字/近形字替换**：那是另一回事，
/// 而且会引入"看起来像就算匹配"这种本项目明确拒绝的判据。
pub fn normalize_text(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace() && !c.is_control()).collect()
}

/// 把配置里的「联系人分组标题」拆成多项。
///
/// 分隔符：`/`、`、`、以及任意空白。每项 trim，空项丢掉。
/// 默认值是「联系人 / 最常使用」——Mac 微信上人有时只出现在「最常使用」底下。
pub fn parse_search_contact_group_labels(raw: &str) -> Vec<String> {
    raw.split(|c: char| c == '/' || c == '、' || c.is_whitespace())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// 下拉里除「联系人」之外、用来截断分组的其它已知分区标题。
///
/// 命中某一组之后只往下扫到**下一个**已知分区标题为止，否则会把
/// 「聊天记录」里提到同名的那一行也算进联系人。
const KNOWN_NON_CONTACT_SECTION_HEADERS: &[&str] = &["群聊", "聊天记录", "公众号", "小程序"];

/// 一次下拉判定的产物：**结论 + 轨迹**。
pub struct DropdownJudgement {
    /// 挑中的那一行，或者失败原因。
    pub result: Result<TextBox, AutomationError>,
    /// 这次判定在问什么、按什么判、每个候选为什么。
    ///
    /// [`Decision::step`] 留空，由调用方填——只有调用方知道这一步叫什么。
    pub decision: Decision,
}

/// 一条分组的事实：标题认在了哪一块、这一组的正文到哪里为止。
struct GroupFact {
    label: String,
    /// 标题块的下沿（正文从这里开始）。
    body_top: i32,
    /// 标题是靠"逐字相等"认出来的（否则是靠"包含"）。
    title_exact: bool,
    /// 下一个已知分区标题的 y；`None` = 这一组后面没有别的分区。
    section_end: Option<i32>,
    /// 那个分区标题叫什么（写进理由，用来说明"为什么从这里截断"）。
    section_header: Option<String>,
}

/// 在下拉列表里挑出目标联系人，并留下**每个候选为什么**。
///
/// 判据的完整说明见模块文档。参数与判定同源：
/// 关键词、分组标题原文、最低置信度三者都是判定本身的输入，所以轨迹里也如实记下来
/// （离线重放要拿它们重跑）。
pub fn judge_dropdown(
    boxes: &[TextBox],
    keyword: &str,
    group_labels: &str,
    min_confidence: f32,
) -> DropdownJudgement {
    let labels = parse_search_contact_group_labels(group_labels);
    let groups_desc = labels.join(" / ");
    let needle = normalize_text(keyword);

    // 已知分区标题 = 配置的联系人标签 + 固定的其它分区。
    // 用来在某一组下方截断，避免扫进「聊天记录」等下一组。
    let mut known_headers: Vec<String> = labels.iter().map(|l| normalize_text(l)).collect();
    for header in KNOWN_NON_CONTACT_SECTION_HEADERS {
        let n = normalize_text(header);
        if !known_headers.iter().any(|h| h == &n) {
            known_headers.push(n);
        }
    }

    let mut groups: Vec<GroupFact> = Vec::new();
    // 被认作标题的块（文字块下标, 分组下标），以及被"取最上面"淘汰掉的重复标题。
    let mut titles: Vec<(usize, usize)> = Vec::new();
    let mut duplicate_titles: Vec<(usize, usize)> = Vec::new();
    let mut all_hits: Vec<usize> = Vec::new();

    for label in &labels {
        let group_needle = normalize_text(label);

        // ── 先找出这一组的标题 ────────────────────────────────
        //
        // **先要逐字相等，找不到才退到「包含」**。
        //
        // 只用「包含」是不够的，而且会错得很隐蔽：下拉里的聊天记录行常常长成
        // 「和 张三 的聊天」，它**也**含「联系人」这三个字。于是一旦按
        // "最上面那个含「联系人」的块"去认标题，就可能认到一行聊天记录上，
        // 而它下面根本没有联系人分组——后面整段判据全部错位，
        // 症状是"点到了不相干的一行"，看不出是标题认错了。
        //
        // 逐字相等先命中就轮不到聊天记录行来冒充；退到「包含」是留给
        // OCR 把标题多读出一个字符的情况（那是"包含"要兜的原始场景）。
        // 两级都取**最上面**那一个：同一个词可能因为排版被拆成两块。
        let exact = boxes
            .iter()
            .enumerate()
            .filter(|(_, b)| b.confidence >= min_confidence)
            .filter(|(_, b)| normalize_text(&b.text) == group_needle)
            .min_by_key(|(_, b)| b.bounds.y);
        let (title_exact, title) = match exact {
            Some(found) => (true, Some(found)),
            None => (
                false,
                boxes
                    .iter()
                    .enumerate()
                    .filter(|(_, b)| b.confidence >= min_confidence)
                    .filter(|(_, b)| normalize_text(&b.text).contains(&group_needle))
                    .min_by_key(|(_, b)| b.bounds.y),
            ),
        };
        let group_index = groups.len();
        let Some((title_index, title_box)) = title else {
            continue;
        };
        titles.push((title_index, group_index));
        // 同一分组的标题被读成好几块时只有最上面那块算数，其余如实记下来：
        // "标题重复"在现场看起来就像"下拉里有两个「联系人」"，值得留痕。
        for (i, b) in boxes.iter().enumerate() {
            if i == title_index || b.confidence < min_confidence {
                continue;
            }
            let t = normalize_text(&b.text);
            if t == group_needle || t.contains(&group_needle) {
                duplicate_titles.push((i, group_index));
            }
        }

        let body_top = title_box.bounds.y + title_box.bounds.height;
        // 下一个已知分区标题：只用**归一化后逐字相等**截断。
        // 「包含」会把聊天记录行里碰巧含「群聊」二字的也当成标题，
        // 于是这一组被提前截断，人就从这一组里消失了。
        let section = boxes
            .iter()
            .filter(|b| b.confidence >= min_confidence)
            .filter(|b| b.bounds.y >= body_top)
            .filter(|b| {
                let t = normalize_text(&b.text);
                known_headers.iter().any(|h| t == *h)
            })
            .min_by_key(|b| b.bounds.y);
        let section_end = section.map(|b| b.bounds.y);
        groups.push(GroupFact {
            label: label.clone(),
            body_top,
            title_exact,
            section_end,
            section_header: section.map(|b| b.text.trim().to_string()),
        });

        for (i, b) in boxes.iter().enumerate() {
            if b.confidence < min_confidence || b.bounds.y < body_top {
                continue;
            }
            if section_end.map(|end| b.bounds.y >= end).unwrap_or(false) {
                continue;
            }
            if !normalize_text(&b.text).contains(&needle) {
                continue;
            }
            let dup = all_hits
                .iter()
                .any(|&j| boxes[j].text == b.text && boxes[j].bounds == b.bounds);
            if !dup {
                all_hits.push(i);
            }
        }
    }

    let (result, picked, outcome) = conclude(boxes, &groups, &all_hits, &groups_desc, keyword);
    // 「成没成」取**结论本身**，不另判一遍（§1.3）：`conclude` 已经把成败编进了
    // `result`，这里只是把它抄成结构化的一笔（`result` 马上要被移进返回值）。
    let passed = result.is_ok();
    let candidates = trail(
        boxes,
        &groups,
        &titles,
        &duplicate_titles,
        &all_hits,
        picked,
        keyword,
        &groups_desc,
        min_confidence,
    );

    DropdownJudgement {
        result,
        decision: Decision {
            step: String::new(),
            question: format!("下拉列表里哪一行是目标联系人「{keyword}」？"),
            rule: format!(
                "在「{groups_desc}」分组标题下方、下一个已知分区标题\
                 （{}）之前，取文本包含关键词、置信度 ≥ {min_confidence:.2} 的最上面一行",
                KNOWN_NON_CONTACT_SECTION_HEADERS.join(" / ")
            ),
            outcome,
            passed,
            min_confidence,
            replay: Some(ReplayInput::Dropdown {
                keyword: keyword.to_string(),
                group_labels: group_labels.to_string(),
            }),
            candidates,
        },
    }
}

/// 由"找到了哪些分组、命中了哪些行"得**结论**。
fn conclude(
    boxes: &[TextBox],
    groups: &[GroupFact],
    all_hits: &[usize],
    groups_desc: &str,
    keyword: &str,
) -> (Result<TextBox, AutomationError>, Option<usize>, String) {
    if groups.is_empty() {
        let reason = format!(
            "搜索下拉列表里没有找到「{groups_desc}」中的任何一组，读到 {} 块文字。\
             常见原因：关键词没匹配到任何联系人（下拉里只有聊天记录或群聊），\
             或者「搜索下拉列表」区域标定偏了。",
            boxes.len()
        );
        let outcome = format!("转人工：{}", one_line(&reason));
        return (Err(AutomationError::NeedsHumanReview(reason)), None, outcome);
    }

    let tried = groups
        .iter()
        .map(|g| g.label.as_str())
        .collect::<Vec<_>>()
        .join(" / ");
    match all_hits.len() {
        1 => {
            let index = all_hits[0];
            let outcome = format!("选中「{}」（{}）", boxes[index].text.trim(), groups[0].label);
            (Ok(boxes[index].clone()), Some(index), outcome)
        }
        0 => {
            // 失败时把"这一组下面的文字"一并报出来：只报"没有那一行"，看的人会默认
            // "确实没有这个人"；把原文列出来才分得清是"没搜到"还是"读错了"。
            let min_title_y = boxes
                .iter()
                .filter(|b| {
                    let t = normalize_text(&b.text);
                    groups.iter().any(|g| {
                        let n = normalize_text(&g.label);
                        t == n || t.contains(&n)
                    })
                })
                .map(|b| b.bounds.y)
                .min()
                .unwrap_or(0);
            let below_any: Vec<TextBox> = boxes
                .iter()
                .filter(|b| b.bounds.y > min_title_y)
                .cloned()
                .collect();
            let reason = format!(
                "「{tried}」分组下方没有匹配「{keyword}」的那一行。这一组下方的文字是：{}",
                describe_candidates(&below_any)
            );
            let outcome = format!("转人工：{}", one_line(&reason));
            (Err(AutomationError::NeedsHumanReview(reason)), None, outcome)
        }
        _ => {
            // 多行都含关键词 ⇒ 取**最上面**的那一行（依据见模块文档）。
            //
            // 最上面的那一行就是客户端自己排在最前面的匹配——它比我们
            // 靠文字长短猜要可信：搜「李小明」时 `李小明` 排在
            // `Jerry-李小明同学` 上面。
            let top = all_hits
                .iter()
                .copied()
                .min_by_key(|&i| (boxes[i].bounds.y, boxes[i].bounds.x))
                .expect("all_hits 非空，最上面的那个必然存在");
            let others = all_hits.len() - 1;
            let outcome = format!(
                "选中「{}」（同时命中 {others} 行，取最上面那一行）",
                boxes[top].text.trim()
            );
            (Ok(boxes[top].clone()), Some(top), outcome)
        }
    }
}

/// 给每个候选写一句"它为什么被选中 / 被淘汰"。
///
/// **它只读上面已经算出来的事实**（分组、标题、命中集合），不再自己判一次——
/// 判据仍然只有 `judge_dropdown` 那一条。
#[allow(clippy::too_many_arguments)]
fn trail(
    boxes: &[TextBox],
    groups: &[GroupFact],
    titles: &[(usize, usize)],
    duplicate_titles: &[(usize, usize)],
    all_hits: &[usize],
    picked: Option<usize>,
    keyword: &str,
    groups_desc: &str,
    min_confidence: f32,
) -> Vec<Verdict> {
    // 命中的行里哪一行被选中了：写理由时要说清"同样命中、但它排在下面"。
    let picked_y = picked.map(|i| boxes[i].bounds.y);
    boxes
        .iter()
        .enumerate()
        .map(|(i, b)| {
            if b.confidence < min_confidence {
                // ★ 这一条最要紧：2026-09-21 那次失败里「联系人」三个字明明读到了，
                // 却因为置信度没过阈值而"不存在"。理由里带上两个数字，一眼可对。
                return Verdict::rejected(
                    b,
                    format!(
                        "置信度 {:.2} 低于阈值 {:.2}，没参与判定（阈值由「最低 OCR 置信度」决定）",
                        b.confidence, min_confidence
                    ),
                );
            }
            if Some(i) == picked {
                return Verdict::passed(
                    b,
                    format!("含关键词「{keyword}」，且在命中的行里排在最上面 → 选中"),
                );
            }
            if let Some((_, group)) = titles.iter().find(|(index, _)| *index == i) {
                let fact = &groups[*group];
                let how = if fact.title_exact { "（逐字相等）" } else { "（靠包含命中）" };
                return Verdict::passed(
                    b,
                    format!("被认作「{}」分组的标题行{how}（标题下面才是候选项）", fact.label),
                );
            }
            if let Some((_, group)) = duplicate_titles.iter().find(|(index, _)| *index == i) {
                return Verdict::rejected(
                    b,
                    format!(
                        "与「{}」的标题重复，同一分组只取最上面那一块当标题",
                        groups[*group].label
                    ),
                );
            }
            if let Some(group) = groups.iter().find(|g| in_body(g, b.bounds.y)) {
                if all_hits.contains(&i) {
                    return Verdict::rejected(
                        b,
                        match picked_y {
                            Some(y) if y <= b.bounds.y => format!(
                                "同样含关键词，但它排在被选中那一行（y={y}）下面，按「取最上面」落选"
                            ),
                            _ => "同样含关键词，但不是最上面那一行".to_string(),
                        },
                    );
                }
                return Verdict::rejected(
                    b,
                    format!("在「{}」分组下方，但文本不含关键词「{keyword}」", group.label),
                );
            }
            if let Some(group) = groups.iter().find(|g| beyond_section(g, b.bounds.y)) {
                return Verdict::rejected(
                    b,
                    format!(
                        "在「{}」分组下方、过了「{}」这个分区标题，不再算联系人",
                        group.label,
                        group.section_header.as_deref().unwrap_or("下一个分区")
                    ),
                );
            }
            if groups.is_empty() {
                return Verdict::rejected(
                    b,
                    format!("整屏没有出现「{groups_desc}」的标题，这一块没有参与判定"),
                );
            }
            let found = groups
                .iter()
                .map(|g| g.label.as_str())
                .collect::<Vec<_>>()
                .join(" / ");
            Verdict::rejected(
                b,
                format!("不在「{found}」任何一组的标题下方（也不含关键词「{keyword}」）"),
            )
        })
        .collect()
}

/// 这一行落在某一组的正文里吗（标题下沿之后、分区截断之前）。
fn in_body(group: &GroupFact, y: i32) -> bool {
    y >= group.body_top && group.section_end.map(|end| y < end).unwrap_or(true)
}

/// 这一行落在某一组"已被分区标题截断"的那一段里吗。
fn beyond_section(group: &GroupFact, y: i32) -> bool {
    group.section_end.map(|end| y >= end).unwrap_or(false)
}

/// 把一段可能很长的原因压成一行，供决策记录里的 `outcome` 用。
fn one_line(text: &str) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect();
    flat.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests;