//! **标定清单本身**：要标哪几个界面、每个界面里框哪几块。
//!
//! 这里是纯数据。清单与代码分开放，是因为两者的**改动节奏完全不同**：
//! 加一项、改一句引导语是「业务上的调整」，改 `plan()` 的组装方式是「实现上的调整」。
//! 混在一个文件里时，每次调清单都要在一个五百行的文件里翻找。
//!
//! ## 怎么加一项
//!
//! 1. 在 [`ITEMS`] 里加一条，`scene` 指向 [`SCENES`] 里的某个 `id`；
//! 2. 选好 `storage`（见 [`MarkStorage`]）：
//!    - 编排层**已经在读**的具名区域 → `Core`；
//!    - 其余一律 `Extra`。**不要为了"看起来完整"给 `Extra` 补默认值**，
//!      `None`（还没标）是如实的状态，猜一个默认值会让人以为已经标好了。
//! 3. 界面上不需要改任何东西——它把清单当数据渲染。
//!
//! ## 改清单前先想一件事
//!
//! 配置是**持久化**的，清单是**代码**。改了 key、删了项之后，
//! 旧配置里就会留下读不到的键，而且它们会挡住保存
//! （`set_runtime_config` 拒绝未知 key）。出口是
//! [`super::stale_keys`] / [`super::prune_stale_marks`]，界面上有对应入口。
//! 所以：**改 key 是"破坏性"改动**，能不改就不改。

use super::{CoreRegion, ItemSpec, MarkStorage, SceneSpec};

/// 界面状态。**顺序即界面上的顺序**。
pub const SCENES: &[SceneSpec] = &[
    SceneSpec {
        id: "main",
        label: "主界面",
        instruction: "让客户端停在主界面：左侧竖排导航图标、顶部搜索框、下面会话列表，\
                      右侧是内容展示区。不要停在搜索面板或任何弹层上——\
                      截图上必须同时看得见这四个区域。",
    },
    SceneSpec {
        id: "history",
        label: "历史对话",
        instruction: "点开一个历史对话，让聊天页显示出来：顶部是对话标题，\
                      中间是对话内容，底部是输入框与发送按钮。\
                      左侧要能看到对话列表，截图时别把它裁掉。",
    },
    SceneSpec {
        id: "contacts",
        label: "联系人列表",
        instruction: "点导航区的「联系人」图标切过去。左侧要能看到搜索框与联系人名单，\
                      右侧是选中联系人的资料面板。没有选中任何人时，\
                      先点一个联系人让资料面板显示出来再截图。",
    },
    SceneSpec {
        id: "search_dropdown",
        label: "搜索下拉列表",
        instruction: "点开搜索框，让下拉列表弹出来（能看到历史对话或联系人候选）。\
                      输入框里留着词不要清掉——清掉之后面板会消失。",
    },
];

