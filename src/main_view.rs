use std::collections::{HashMap, HashSet};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputContentType, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::select::{Select, SelectState};
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme as _, WindowExt as _};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::model::{new_id, ServerEntry, Store, DEFAULT_GROUP};
use crate::{crypto, rdp, store, transfer};

const NEW_GROUP_ITEM: &str = "＋ 新建分组…";

#[derive(Clone, Copy)]
enum Latency {
    Checking,
    Ok(u128),
    Offline,
}

impl From<Option<u128>> for Latency {
    fn from(v: Option<u128>) -> Self {
        match v {
            Some(ms) => Latency::Ok(ms),
            None => Latency::Offline,
        }
    }
}

// ---------- 拖拽 ----------

/// 正在被拖动的服务器
#[derive(Clone)]
struct DragServer {
    id: String,
    name: String,
}

/// 正在被拖动的分组
#[derive(Clone)]
struct DragGroup {
    name: String,
}

/// 拖拽落点：一个「插入缝隙」。
///
/// 用槽位而不是「在某个元素前/后」来表示，是为了让同一个缝隙只有一种表示：
/// 「在 X 之后」和「在 X 的下一项之前」是同一个缝隙，落点相同就不会来回切换、重复重绘。
#[derive(Clone, PartialEq)]
enum DropSpot {
    /// 服务器落进该分组的第 `index` 个槽位（0 = 该组第一项之前）
    Server {
        group: String,
        index: usize,
        /// 光标是否正压在该分组的标题上（用于高亮标题）
        on_header: bool,
    },
    /// 分组落进分组列表的第 `index` 个槽位
    Group { index: usize },
}

/// 跟着光标走的拖拽影子
struct DragGhost {
    icon: IconName,
    label: SharedString,
}

impl Render for DragGhost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .px_2()
            .py_1()
            .gap_2()
            .items_center()
            .rounded_md()
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().drag_border)
            .shadow_md()
            .text_sm()
            .child(
                // 图标固定尺寸、不参与收缩，避免被后面的长名字挤压而产生偏移
                div()
                    .flex_shrink_0()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.icon),
            )
            .child(
                // 名字强制单行，过长就省略：保证影子始终只有一行，图标与文字垂直居中对齐
                div()
                    .max_w(px(240.))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(self.label.clone()),
            )
    }
}

/// 2px 指示边框的颜色：落点在这里时用主题色，否则全透明。
///
/// 边框宽度恒定、只切换颜色，因此拖拽过程中元素的位置和尺寸完全不变。
fn drop_border(active: bool, color: Hsla) -> Hsla {
    if active {
        color
    } else {
        transparent_black()
    }
}

/// 列表末尾的落点指示条（表示「插到整个列表最后」），同样始终存在、只切换颜色
fn drop_tail(active: bool, color: Hsla) -> impl IntoElement {
    div()
        .h(px(2.))
        .rounded_sm()
        .bg(drop_border(active, color))
}

/// 服务器编辑对话框中的实体集合
struct ServerEditor {
    edit_id: Option<String>,
    name: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    username: Entity<InputState>,
    password: Entity<InputState>,
    group: Entity<SelectState<Vec<SharedString>>>,
    new_group: Entity<InputState>,
}

struct PendingImport {
    file: transfer::ExportFile,
    passwords: Vec<String>,
}

/// 对话框内的校验提示。
///
/// 用独立 Entity 渲染：notify 后它自身重绘，即使对话框层被缓存也能显示，
/// 从而避免 toast 被对话框遮住。
struct ErrorNotice {
    message: Option<SharedString>,
}

impl Render for ErrorNotice {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 固定高度：提示出现时对话框布局不变，父级缓存的布局才不会把文字裁掉
        h_flex()
            .h(px(20.))
            .gap_1()
            .items_center()
            .overflow_hidden()
            .text_sm()
            .text_color(cx.theme().danger)
            .when_some(self.message.clone(), |this, msg| this.child(msg))
    }
}

pub struct MainView {
    store: Store,
    selected: Option<String>,
    latency: HashMap<String, Latency>,
    /// 已折叠的分组名（仅本次运行内有效）
    collapsed: HashSet<String>,
    /// 当前拖拽的落点（没有拖拽时为 None）
    drop_spot: Option<DropSpot>,
    /// 服务器列表滚动控制，绑定滚动条
    list_scroll: ScrollHandle,
    editor: Option<ServerEditor>,
    error_notice: Entity<ErrorNotice>,
    group_input: Option<Entity<InputState>>,
    export_input: Option<Entity<InputState>>,
    import_password_input: Option<Entity<InputState>>,
    pending_import: Option<PendingImport>,
}

