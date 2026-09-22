//! `state` 的用例。
//!
//! 拆成独立文件是因为主体已经接近文件行数上限，而**测试文件不限行数**
//! （`CONVENTIONS.md` §2）。组织方式跟 `calibration/tests.rs` 一致。

use super::*;

#[test]
fn only_the_full_verified_path_can_complete() {
    let mut task = TaskMachine::default();
    for state in [
        TaskState::LaunchingClient,
        TaskState::WaitingForClient,
        TaskState::SearchingContact,
        TaskState::VerifyingCandidate,
        TaskState::VerifyingChatHeader,
        TaskState::PreparingMessage,
        TaskState::Sending,
        TaskState::VerifyingDelivery,
        TaskState::Completed,
    ] {
        task.transition(state).unwrap();
    }
    assert_eq!(task.state(), TaskState::Completed);
}

/// 「先点导航图标切视图」这一步是**可选**的，两条边都必须放行。
///
/// 只留其中一条都会坏：删掉直连边 ⇒ 关掉这个功能之后每次任务都在这里报非法转换；
/// 删掉经 `NavigatingToView` 的边 ⇒ 打开这个功能就再也跑不起来。
/// 这两种失败都发生在任务刚开始的时候，看起来都像"程序坏了"。
#[test]
fn navigating_to_view_is_an_optional_step() {
    let mut direct = TaskMachine::default();
    for state in [TaskState::LaunchingClient, TaskState::WaitingForClient] {
        direct.transition(state).unwrap();
    }
    direct.transition(TaskState::SearchingContact).unwrap();
    assert_eq!(direct.state(), TaskState::SearchingContact);

    let mut via_icon = TaskMachine::default();
    for state in [
        TaskState::LaunchingClient,
        TaskState::WaitingForClient,
        TaskState::NavigatingToView,
        TaskState::SearchingContact,
    ] {
        via_icon.transition(state).unwrap();
    }
    assert_eq!(via_icon.state(), TaskState::SearchingContact);
}

/// 每个状态都必须能从自己的标识**读回来**，并且有非空的说明文案。
///
/// 遍历 [`TaskState::ALL`] 而不是逐个手写：漏了哪个状态，这条用例就会点名报出来。
/// （实测过它的价值：`NavigatingToView` 曾经漏在 `from_str_name` 之外，
/// 症状是审计库里这一条记录的 `state` 列还原不出来。）
#[test]
fn all_variants_round_trip_through_their_identifier() {
    // 先钉住表本身没被漏改：长度与去重后的数量必须一致。
    assert_eq!(
        TaskState::ALL.len(),
        18,
        "状态数量变了——请同时更新 ALL 的长度与内容"
    );
    let mut names: Vec<&str> = TaskState::ALL.iter().map(|s| s.as_str()).collect();
    names.sort_unstable();
    let unique = names.len();
    names.dedup();
    assert_eq!(names.len(), unique, "两个状态用了同一个标识：{names:?}");

    for state in TaskState::ALL {
        let name = state.as_str();
        assert_eq!(
            TaskState::from_str_name(name),
            Some(state),
            "{name} 读不回来 —— `from_str_name` 里漏了这个分支"
        );
        assert!(
            !state.describe().is_empty(),
            "{name} 没有说明文案 —— `describe` 里漏了这个分支"
        );
    }

    // 未知标识必须返回 None，不能悄悄落到某个默认状态上。
    assert_eq!(TaskState::from_str_name("NoSuchState"), None);
    assert_eq!(TaskState::from_str_name(""), None);
}

/// 已退场状态的标识仍要能读回来。
///
/// `AwaitingHumanConfirmation` 在 2026-09-22 取消人工确认后不再产生，
/// 但审计库里已经有一批按它落库的记录。读不回来的后果不是"少显示一个状态"，
/// 而是 `storage` 读那些记录时报 `UnknownState`——整条记录连同这个任务的历史
/// 一起打不开。所以它映射到当时含义最接近的前驱（正文已就绪、还没发出去）。
///
/// 这条用例与 `LaunchingClient` 那条注释同一条约定：**落过库的标识不许读不回来**。
#[test]
fn a_retired_state_identifier_is_still_readable() {
    assert_eq!(
        TaskState::from_str_name("AwaitingHumanConfirmation"),
        Some(TaskState::PreparingMessage),
        "历史审计记录靠这个映射才读得回来"
    );
    // 但它**不能**再被产出：`ALL` 里没有它，状态机也没有一条通到它的边。
    assert!(
        !TaskState::ALL.iter().any(|s| s.as_str() == "AwaitingHumanConfirmation"),
        "这个状态已经退场，不该再出现在 ALL 里"
    );
}