/// 全部标定项。**顺序即界面上的编号**。
///
/// 编号按场景分段（`1.1`、`2.3`…），与命令行探针的 `1`~`4` 是**两套**，
/// 见模块文档「两套编号，别混」。
pub const ITEMS: &[ItemSpec] = &[
    // ── 1. 主界面 ──────────────────────────────────────────────────────
    ItemSpec {
        key: "nav_bar",
        scene: "main",
        label: "导航区",
        hint: "左侧竖排那一列图标（历史对话 / 联系人 / 收藏 / 发现 …）。\
               这些图标上没有文字，OCR 读不到，只能靠图标模板匹配来点。\
               框的时候只圈图标那一列——把旁边的文字或未读红点一起圈进来，\
               模板就会带着它们，换个未读数就匹配不上。",
        required: false,
        storage: MarkStorage::Extra,
    },
    ItemSpec {
        key: "main_search",
        scene: "main",
        label: "搜索框区",
        hint: "主界面顶部的搜索入口。左右要盖住整条框的可点击范围——\
               点偏到框外就点不进去，而界面上看起来只是「点了一下没反应」。\
               这与历史对话、联系人列表里那两个搜索框是三个不同的控件，分别标。",
        required: false,
        storage: MarkStorage::Extra,
    },
    ItemSpec {
        key: "list_area",
        scene: "main",
        label: "列表区",
        hint: "左侧的会话列表，OCR 要在这里找到唯一逐字匹配的姓名。\
               找不到、或找到多个，任务都会转人工处理。\
               左边界要让开导航图标栏与头像列——头像上的未读红点会被 OCR 并进姓名里\
               （实测把「李四」读成「0 李四」），而姓名是逐字精确匹配，多一个字符就永远找不到人。",
        required: true,
        storage: MarkStorage::Core(CoreRegion::ContactPanel),
    },
    ItemSpec {
        key: "content_area",
        scene: "main",
        label: "右侧内容展示区域",
        hint: "主界面右侧那一整块。用来判断「当前有没有打开任何对话」——\
               没有打开时右侧是空白或欢迎页，此时不该继续往下走。",
        required: false,
        storage: MarkStorage::Extra,
    },
    // ── 2. 历史对话 ────────────────────────────────────────────────────
    ItemSpec {
        key: "history_search",
        scene: "history",
        label: "搜索框区",
        hint: "聊天页顶部的搜索入口。与主界面那个是两个位置，分别框、分别存。",
        required: false,
        storage: MarkStorage::Extra,
    },
    ItemSpec {
        key: "history_list",
        scene: "history",
        label: "对话历史列表区域",
        hint: "左侧的对话列表（有对话名字与消息摘要的那几行）。\
               整块盖住——只盖住前几行的话，排在后面的对话会被当成「不存在」。",
        required: false,
        storage: MarkStorage::Extra,
    },
    ItemSpec {
        key: "chat_header",
        scene: "history",
        label: "对话标题",
        hint: "聊天页顶部那一行标题。用它做第二次姓名核验：\
               列表里点开的那个人，与标题上写的人必须是同一个。\
               只框标题那一行就够，别盖到下面的消息区。",
        required: true,
        storage: MarkStorage::Core(CoreRegion::ChatHeader),
    },
    ItemSpec {
        key: "chat_body",
        scene: "history",
        label: "对话内容",
        hint: "消息正文那一整块。发送前后各截一次：既要看到画面变化，\
               也要在里面识别出这条消息本身。",
        required: true,
        storage: MarkStorage::Core(CoreRegion::ChatBody),
    },
    ItemSpec {
        key: "composer",
        scene: "history",
        label: "发送消息输入框",
        hint: "粘贴前会先点这里。少了这一步，文字可能粘进搜索框而不是输入框。",
        required: true,
        storage: MarkStorage::Core(CoreRegion::Composer),
    },
    ItemSpec {
        key: "send_button",
        scene: "history",
        label: "发送按钮",
        hint: "输入框右下角那个发送按钮。只框按钮本身，别把旁边那块空白一起圈进来——\
               点击落点取框的中心，框大了中心就落到按钮外面，\
               而现象只是「点了一下没反应」。",
        required: false,
        storage: MarkStorage::Extra,
    },
    // ── 3. 联系人列表 ──────────────────────────────────────────────────
    ItemSpec {
        key: "contacts_search",
        scene: "contacts",
        label: "搜索框区",
        hint: "联系人页顶部的搜索入口。第三个搜索框，同样单独框、单独存。",
        required: false,
        storage: MarkStorage::Extra,
    },
    ItemSpec {
        key: "contacts_list",
        scene: "contacts",
        label: "联系人列表区域",
        hint: "联系人页左侧的名单。要整块盖住——只盖住前几行的话，\
               排在后面的联系人会被当成「不存在」。",
        required: false,
        storage: MarkStorage::Extra,
    },
    ItemSpec {
        key: "contact_profile",
        scene: "contacts",
        label: "联系人资料区域",
        hint: "右侧的资料面板（头像、名字、备注等）。\
               用于在点开联系人之后核对「这是不是要找的那个人」——\
               姓名重名时，靠这一块才分得清。",
        required: false,
        storage: MarkStorage::Extra,
    },
    // ── 4. 搜索下拉列表 ────────────────────────────────────────────────
    ItemSpec {
        key: "search_dropdown",
        scene: "search_dropdown",
        label: "下拉列表区域",
        hint: "搜索框下面弹出来的整个面板，从第一行结果一直到面板底边。\
               编排时要在这块区域里找目标联系人，所以上下都要盖住——\
               只盖住前几行的话，排在后面的联系人会被当成「不存在」。",
        required: false,
        storage: MarkStorage::Extra,
    },
];