impl MainView {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let store = store::load();
        let servers = store.servers.clone();
        let mut this = Self {
            store,
            selected: None,
            latency: HashMap::new(),
            collapsed: HashSet::new(),
            drop_spot: None,
            list_scroll: ScrollHandle::new(),
            editor: None,
            error_notice: cx.new(|_| ErrorNotice { message: None }),
            group_input: None,
            export_input: None,
            import_password_input: None,
            pending_import: None,
        };
        this.start_latency_check(servers, cx);
        this
    }

    /// 在对话框内显示校验提示（toast 会被对话框层遮住）
    fn set_dialog_error(&mut self, msg: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.error_notice.update(cx, |notice, cx| {
            notice.message = Some(msg.into());
            cx.notify();
        });
    }

    fn clear_dialog_error(&mut self, cx: &mut Context<Self>) {
        self.error_notice.update(cx, |notice, cx| {
            notice.message = None;
            cx.notify();
        });
    }

    // ---------- 延迟检测 ----------

    fn start_latency_check(&mut self, servers: Vec<ServerEntry>, cx: &mut Context<Self>) {
        for s in &servers {
            self.latency.insert(s.id.clone(), Latency::Checking);
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let results = cx
                .background_executor()
                .spawn(async move { rdp::check_all(&servers) })
                .await;
            _ = this.update(cx, |this, cx| {
                for (id, r) in results {
                    this.latency.insert(id, Latency::from(r));
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ---------- 连接 ----------

    fn connect_server(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(server) = self.store.find(id).cloned() else {
            return;
        };
        let password = if server.password_enc.is_empty() {
            String::new()
        } else {
            match crypto::dpapi_decrypt(&server.password_enc) {
                Ok(p) => p,
                Err(_) => {
                    window.push_notification(
                        Notification::error("密码解密失败（仅本机当前用户可解密）"),
                        cx,
                    );
                    return;
                }
            }
        };
        rdp::connect(server.clone(), password);
        window.push_notification(
            Notification::info(format!("正在连接 {}（{}）…", server.name, server.host)),
            cx,
        );
    }

    fn connect_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected.clone() else {
            window.push_notification(Notification::warning("请先在列表中选择服务器"), cx);
            return;
        };
        self.connect_server(&id, window, cx);
    }

    // ---------- 刷新 ----------

    fn refresh(&mut self, cx: &mut Context<Self>) {
        // id 是每次读取时在内存里重新生成的，所以用「名称 + 地址」把选中项找回来
        let previous = self
            .selected
            .as_ref()
            .and_then(|id| self.store.find(id))
            .map(|s| (s.name.clone(), s.host.clone()));
        self.store = store::load();
        self.latency.clear();
        self.selected = previous.and_then(|(name, host)| {
            self.store
                .servers
                .iter()
                .find(|s| s.name == name && s.host == host)
                .map(|s| s.id.clone())
        });
        let servers = self.store.servers.clone();
        self.start_latency_check(servers, cx);
    }

    // ---------- 服务器编辑对话框 ----------

    fn open_server_editor(
        &mut self,
        edit_id: Option<String>,
        preset_group: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let existing = edit_id.as_ref().and_then(|id| self.store.find(id)).cloned();
        let is_edit = existing.is_some();

        let name = cx.new(|cx| {
            let mut s = InputState::new(window, cx).placeholder("例如：生产服务器");
            if let Some(e) = &existing {
                s = s.default_value(e.name.clone());
            }
            s
        });
        let host = cx.new(|cx| {
            let mut s = InputState::new(window, cx).placeholder("IP 地址或域名");
            if let Some(e) = &existing {
                s = s.default_value(e.host.clone());
            }
            s
        });
        let port = cx.new(|cx| {
            let mut s = InputState::new(window, cx).placeholder("默认 3389");
            if let Some(e) = &existing {
                s = s.default_value(e.port.to_string());
            }
            s
        });
        let username = cx.new(|cx| {
            let mut s = InputState::new(window, cx).placeholder("例如：Administrator");
            if let Some(e) = &existing {
                s = s.default_value(e.username.clone());
            }
            s
        });
        let password = cx.new(|cx| {
            let mut s = InputState::new(window, cx).masked(true);
            s = if is_edit {
                s.placeholder("留空则保持不变")
            } else {
                s.placeholder("可为空，连接时手动输入")
            };
            s
        });

        let mut items: Vec<SharedString> = self
            .store
            .group_names()
            .iter()
            .map(|g| SharedString::from(g.clone()))
            .collect();
        items.push(SharedString::from(NEW_GROUP_ITEM));
        let preset = existing
            .as_ref()
            .map(|e| e.group.clone())
            .or(preset_group)
            .unwrap_or_else(|| DEFAULT_GROUP.to_string());
        let group = cx.new(|cx| SelectState::new(items, None, window, cx));
        group.update(cx, |s, cx| {
            s.set_selected_value(&SharedString::from(preset), window, cx)
        });
        let new_group = cx.new(|cx| InputState::new(window, cx).placeholder("新分组名称"));

        self.clear_dialog_error(cx);
        self.editor = Some(ServerEditor {
            edit_id: edit_id.clone(),
            name: name.clone(),
            host: host.clone(),
            port: port.clone(),
            username: username.clone(),
            password: password.clone(),
            group: group.clone(),
            new_group: new_group.clone(),
        });

        let error_notice = self.error_notice.clone();
        let view = cx.entity();
        let title = if is_edit {
            "编辑服务器"
        } else {
            "新增服务器"
        };
        window.open_dialog(cx, move |dialog, _window, cx| {
            let creating_group = group
                .read(cx)
                .selected_value()
                .map(|v| v.as_ref() == NEW_GROUP_ITEM)
                .unwrap_or(false);
            let label_color = cx.theme().muted_foreground;
            let row = |label: &'static str, content: AnyElement| {
                v_flex()
                    .gap_1()
                    .child(div().text_sm().text_color(label_color).child(label))
                    .child(content)
            };
            let form = v_flex()
                .gap_3()
                .child(row("分组", Select::new(&group).w_full().into_any_element()))
                .when(creating_group, |f| {
                    f.child(row("新分组名称", Input::new(&new_group).into_any_element()))
                })
                .child(row("服务器名称", Input::new(&name).into_any_element()))
                .child(row(
                    "服务器地址（IP 或域名）",
                    Input::new(&host).into_any_element(),
                ))
                .child(row("端口", Input::new(&port).into_any_element()))
                .child(row("用户名", Input::new(&username).into_any_element()))
                .child(row(
                    "密码",
                    Input::new(&password)
                        .content_type(InputContentType::Password)
                        .mask_toggle()
                        .into_any_element(),
                ));
            dialog
                .title(title)
                .w(px(440.))
                .child(form)
                .child(error_notice.clone())
                .footer(
                    DialogFooter::new()
                        .child(
                            DialogClose::new()
                                .trigger(|b| b.outline().icon(IconName::X).label("取消")),
                        )
                        .child(
                            DialogAction::new().child(
                                Button::new("ok")
                                    .primary()
                                    .icon(IconName::Check)
                                    .label("确定"),
                            ),
                        ),
                )
                .on_ok({
                    let view = view.clone();
                    move |_, window, cx| {
                        view.update(cx, |this, cx| this.commit_server_editor(window, cx))
                    }
                })
        });
    }

    fn commit_server_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(editor) = self.editor.take() else {
            return true;
        };
        let fail = |this: &mut Self,
                    editor: ServerEditor,
                    _window: &mut Window,
                    cx: &mut Context<Self>,
                    msg: &str| {
            this.set_dialog_error(msg.to_string(), cx);
            this.editor = Some(editor);
            false
        };

        let mut name = editor.name.read(cx).value().trim().to_string();
        let host = editor.host.read(cx).value().trim().to_string();
        if host.is_empty() {
            return fail(self, editor, window, cx, "请输入服务器地址");
        }
        if name.is_empty() {
            if editor.edit_id.is_some() {
                return fail(self, editor, window, cx, "请输入服务器名称");
            }
            // 新增服务器时名称为空，自动用服务器地址作为名称
            name = host.clone();
        }
        let port_text = editor.port.read(cx).value().trim().to_string();
        let port: u16 = if port_text.is_empty() {
            3389
        } else {
            match port_text.parse() {
                Ok(p) => p,
                Err(_) => return fail(self, editor, window, cx, "端口必须是 1-65535 的数字"),
            }
        };

        let selected_group = editor.group.read(cx).selected_value().cloned();
        let group = match selected_group.as_ref().map(|v| v.as_ref()) {
            Some(NEW_GROUP_ITEM) => editor.new_group.read(cx).value().trim().to_string(),
            Some(g) => g.to_string(),
            None => DEFAULT_GROUP.to_string(),
        };
        if group.is_empty() {
            return fail(self, editor, window, cx, "请输入分组名称");
        }

        let password_input = editor.password.read(cx).value().to_string();
        let password_enc = if !password_input.is_empty() {
            match crypto::dpapi_encrypt(&password_input) {
                Ok(enc) => enc,
                Err(e) => {
                    return fail(self, editor, window, cx, &format!("密码加密失败: {e}"));
                }
            }
        } else if let Some(id) = &editor.edit_id {
            self.store
                .find(id)
                .map(|s| s.password_enc.clone())
                .unwrap_or_default()
        } else {
            String::new()
        };

        let username = editor.username.read(cx).value().trim().to_string();
        self.store.add_group(&group);
        let id = editor.edit_id.clone().unwrap_or_else(new_id);
        let entry = ServerEntry {
            id: id.clone(),
            group,
            name,
            host,
            port,
            username,
            password_enc,
        };
        self.store.upsert_server(entry.clone());
        if let Err(e) = store::save(&self.store) {
            window.push_notification(Notification::error(format!("保存失败: {e}")), cx);
        }
        self.selected = Some(id);
        self.start_latency_check(vec![entry], cx);
        cx.notify();
        true
    }

    // ---------- 拖拽排序 ----------

    /// 拖拽落点判定：鼠标既要压在元素上，也要落在列表可视区域内。
    /// 滚动出视野的行在几何上仍可能盖住鼠标位置（比如压在工具栏上），不能当成落点。
    fn drop_hovered(&self, bounds: &Bounds<Pixels>, position: &Point<Pixels>) -> bool {
        bounds.contains(position) && self.list_scroll.bounds().contains(position)
    }

    /// 记录当前落点。落点没变时不重绘，避免拖拽过程中的无谓刷新。
    fn set_drop_spot(&mut self, spot: DropSpot, cx: &mut Context<Self>) {
        if self.drop_spot.as_ref() != Some(&spot) {
            self.drop_spot = Some(spot);
            cx.notify();
        }
    }

    /// 服务器拖拽：把落点落到 `group` 组的 `index` 号槽位
    fn drop_server(&mut self, drag: &DragServer, window: &mut Window, cx: &mut Context<Self>) {
        let Some(DropSpot::Server { group, index, .. }) = self.drop_spot.take() else {
            cx.notify();
            return;
        };
        // 落到折叠的分组时展开它，让用户看得到结果
        self.collapsed.remove(&group);
        if self.store.move_server_to(&drag.id, &group, index) {
            if let Err(e) = store::save(&self.store) {
                window.push_notification(Notification::error(format!("保存失败: {e}")), cx);
            }
        }
        cx.notify();
    }

    /// 分组拖拽：把落点落到分组列表的 `index` 号槽位
    fn drop_group(&mut self, drag: &DragGroup, window: &mut Window, cx: &mut Context<Self>) {
        let Some(DropSpot::Group { index }) = self.drop_spot.take() else {
            cx.notify();
            return;
        };
        if self.store.move_group_to(&drag.name, index) {
            if let Err(e) = store::save(&self.store) {
                window.push_notification(Notification::error(format!("保存失败: {e}")), cx);
            }
        }
        cx.notify();
    }

    // ---------- 删除服务器 ----------

    /// 把服务器移到所在分组的顶端
    fn move_to_top(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.store.move_server_to_top(id) {
            return;
        }
        if let Err(e) = store::save(&self.store) {
            window.push_notification(Notification::error(format!("保存失败: {e}")), cx);
        }
        cx.notify();
    }

    /// 把服务器移到所在分组的底端
    fn move_to_bottom(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.store.move_server_to_bottom(id) {
            return;
        }
        if let Err(e) = store::save(&self.store) {
            window.push_notification(Notification::error(format!("保存失败: {e}")), cx);
        }
        cx.notify();
    }

    /// 把分组移到分组列表的顶端
    fn move_group_to_top(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.store.move_group_to_top(name) {
            return;
        }
        if let Err(e) = store::save(&self.store) {
            window.push_notification(Notification::error(format!("保存失败: {e}")), cx);
        }
        cx.notify();
    }

    /// 把分组移到分组列表的底端
    fn move_group_to_bottom(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.store.move_group_to_bottom(name) {
            return;
        }
        if let Err(e) = store::save(&self.store) {
            window.push_notification(Notification::error(format!("保存失败: {e}")), cx);
        }
        cx.notify();
    }

    fn open_delete_server_confirm(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(server) = self.store.find(&id).cloned() else {
            return;
        };
        let view = cx.entity();
        window.open_alert_dialog(cx, move |a, _window, _cx| {
            a.confirm()
                .title("删除服务器")
                .description(format!(
                    "确定要删除服务器「{}」吗？此操作不可恢复。",
                    server.name
                ))
                .footer(
                    DialogFooter::new()
                        .child(
                            DialogClose::new()
                                .trigger(|b| b.outline().icon(IconName::X).label("取消")),
                        )
                        .child(
                            DialogAction::new().child(
                                Button::new("ok")
                                    .primary()
                                    .icon(IconName::Trash)
                                    .label("确定"),
                            ),
                        ),
                )
                .on_ok({
                    let view = view.clone();
                    let id = id.clone();
                    move |_, window, cx| {
                        view.update(cx, |this, cx| {
                            this.store.remove_server(&id);
                            this.latency.remove(&id);
                            if this.selected.as_deref() == Some(id.as_str()) {
                                this.selected = None;
                            }
                            if let Err(e) = store::save(&this.store) {
                                window.push_notification(
                                    Notification::error(format!("保存失败: {e}")),
                                    cx,
                                );
                            }
                            cx.notify();
                        });
                        true
                    }
                })
        });
    }

    fn delete_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected.clone() else {
            window.push_notification(Notification::warning("请先在列表中选择服务器"), cx);
            return;
        };
        self.open_delete_server_confirm(id, window, cx);
    }

    // ---------- 分组 ----------

    fn open_add_group_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("分组名称"));
        self.group_input = Some(input.clone());
        self.clear_dialog_error(cx);
        let error_notice = self.error_notice.clone();
        let view = cx.entity();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            dialog
                .title("新增分组")
                .w(px(360.))
                .child(Input::new(&input))
                .child(error_notice.clone())
                .footer(
                    DialogFooter::new()
                        .child(
                            DialogClose::new()
                                .trigger(|b| b.outline().icon(IconName::X).label("取消")),
                        )
                        .child(
                            DialogAction::new().child(
                                Button::new("ok")
                                    .primary()
                                    .icon(IconName::Check)
                                    .label("确定"),
                            ),
                        ),
                )
                .on_ok({
                    let view = view.clone();
                    move |_, window, cx| {
                        view.update(cx, |this, cx| {
                            let Some(input) = this.group_input.take() else {
                                return true;
                            };
                            let name = input.read(cx).value().trim().to_string();
                            if name.is_empty() {
                                this.set_dialog_error("请输入分组名称", cx);
                                this.group_input = Some(input);
                                return false;
                            }
                            if !this.store.add_group(&name) {
                                this.set_dialog_error("分组已存在", cx);
                                this.group_input = Some(input);
                                return false;
                            }
                            if let Err(e) = store::save(&this.store) {
                                window.push_notification(
                                    Notification::error(format!("保存失败: {e}")),
                                    cx,
                                );
                            }
                            cx.notify();
                            true
                        })
                    }
                })
        });
    }

    fn open_rename_group_dialog(
        &mut self,
        group: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("新分组名称")
                .default_value(group.clone())
        });
        self.group_input = Some(input.clone());
        self.clear_dialog_error(cx);
        let error_notice = self.error_notice.clone();
        let view = cx.entity();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            dialog
                .title("重命名分组")
                .w(px(360.))
                .child(Input::new(&input))
                .child(error_notice.clone())
                .footer(
                    DialogFooter::new()
                        .child(
                            DialogClose::new()
                                .trigger(|b| b.outline().icon(IconName::X).label("取消")),
                        )
                        .child(
                            DialogAction::new().child(
                                Button::new("ok")
                                    .primary()
                                    .icon(IconName::Check)
                                    .label("确定"),
                            ),
                        ),
                )
                .on_ok({
                    let view = view.clone();
                    let group = group.clone();
                    move |_, window, cx| {
                        view.update(cx, |this, cx| {
                            let Some(input) = this.group_input.take() else {
                                return true;
                            };
                            let new_name = input.read(cx).value().trim().to_string();
                            if new_name.is_empty() {
                                this.set_dialog_error("请输入分组名称", cx);
                                this.group_input = Some(input);
                                return false;
                            }
                            if !this.store.rename_group(&group, &new_name) {
                                this.set_dialog_error("重命名失败（名称为空或已存在）", cx);
                                this.group_input = Some(input);
                                return false;
                            }
                            if let Err(e) = store::save(&this.store) {
                                window.push_notification(
                                    Notification::error(format!("保存失败: {e}")),
                                    cx,
                                );
                            }
                            cx.notify();
                            true
                        })
                    }
                })
        });
    }

    fn open_delete_group_confirm(
        &mut self,
        group: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if group == DEFAULT_GROUP {
            window.push_notification(Notification::warning("默认分组不可删除"), cx);
            return;
        }
        let view = cx.entity();
        window.open_alert_dialog(cx, move |a, _window, _cx| {
            a.confirm()
                .title("删除分组")
                .description(format!("删除分组「{group}」后，组内服务器将移入默认分组。"))
                .footer(
                    DialogFooter::new()
                        .child(
                            DialogClose::new()
                                .trigger(|b| b.outline().icon(IconName::X).label("取消")),
                        )
                        .child(
                            DialogAction::new().child(
                                Button::new("ok")
                                    .primary()
                                    .icon(IconName::Trash)
                                    .label("确定"),
                            ),
                        ),
                )
                .on_ok({
                    let view = view.clone();
                    let group = group.clone();
                    move |_, window, cx| {
                        view.update(cx, |this, cx| {
                            this.store.delete_group(&group);
                            if let Err(e) = store::save(&this.store) {
                                window.push_notification(
                                    Notification::error(format!("保存失败: {e}")),
                                    cx,
                                );
                            }
                            cx.notify();
                        });
                        true
                    }
                })
        });
    }

    // ---------- 导出 ----------

    fn open_export_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("留空则不导出密码")
        });
        self.export_input = Some(input.clone());
        let view = cx.entity();
        window.open_dialog(cx, move |dialog, _window, cx| {
            dialog
                .title("导出连接")
                .w(px(400.))
                .child(
                    v_flex()
                        .gap_3()
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(
                                "设置导出密码后，服务器密码将加密写入导出文件；留空则不导出密码。",
                            ),
                        )
                        .child(
                            Input::new(&input)
                                .content_type(InputContentType::Password)
                                .mask_toggle(),
                        ),
                )
                .footer(
                    DialogFooter::new()
                        .child(
                            DialogClose::new()
                                .trigger(|b| b.outline().icon(IconName::X).label("取消")),
                        )
                        .child(
                            DialogAction::new().child(
                                Button::new("ok")
                                    .primary()
                                    .icon(IconName::Save)
                                    .label("选择保存位置"),
                            ),
                        ),
                )
                .on_ok({
                    let view = view.clone();
                    move |_, window, cx| {
                        view.update(cx, |this, cx| {
                            let password = this
                                .export_input
                                .take()
                                .map(|i| i.read(cx).value().to_string())
                                .unwrap_or_default();
                            this.do_export(password, window, cx);
                            true
                        })
                    }
                })
        });
    }

    fn do_export(&mut self, export_password: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("JSON", &["json"])
            .set_file_name("connections-export.json")
            .save_file()
        else {
            return;
        };
        let result = transfer::build_export(&self.store, &export_password)
            .and_then(|f| serde_json::to_string_pretty(&f).map_err(Into::into))
            .and_then(|text| std::fs::write(&path, text).map_err(Into::into));
        match result {
            Ok(()) => window.push_notification(
                Notification::success(format!("已导出到 {}", path.display())),
                cx,
            ),
            Err(e) => window.push_notification(Notification::error(format!("导出失败: {e}")), cx),
        }
    }

    // ---------- 导入 ----------

    fn start_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("JSON", &["json"])
            .pick_file()
        else {
            return;
        };
        let file = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| transfer::parse_export(&text).ok());
        match file {
            None => {
                window.push_notification(Notification::error("导入失败：文件格式无效"), cx);
            }
            Some(f) if f.encrypted => {
                self.pending_import = Some(PendingImport {
                    file: f,
                    passwords: Vec::new(),
                });
                self.open_import_password_dialog(window, cx);
            }
            Some(f) => {
                let passwords = vec![String::new(); f.servers.len()];
                self.pending_import = Some(PendingImport { file: f, passwords });
                self.proceed_import(window, cx);
            }
        }
    }

    fn open_import_password_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("导出时设置的密码")
        });
        self.import_password_input = Some(input.clone());
        self.clear_dialog_error(cx);
        let error_notice = self.error_notice.clone();
        let view = cx.entity();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            dialog
                .title("输入导出密码")
                .w(px(360.))
                .child(
                    Input::new(&input)
                        .content_type(InputContentType::Password)
                        .mask_toggle(),
                )
                .child(error_notice.clone())
                .footer(
                    DialogFooter::new()
                        .child(
                            DialogClose::new()
                                .trigger(|b| b.outline().icon(IconName::X).label("取消")),
                        )
                        .child(
                            DialogAction::new().child(
                                Button::new("ok")
                                    .primary()
                                    .icon(IconName::Check)
                                    .label("确定"),
                            ),
                        ),
                )
                .on_ok({
                    let view = view.clone();
                    move |_, window, cx| {
                        view.update(cx, |this, cx| {
                            let Some(input) = this.import_password_input.take() else {
                                return true;
                            };
                            let password = input.read(cx).value().to_string();
                            let Some(pending) = this.pending_import.take() else {
                                return true;
                            };
                            match transfer::decrypt_passwords(&pending.file, &password) {
                                Ok(passwords) => {
                                    this.pending_import = Some(PendingImport {
                                        file: pending.file,
                                        passwords,
                                    });
                                    this.proceed_import(window, cx);
                                    true
                                }
                                Err(e) => {
                                    // 保留待导入数据，用户改密码后可直接重试
                                    this.pending_import = Some(pending);
                                    this.set_dialog_error(format!("解密失败: {e}"), cx);
                                    false
                                }
                            }
                        })
                    }
                })
                .on_cancel({
                    let view = view.clone();
                    move |_, _window, cx| {
                        view.update(cx, |this, _cx| {
                            this.pending_import = None;
                        });
                        true
                    }
                })
        });
    }

    /// 密码就绪后：检查重名，决定直接导入或弹冲突对话框
    fn proceed_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = &self.pending_import else {
            return;
        };
        let conflicts = transfer::count_conflicts(&self.store, &pending.file);
        if conflicts == 0 {
            self.apply_pending_import(false, window, cx);
            return;
        }
        let view = cx.entity();
        window.open_dialog(cx, move |dialog, _window, cx| {
            dialog
                .title("处理重名服务器")
                .w(px(400.))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("导入数据中有 {conflicts} 个服务器与现有列表重名。")),
                )
                .footer(
                    DialogFooter::new()
                        .child(
                            DialogClose::new()
                                .trigger(|b| b.outline().icon(IconName::X).label("取消")),
                        )
                        .child(
                            Button::new("skip")
                                .outline()
                                .icon(IconName::SkipForward)
                                .label("跳过重名")
                                .on_click({
                                    let view = view.clone();
                                    move |_, window, cx| {
                                        view.update(cx, |this, cx| {
                                            this.apply_pending_import(false, window, cx)
                                        });
                                        window.close_dialog(cx);
                                    }
                                }),
                        )
                        .child(
                            Button::new("overwrite")
                                .primary()
                                .icon(IconName::RefreshCw)
                                .label("覆盖重名")
                                .on_click({
                                    let view = view.clone();
                                    move |_, window, cx| {
                                        view.update(cx, |this, cx| {
                                            this.apply_pending_import(true, window, cx)
                                        });
                                        window.close_dialog(cx);
                                    }
                                }),
                        ),
                )
                .on_cancel({
                    let view = view.clone();
                    move |_, _window, cx| {
                        view.update(cx, |this, _cx| {
                            this.pending_import = None;
                        });
                        true
                    }
                })
        });
    }

    fn apply_pending_import(
        &mut self,
        overwrite: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self.pending_import.take() else {
            return;
        };
        match transfer::apply_import(
            &mut self.store,
            &pending.file,
            &pending.passwords,
            overwrite,
        ) {
            Ok((added, overwritten, skipped)) => {
                if let Err(e) = store::save(&self.store) {
                    window.push_notification(Notification::error(format!("保存失败: {e}")), cx);
                }
                let servers = self.store.servers.clone();
                self.start_latency_check(servers, cx);
                window.push_notification(
                    Notification::success(format!(
                        "导入完成：新增 {added}，覆盖 {overwritten}，跳过 {skipped}"
                    )),
                    cx,
                );
            }
            Err(e) => {
                window.push_notification(Notification::error(format!("导入失败: {e}")), cx);
            }
        }
        cx.notify();
    }

    // ---------- 渲染 ----------

    fn render_toolbar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .px_3()
            .py_2()
            .gap_2()
            .flex_wrap()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("add-group")
                    .outline()
                    .icon(IconName::FolderPlus)
                    .label("新增分组")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_add_group_dialog(window, cx);
                    })),
            )
            .child(
                Button::new("add-server")
                    .icon(IconName::Plus)
                    .label("新增服务器")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_server_editor(None, None, window, cx);
                    })),
            )
            .child(
                Button::new("edit-server")
                    .outline()
                    .icon(IconName::Pencil)
                    .label("编辑服务器")
                    .on_click(cx.listener(|this, _, window, cx| {
                        let Some(id) = this.selected.clone() else {
                            window.push_notification(
                                Notification::warning("请先在列表中选择服务器"),
                                cx,
                            );
                            return;
                        };
                        this.open_server_editor(Some(id), None, window, cx);
                    })),
            )
            .child(
                Button::new("del-server")
                    .outline()
                    .icon(IconName::Trash)
                    .label("删除服务器")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.delete_selected(window, cx);
                    })),
            )
            .child(
                Button::new("connect")
                    .outline()
                    .icon(IconName::Monitor)
                    .label("连接")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.connect_selected(window, cx);
                    })),
            )
            .child(
                Button::new("refresh")
                    .outline()
                    .icon(IconName::RefreshCw)
                    .label("刷新")
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.refresh(cx);
                    })),
            )
            .child(
                Button::new("import")
                    .outline()
                    .icon(IconName::Download)
                    .label("导入")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.start_import(window, cx);
                    })),
            )
            .child(
                Button::new("export")
                    .outline()
                    .icon(IconName::Upload)
                    .label("导出")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_export_dialog(window, cx);
                    })),
            )
    }

    fn render_group_header(
        &self,
        view: &Entity<Self>,
        group: &str,
        count: usize,
        // 该分组在分组列表中的位置（第几个分组）
        group_slot: usize,
        // 该标题的上边框是否是当前落点（要画指示线）
        marker: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let group_name = group.to_string();
        let collapsed = self.collapsed.contains(group);
        let text_color = cx.theme().muted_foreground;
        let hover_bg = cx.theme().list_hover;
        let drop_bg = cx.theme().drop_target;
        let drag_line = cx.theme().drag_border;
        // 服务器拖到标题上 = 移到该分组最前面，标题高亮
        let into = matches!(
            self.drop_spot.as_ref(),
            Some(DropSpot::Server { group: g, on_header: true, .. }) if g == group
        );
        h_flex()
            .id(SharedString::from(format!("grp-{group}")))
            .px_3()
            .py_1p5()
            .gap_1()
            .items_center()
            .rounded_md()
            .cursor_pointer()
            .text_sm()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(text_color)
            // 2px 上边框永远保留，只切换颜色：布局恒定，拖拽时不会有任何跳动
            .border_t_2()
            .border_color(drop_border(marker, drag_line))
            .when(into, |d| d.bg(drop_bg))
            .hover(|d| d.bg(hover_bg))
            .on_click({
                let group = group_name.clone();
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    if !this.collapsed.insert(group.clone()) {
                        this.collapsed.remove(&group);
                    }
                    cx.notify();
                })
            })
            // 分组标题本身可以拖动，用来调整分组顺序
            .on_drag(
                DragGroup {
                    name: group_name.clone(),
                },
                |drag, _offset, _window, cx| {
                    cx.new(|_| DragGhost {
                        icon: IconName::Folder,
                        label: drag.name.clone().into(),
                    })
                },
            )
            .on_drag_move::<DragGroup>(cx.listener({
                let slot = group_slot;
                move |this, ev: &DragMoveEvent<DragGroup>, _window, cx| {
                    if !this.drop_hovered(&ev.bounds, &ev.event.position) {
                        return;
                    }
                    // 落在上半部分 = 插到本分组之前，下半部分 = 之后
                    let index = if ev.event.position.y < ev.bounds.center().y {
                        slot
                    } else {
                        slot + 1
                    };
                    this.set_drop_spot(DropSpot::Group { index }, cx);
                }
            }))
            .on_drop::<DragGroup>(cx.listener(|this, drag: &DragGroup, window, cx| {
                this.drop_group(drag, window, cx);
            }))
            // 服务器拖到分组标题上 = 移到该分组最前面
            .on_drag_move::<DragServer>(cx.listener({
                let group = group_name.clone();
                move |this, ev: &DragMoveEvent<DragServer>, _window, cx| {
                    if !this.drop_hovered(&ev.bounds, &ev.event.position) {
                        return;
                    }
                    this.set_drop_spot(
                        DropSpot::Server {
                            group: group.clone(),
                            index: 0,
                            on_header: true,
                        },
                        cx,
                    );
                }
            }))
            .on_drop::<DragServer>(cx.listener(|this, drag: &DragServer, window, cx| {
                this.drop_server(drag, window, cx);
            }))
            .child(if collapsed {
                IconName::ChevronRight
            } else {
                IconName::ChevronDown
            })
            .child(format!("{group}（{count}）"))
            .context_menu({
                let view = view.clone();
                let group_name = group_name.clone();
                move |menu, _window, _cx| {
                    menu.min_w(px(160.))
                        .item(
                            PopupMenuItem::new("新增服务器")
                                .icon(IconName::Plus)
                                .on_click({
                                    let view = view.clone();
                                    let group = group_name.clone();
                                    move |_, window, cx| {
                                        view.update(cx, |this, cx| {
                                            this.open_server_editor(
                                                None,
                                                Some(group.clone()),
                                                window,
                                                cx,
                                            )
                                        });
                                    }
                                }),
                        )
                        .item(
                            PopupMenuItem::new("重命名分组")
                                .icon(IconName::PenLine)
                                .on_click({
                                    let view = view.clone();
                                    let group = group_name.clone();
                                    move |_, window, cx| {
                                        view.update(cx, |this, cx| {
                                            this.open_rename_group_dialog(group.clone(), window, cx)
                                        });
                                    }
                                }),
                        )
                        .item(
                            PopupMenuItem::new("移到顶端")
                                .icon(IconName::ArrowUpToLine)
                                .on_click({
                                    let view = view.clone();
                                    let group = group_name.clone();
                                    move |_, window, cx| {
                                        view.update(cx, |this, cx| {
                                            this.move_group_to_top(&group, window, cx)
                                        });
                                    }
                                }),
                        )
                        .item(
                            PopupMenuItem::new("移到底端")
                                .icon(IconName::ArrowDownToLine)
                                .on_click({
                                    let view = view.clone();
                                    let group = group_name.clone();
                                    move |_, window, cx| {
                                        view.update(cx, |this, cx| {
                                            this.move_group_to_bottom(&group, window, cx)
                                        });
                                    }
                                }),
                        )
                        .item(
                            PopupMenuItem::new("删除分组")
                                .icon(IconName::Trash)
                                .on_click({
                                    let view = view.clone();
                                    let group = group_name.clone();
                                    move |_, window, cx| {
                                        view.update(cx, |this, cx| {
                                            this.open_delete_group_confirm(
                                                group.clone(),
                                                window,
                                                cx,
                                            )
                                        });
                                    }
                                }),
                        )
                }
            })
    }

    fn render_server_row(
        &self,
        view: &Entity<Self>,
        server: &ServerEntry,
        // 该服务器在所属分组中的位置（第几项）
        row_slot: usize,
        // 所属分组在分组列表中的位置（第几个分组）
        group_slot: usize,
        // 该行的上边框是否是当前落点（要画指示线）
        marker: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let selected = self.selected.as_deref() == Some(server.id.as_str());
        let latency = self.latency.get(&server.id).copied();
        let (latency_text, latency_color) = match latency {
            Some(Latency::Checking) | None => ("检测中…".to_string(), theme.muted_foreground),
            Some(Latency::Ok(ms)) => (format!("{ms} ms"), theme.info),
            Some(Latency::Offline) => ("离线".to_string(), theme.danger),
        };
        let drag_line = theme.drag_border;
        let sid = server.id.clone();
        h_flex()
            .id(SharedString::from(format!("srv-{}", server.id)))
            .h(px(38.))
            // 左缩进与分组标题文字对齐：12(分组内边距) + 14(小三角) + 4(间距) = 30
            .pl(px(30.))
            .pr_3()
            .items_center()
            .gap_3()
            .rounded_md()
            // 2px 上边框永远保留，只切换颜色：布局恒定，拖拽时不会有任何跳动
            .border_t_2()
            .border_color(drop_border(marker, drag_line))
            .when(selected, |d| d.bg(theme.list_active))
            .hover(|d| d.bg(theme.list_hover))
            .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                this.selected = Some(sid.clone());
                if ev.click_count() >= 2 {
                    this.connect_server(&sid, window, cx);
                }
                cx.notify();
            }))
            // 拖动服务器行：组内排序，或拖到别的分组里
            .on_drag(
                DragServer {
                    id: server.id.clone(),
                    name: server.name.clone(),
                },
                |drag, _offset, _window, cx| {
                    cx.new(|_| DragGhost {
                        icon: IconName::Monitor,
                        label: drag.name.clone().into(),
                    })
                },
            )
            .on_drag_move::<DragServer>(cx.listener({
                let group = server.group.clone();
                move |this, ev: &DragMoveEvent<DragServer>, _window, cx| {
                    if !this.drop_hovered(&ev.bounds, &ev.event.position) {
                        return;
                    }
                    // 落在上半部分 = 插到本行之前，下半部分 = 本行之后
                    let index = if ev.event.position.y < ev.bounds.center().y {
                        row_slot
                    } else {
                        row_slot + 1
                    };
                    this.set_drop_spot(
                        DropSpot::Server {
                            group: group.clone(),
                            index,
                            on_header: false,
                        },
                        cx,
                    );
                }
            }))
            .on_drop::<DragServer>(cx.listener(|this, drag: &DragServer, window, cx| {
                this.drop_server(drag, window, cx);
            }))
            // 拖动分组经过本行时，按本行所在分组显示插入位置
            .on_drag_move::<DragGroup>(cx.listener({
                let slot = group_slot;
                move |this, ev: &DragMoveEvent<DragGroup>, _window, cx| {
                    if !this.drop_hovered(&ev.bounds, &ev.event.position) {
                        return;
                    }
                    let index = if ev.event.position.y < ev.bounds.center().y {
                        slot
                    } else {
                        slot + 1
                    };
                    this.set_drop_spot(DropSpot::Group { index }, cx);
                }
            }))
            .on_drop::<DragGroup>(cx.listener(|this, drag: &DragGroup, window, cx| {
                this.drop_group(drag, window, cx);
            }))
            .context_menu({
                let view = view.clone();
                let sid = server.id.clone();
                move |menu, _window, _cx| {
                    menu.min_w(px(160.))
                        .item(
                            PopupMenuItem::new("连接")
                                .icon(IconName::Monitor)
                                .on_click({
                                    let view = view.clone();
                                    let sid = sid.clone();
                                    move |_, window, cx| {
                                        view.update(cx, |this, cx| {
                                            this.connect_server(&sid, window, cx)
                                        });
                                    }
                                }),
                        )
                        .item(
                            PopupMenuItem::new("编辑服务器")
                                .icon(IconName::Pencil)
                                .on_click({
                                    let view = view.clone();
                                    let sid = sid.clone();
                                    move |_, window, cx| {
                                        view.update(cx, |this, cx| {
                                            this.open_server_editor(
                                                Some(sid.clone()),
                                                None,
                                                window,
                                                cx,
                                            )
                                        });
                                    }
                                }),
                        )
                        .item(
                            PopupMenuItem::new("移到顶端")
                                .icon(IconName::ArrowUpToLine)
                                .on_click({
                                    let view = view.clone();
                                    let sid = sid.clone();
                                    move |_, window, cx| {
                                        view.update(cx, |this, cx| {
                                            this.move_to_top(&sid, window, cx)
                                        });
                                    }
                                }),
                        )
                        .item(
                            PopupMenuItem::new("移到底端")
                                .icon(IconName::ArrowDownToLine)
                                .on_click({
                                    let view = view.clone();
                                    let sid = sid.clone();
                                    move |_, window, cx| {
                                        view.update(cx, |this, cx| {
                                            this.move_to_bottom(&sid, window, cx)
                                        });
                                    }
                                }),
                        )
                        .item(
                            PopupMenuItem::new("删除服务器")
                                .icon(IconName::Trash)
                                .on_click({
                                    let view = view.clone();
                                    let sid = sid.clone();
                                    move |_, window, cx| {
                                        view.update(cx, |this, cx| {
                                            this.open_delete_server_confirm(sid.clone(), window, cx)
                                        });
                                    }
                                }),
                        )
                }
            })
            .child(
                // 名称列整体保持 180 宽，图标与名称间距收紧为 8，避免挤压后续列
                h_flex()
                    .w(px(180.))
                    .gap_2()
                    .items_center()
                    .child(
                        // 图标不参与收缩，避免被后面的长名字挤压
                        div()
                            .flex_shrink_0()
                            .text_color(theme.muted_foreground)
                            .child(IconName::Monitor),
                    )
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(server.name.clone()),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_color(theme.muted_foreground)
                    .child(format!("{}:{}", server.host, server.port)),
            )
            .child(
                div()
                    .w(px(140.))
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_color(theme.muted_foreground)
                    .child(server.username.clone()),
            )
            .child(
                div()
                    .w(px(90.))
                    .text_right()
                    .text_color(latency_color)
                    .child(latency_text),
            )
    }

    fn render_list(
        &self,
        view: &Entity<Self>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        if self.store.servers.is_empty() && self.store.groups.len() <= 1 {
            return div()
                .flex()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .text_color(cx.theme().muted_foreground)
                .child("暂无服务器，点击工具栏「新增服务器」添加")
                .into_any_element();
        }
        let drag_line = cx.theme().drag_border;
        let group_names = self.store.group_names();
        let group_count = group_names.len();
        let mut list = v_flex()
            .id("server-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.list_scroll)
            .py_2()
            .px_2()
            .gap_1()
            // 落在列表的空白处（行与行之间的缝隙、最后一行下方）也要能完成拖拽：用最后记录的落点
            .on_drop::<DragServer>(cx.listener(|this, drag: &DragServer, window, cx| {
                this.drop_server(drag, window, cx);
            }))
            .on_drop::<DragGroup>(cx.listener(|this, drag: &DragGroup, window, cx| {
                this.drop_group(drag, window, cx);
            }));
        // 上一个分组的（名字, 成员数）：服务器「追加到上一个分组末尾」和「插到本分组最前面」
        // 是同一个缝隙，都画在本标题的上边框上，这样同一个缝隙永远只有一种画法。
        let mut prev: Option<(String, usize)> = None;
        for (group_slot, group) in group_names.iter().enumerate() {
            let servers: Vec<&ServerEntry> = self
                .store
                .servers
                .iter()
                .filter(|s| s.group == *group)
                .collect();
            let count = servers.len();
            let header_marker = match self.drop_spot.as_ref() {
                Some(DropSpot::Group { index }) => *index == group_slot,
                Some(DropSpot::Server { group, index, .. }) => prev
                    .as_ref()
                    .is_some_and(|(name, count)| name == group && *index >= *count),
                None => false,
            };
            list = list.child(self.render_group_header(
                view,
                group,
                count,
                group_slot,
                header_marker,
                cx,
            ));
            if self.collapsed.contains(group) {
                prev = Some((group.clone(), count));
                continue;
            }
            for (row_slot, server) in servers.into_iter().enumerate() {
                let marker = matches!(
                    self.drop_spot.as_ref(),
                    Some(DropSpot::Server { group: g, index, .. })
                        if g == group && *index == row_slot
                );
                list = list.child(self.render_server_row(
                    view, server, row_slot, group_slot, marker, cx,
                ));
            }
            prev = Some((group.clone(), count));
        }
        // 列表末尾的指示条：表示「插到整个列表最后」
        let tail_marker = match self.drop_spot.as_ref() {
            Some(DropSpot::Group { index }) => *index >= group_count,
            Some(DropSpot::Server { group, index, .. }) => prev
                .as_ref()
                .is_some_and(|(name, count)| name == group && *index >= *count),
            None => false,
        };
        list = list.child(drop_tail(tail_marker, drag_line));
        // 外层 relative 容器叠加自定义滚动条；内层列表受 flex 约束才会内部溢出滚动
        v_flex()
            .flex_1()
            .min_h_0()
            .relative()
            .overflow_hidden()
            .child(list)
            .child(Scrollbar::vertical(&self.list_scroll))
            .into_any_element()
    }
}

impl Render for MainView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 拖拽在列表外松手（没有触发任何 on_drop）时，落点提示要跟着一起清掉
        if self.drop_spot.is_some() && !cx.has_active_drag() {
            self.drop_spot = None;
        }
        let view = cx.entity();
        v_flex()
            .id("main-root")
            .size_full()
            .child(self.render_toolbar(cx))
            .child(self.render_list(&view, window, cx))
    }
}