/// 终态集合必须与 `is_terminal` 一致，且**恰好**是那六个。
///
/// 写成遍历而不是 `assert!(X.is_terminal())`：后者只能证明"多算了一个"，
/// 证明不了"少算了一个"——而少算一个的后果是任务已经结束了、
/// 界面却还在转圈等下一步。
#[test]
fn exactly_six_states_are_terminal() {
    let terminal: Vec<&str> = TaskState::ALL
        .iter()
        .filter(|state| state.is_terminal())
        .map(|state| state.as_str())
        .collect();
    assert_eq!(
        terminal,
        vec![
            "Completed",
            "Prepared",
            "Navigated",
            "NeedsHumanReview",
            "Failed",
            "Cancelled"
        ],
        "终态集合变了——先想清楚「界面该不该停止等待」，再改这条断言"
    );
}

/// `Sending` 只能从 `PreparingMessage` 到达，不能凭空跳进去。
///
/// 取消人工确认把这条边缩短了（原来中间还夹一个 `AwaitingHumanConfirmation`），
/// 但**没有**把它变成"任意状态都能发"：发送仍然必须建立在
/// "已经定位到人、正文已就绪"之上。
#[test]
fn sending_can_only_be_reached_from_a_prepared_message() {
    let mut fresh = TaskMachine::default();
    assert_eq!(
        fresh.transition(TaskState::Sending),
        Err(StateError::InvalidTransition { from: TaskState::Draft, to: TaskState::Sending })
    );

    // 反例再取一个"已经走了几步但与消息无关"的位置：核验标题之后直接发也不行，
    // 必须先经过 `PreparingMessage`（那一步才聚焦输入框、记发送前基线）。
    let mut early = TaskMachine::default();
    for state in [
        TaskState::LaunchingClient,
        TaskState::WaitingForClient,
        TaskState::SearchingContact,
        TaskState::VerifyingCandidate,
    ] {
        early.transition(state).unwrap();
    }
    assert_eq!(
        early.transition(TaskState::Sending),
        Err(StateError::InvalidTransition {
            from: TaskState::VerifyingCandidate,
            to: TaskState::Sending
        })
    );
}

#[test]
fn prepared_is_a_terminal_state() {
    assert!(TaskState::Prepared.is_terminal());
    assert_eq!(TaskState::Prepared.as_str(), "Prepared");
    assert_eq!(TaskState::from_str_name("Prepared"), Some(TaskState::Prepared));
}

/// `Prepared` 只能从 `PreparingMessage` 到达。
///
/// 它和 `Completed` 一样表示"某一步正常走完"，**不是**那种任意状态都能跳过去的
/// 失败终态。如果哪天有人把它并进 `NeedsHumanReview | Failed | Cancelled` 那一组，
/// `Draft → Prepared` 这种毫无意义的跳转就会被放行。
#[test]
fn prepared_cannot_be_reached_from_an_unrelated_state() {
    let mut task = TaskMachine::default();
    assert_eq!(
        task.transition(TaskState::Prepared),
        Err(StateError::InvalidTransition { from: TaskState::Draft, to: TaskState::Prepared })
    );
}

#[test]
fn preparing_message_may_end_at_prepared() {
    let mut task = TaskMachine::default();
    for state in [
        TaskState::LaunchingClient,
        TaskState::WaitingForClient,
        TaskState::SearchingContact,
        TaskState::VerifyingCandidate,
        TaskState::VerifyingChatHeader,
        TaskState::PreparingMessage,
    ] {
        task.transition(state).unwrap();
    }
    task.transition(TaskState::Prepared).unwrap();
    assert_eq!(task.state(), TaskState::Prepared);

    // 已经是终态，再想转去 Sending 必须被拒绝。
    assert_eq!(
        task.transition(TaskState::Sending),
        Err(StateError::TerminalState { from: TaskState::Prepared })
    );
}

/// 从 `Prepared` 出发再也发不出消息——这是「只填不发」的安全底线。
#[test]
fn prepared_can_never_be_followed_by_sending() {
    let mut task = TaskMachine::default();
    for state in [
        TaskState::LaunchingClient,
        TaskState::WaitingForClient,
        TaskState::SearchingContact,
        TaskState::VerifyingCandidate,
        TaskState::VerifyingChatHeader,
        TaskState::PreparingMessage,
    ] {
        task.transition(state).unwrap();
    }
    task.transition(TaskState::Prepared).unwrap();

    // 从 Prepared 出发，无论想去哪一步都不行——它是终态。
    for next in [TaskState::Sending, TaskState::VerifyingDelivery, TaskState::Completed] {
        assert_eq!(
            task.transition(next),
            Err(StateError::TerminalState { from: TaskState::Prepared }),
            "Prepared 之后不该还能转到 {next:?}"
        );
    }
    assert_eq!(task.state(), TaskState::Prepared);
}

/// 「只做导航」这条路：点完图标就停在 `Navigated`，不查找任何人。
#[test]
fn navigating_only_ends_at_navigated() {
    let mut task = TaskMachine::default();
    for state in [
        TaskState::LaunchingClient,
        TaskState::WaitingForClient,
        TaskState::NavigatingToView,
    ] {
        task.transition(state).unwrap();
    }
    task.transition(TaskState::Navigated).unwrap();
    assert_eq!(task.state(), TaskState::Navigated);
    assert!(TaskState::Navigated.is_terminal());
    assert_eq!(TaskState::Navigated.describe(), "已切换视图，未打开会话");
}

/// `Navigated` 和 `Prepared` 一样，**不是**那种"任意状态都能跳过去"的失败终态。
///
/// 如果哪天有人把它并进 `NeedsHumanReview | Failed | Cancelled` 那一组，
/// `Draft → Navigated` 这种毫无意义的跳转就会被放行。
#[test]
fn navigated_cannot_be_reached_from_an_unrelated_state() {
    let mut task = TaskMachine::default();
    assert_eq!(
        task.transition(TaskState::Navigated),
        Err(StateError::InvalidTransition { from: TaskState::Draft, to: TaskState::Navigated })
    );
}

/// 搜索式查找的那一段：候选 → 资料页 → 从资料页进聊天 → 核验标题。
///
/// 这条链是**新加**的（旧路径是候选之后直接核验聊天标题），
/// 少任何一条边都会让搜索式查找跑到一半报"非法转换"——
/// 而那时的现象是"任务莫名其妙失败了"，看不出是状态机少了一条边。
#[test]
fn the_search_flow_goes_through_the_profile_page() {
    let mut task = TaskMachine::default();
    for state in [
        TaskState::LaunchingClient,
        TaskState::WaitingForClient,
        TaskState::NavigatingToView,
        TaskState::SearchingContact,
        TaskState::VerifyingCandidate,
        TaskState::VerifyingProfile,
        TaskState::OpeningChatFromProfile,
        TaskState::VerifyingChatHeader,
        TaskState::PreparingMessage,
    ] {
        task.transition(state).unwrap();
    }
    task.transition(TaskState::Prepared).unwrap();
    assert_eq!(task.state(), TaskState::Prepared);
}

/// 点下拉那一行**直接进了已有的会话**那条支路：资料页 → 核验标题，中间那一步没有。
///
/// **为什么必须钉住**：这是界面的两种落点，不是流程的两个版本。缺这条边时，
/// 任务会在真实客户端"直接打开历史对话"的那一次点击之后报"非法转换"，
/// 而现象是"任务莫名其妙失败了"——看不出是状态机少了一条边，
/// 也看不出是"这个联系人本来就有会话"。
#[test]
fn the_search_flow_can_skip_the_profile_entry_when_the_chat_is_already_open() {
    let mut task = TaskMachine::default();
    for state in [
        TaskState::LaunchingClient,
        TaskState::WaitingForClient,
        TaskState::NavigatingToView,
        TaskState::SearchingContact,
        TaskState::VerifyingCandidate,
        TaskState::VerifyingProfile,
        // 这里没有 `OpeningChatFromProfile`：客户端直接打开了那份聊天记录。
        TaskState::VerifyingChatHeader,
        TaskState::PreparingMessage,
    ] {
        task.transition(state).unwrap();
    }
    task.transition(TaskState::Prepared).unwrap();
    assert_eq!(task.state(), TaskState::Prepared);
}

/// 资料页那两步**不能**从半路插进来：它们只接在"已选中候选人"之后。
#[test]
fn the_profile_step_requires_a_verified_candidate() {
    let mut task = TaskMachine::default();
    for state in [
        TaskState::LaunchingClient,
        TaskState::WaitingForClient,
        TaskState::SearchingContact,
    ] {
        task.transition(state).unwrap();
    }
    assert_eq!(
        task.transition(TaskState::VerifyingProfile),
        Err(StateError::InvalidTransition {
            from: TaskState::SearchingContact,
            to: TaskState::VerifyingProfile
        })
    );
}
