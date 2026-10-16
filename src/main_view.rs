use std::collections::{HashMap, HashSet};
use std::time::Duration;

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

/// 拖拽负载：被拖动的服务器
#[derive(Clone)]
struct DragServer {
    id: String,
    name: String,
}

impl Render for DragServer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 跟随光标的拖拽预览
        h_flex()
            .px_3()
            .py_1p5()
            .rounded_md()
            .text_sm()
            .bg(cx.theme().popover)
            .text_color(cx.theme().popover_foreground)
            .border_1()
            .border_color(cx.theme().border)
            .shadow_md()
            .child(self.name.clone())
    }
}

/// 拖拽负载：被拖动的分组
#[derive(Clone)]
struct DragGroup {
    name: String,
}

impl Render for DragGroup {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .px_3()
            .py_1p5()
            .rounded_md()
            .text_sm()
            .font_weight(FontWeight::SEMIBOLD)
            .bg(cx.theme().popover)
            .text_color(cx.theme().popover_foreground)
            .border_1()
            .border_color(cx.theme().border)
            .shadow_md()
            .child(self.name.clone())
    }
}

/// 拖拽落点
#[derive(Clone, PartialEq)]
enum DropTarget {
    /// 插入到某台服务器之前（after = false）或之后（after = true）
    Row { id: String, after: bool },
    /// 追加到某个分组末尾
    Group(String),
    /// 重排分组：插到某分组之前（after = false）或之后（after = true）。
    /// `origin` 记录落点由哪个元素设置（分组标题为 None，服务器行为自身 id），
    /// 用于让各元素只清除自己设置的落点。
    GroupOrder {
        group: String,
        after: bool,
        origin: Option<String>,
    },
}

pub struct MainView {
    store: Store,
    selected: Option<String>,
    latency: HashMap<String, Latency>,
    /// 已折叠的分组名（仅本次运行内有效）
    collapsed: HashSet<String>,
    /// 服务器列表滚动控制，绑定滚动条
    list_scroll: ScrollHandle,
    /// 拖拽落点（拖动过程中实时更新）
    drop_target: Option<DropTarget>,
    /// 拖拽到列表边缘时每个 tick 的滚动量（0 表示不滚动）
    auto_scroll: f32,
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
            list_scroll: ScrollHandle::new(),
            drop_target: None,
            auto_scroll: 0.,
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

    // ---------- 拖拽排序 / 换组 ----------

    /// 把落点换算成「目标分组 + 组内插入序号」
    fn drop_position(&self, target: &DropTarget) -> Option<(String, usize)> {
        match target {
            DropTarget::Group(group) => Some((
                group.clone(),
                self.store.servers.iter().filter(|s| &s.group == group).count(),
            )),
            DropTarget::Row { id, after } => {
                let row = self.store.find(id)?;
                let group = row.group.clone();
                let index = self
                    .store
                    .servers
                    .iter()
                    .filter(|s| s.group == group)
                    .take_while(|s| &s.id != id)
                    .count();
                Some((group, index + usize::from(*after)))
            }
            // 分组重排的落点不对服务器生效
            DropTarget::GroupOrder { .. } => None,
        }
    }

    /// 完成放置：移动服务器（换组 + 排序）并持久化
    fn finish_drop(&mut self, dragged_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.drop_target.take();
        self.auto_scroll = 0.;
        let Some(target) = target else {
            return;
        };
        let Some((group, index)) = self.drop_position(&target) else {
            return;
        };
        if !self.store.move_server(dragged_id, &group, index) {
            return;
        }
        if let Err(e) = store::save(&self.store) {
            window.push_notification(Notification::error(format!("保存失败: {e}")), cx);
        }
        cx.notify();
    }

    /// 完成分组重排并持久化
    fn finish_group_drop(&mut self, dragged: &str, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.drop_target.take();
        self.auto_scroll = 0.;
        let Some(DropTarget::GroupOrder { group, after, .. }) = target else {
            return;
        };
        let names = self.store.group_names();
        let Some(pos) = names.iter().position(|g| g == &group) else {
            return;
        };
        let index = pos + usize::from(after);
        if !self.store.move_group(dragged, index) {
            return;
        }
        if let Err(e) = store::save(&self.store) {
            window.push_notification(Notification::error(format!("保存失败: {e}")), cx);
        }
        cx.notify();
    }

    /// 拖拽到列表上下边缘时自动滚动。
    ///
    /// 光标停在边缘不动时不会再有 mouse move 事件，所以用定时任务持续推进。
    /// 监听挂在根元素上，坐标与列表可视区比较：光标离开列表即停止滚动。
    fn update_auto_scroll(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        const EDGE: f32 = 40.;
        const SPEED: f32 = 12.;
        let bounds = self.list_scroll.bounds();
        let dir = if !bounds.contains(&position) {
            0.
        } else if position.y < bounds.top() + px(EDGE) {
            // 向上滚：offset 增大（趋近 0）
            -SPEED
        } else if position.y > bounds.bottom() - px(EDGE) {
            // 向下滚：offset 变小（更负）
            SPEED
        } else {
            0.
        };
        if dir != self.auto_scroll {
            self.auto_scroll = dir;
            if dir != 0. {
                self.start_auto_scroll(cx);
            }
        }
    }

    fn start_auto_scroll(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let executor = cx.background_executor().clone();
            loop {
                executor.timer(Duration::from_millis(16)).await;
                let keep_going = this.update(cx, |this, cx| {
                    if !cx.has_active_drag() {
                        this.auto_scroll = 0.;
                    }
                    if this.auto_scroll == 0. {
                        return false;
                    }
                    let mut offset = this.list_scroll.offset();
                    offset.y = offset.y - px(this.auto_scroll);
                    this.list_scroll.set_offset(offset);
                    cx.notify();
                    true
                });
                if !matches!(keep_going, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
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
        self.store = store::load();
        if let Some(id) = self.selected.take() {
            if self.store.find(&id).is_some() {
                self.selected = Some(id);
            }
        }
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

    // ---------- 删除服务器 ----------

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
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let group_name = group.to_string();
        let collapsed = self.collapsed.contains(group);
        let text_color = cx.theme().muted_foreground;
        let hover_bg = cx.theme().list_hover;
        let drop_bg = cx.theme().list_active;
        // 拖拽悬停在本分组标题上时高亮，表示「放进这个分组」
        let is_drop_group = cx.has_active_drag()
            && matches!(&self.drop_target, Some(DropTarget::Group(g)) if g.as_str() == group);
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
            .when(is_drop_group, |d| d.bg(drop_bg))
            .hover(|d| d.bg(hover_bg))
            .on_drag(
                DragGroup {
                    name: group_name.clone(),
                },
                |drag, _, _, cx| {
                    cx.stop_propagation();
                    cx.new(|_| drag.clone())
                },
            )
            .on_drag_move::<DragServer>(cx.listener({
                let group = group_name.clone();
                move |this, e: &DragMoveEvent<DragServer>, _window, cx| {
                    // on_drag_move 会对所有注册了监听的元素触发（不限于光标所在元素，
                    // 且执行顺序不定），因此只在光标确实落在本标题上时才设置落点；
                    // 否则仅当当前落点仍指向本分组时才清空，避免清掉别处刚设置的落点
                    let hovering = e.bounds.contains(&e.event.position);
                    let target = if hovering {
                        // 落到分组标题上 = 追加到该分组末尾
                        Some(DropTarget::Group(group.clone()))
                    } else if matches!(&this.drop_target, Some(DropTarget::Group(g)) if g == &group) {
                        None
                    } else {
                        return;
                    };
                    if this.drop_target != target {
                        this.drop_target = target;
                        cx.notify();
                    }
                }
            }))
            .on_drag_move::<DragGroup>(cx.listener({
                let group = group_name.clone();
                move |this, e: &DragMoveEvent<DragGroup>, _window, cx| {
                    let hovering = e.bounds.contains(&e.event.position);
                    let target = if hovering {
                        // 拖动的就是本分组时没有意义
                        if e.drag(cx).name == group {
                            None
                        } else {
                            Some(DropTarget::GroupOrder {
                                group: group.clone(),
                                after: e.event.position.y >= e.bounds.center().y,
                                origin: None,
                            })
                        }
                    } else if matches!(&this.drop_target, Some(DropTarget::GroupOrder { group: g, origin: None, .. }) if g == &group)
                    {
                        None
                    } else {
                        return;
                    };
                    if this.drop_target != target {
                        this.drop_target = target;
                        cx.notify();
                    }
                }
            }))
            .on_drop(cx.listener(|this, drag: &DragServer, window, cx| {
                this.finish_drop(&drag.id, window, cx);
            }))
            .on_drop(cx.listener(|this, drag: &DragGroup, window, cx| {
                this.finish_group_drop(&drag.name, window, cx);
            }))
            .on_click({
                let group = group_name.clone();
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    if !this.collapsed.insert(group.clone()) {
                        this.collapsed.remove(&group);
                    }
                    cx.notify();
                })
            })
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let _ = window;
        let theme = cx.theme();
        let selected = self.selected.as_deref() == Some(server.id.as_str());
        let latency = self.latency.get(&server.id).copied();
        let (latency_text, latency_color) = match latency {
            Some(Latency::Checking) | None => ("检测中…".to_string(), theme.muted_foreground),
            Some(Latency::Ok(ms)) => (format!("{ms} ms"), theme.info),
            Some(Latency::Offline) => ("离线".to_string(), theme.danger),
        };
        let sid = server.id.clone();
        // 插入位置指示线：拖拽中且本行是落点时显示
        let drop_marker = match self.drop_target.as_ref() {
            Some(DropTarget::Row { id, after }) if id == &server.id && cx.has_active_drag() => {
                Some(*after)
            }
            _ => None,
        };
        let marker_color = theme.primary;
        h_flex()
            .id(SharedString::from(format!("srv-{}", server.id)))
            .relative()
            .h(px(38.))
            // 左缩进与分组标题文字对齐：12(分组内边距) + 14(小三角) + 4(间距) = 30
            .pl(px(30.))
            .pr_3()
            .items_center()
            .gap_3()
            .rounded_md()
            .when(selected, |d| d.bg(theme.list_active))
            .hover(|d| d.bg(theme.list_hover))
            .on_drag(
                DragServer {
                    id: server.id.clone(),
                    name: server.name.clone(),
                },
                |drag, _, _, cx| {
                    cx.stop_propagation();
                    cx.new(|_| drag.clone())
                },
            )
            .on_drag_move::<DragServer>(cx.listener({
                let sid = server.id.clone();
                move |this, e: &DragMoveEvent<DragServer>, _window, cx| {
                    // on_drag_move 会对所有注册了监听的元素触发（不限于光标所在的那个，
                    // 且执行顺序不定），所以只在光标确实位于本行时设置落点；
                    // 不在本行时，仅当落点仍指向本行才清除，避免清掉其它行刚设置的落点
                    let hovering = e.bounds.contains(&e.event.position);
                    let target = if hovering {
                        // 落在自己身上不算落点
                        if e.drag(cx).id == sid {
                            None
                        } else {
                            Some(DropTarget::Row {
                                id: sid.clone(),
                                after: e.event.position.y >= e.bounds.center().y,
                            })
                        }
                    } else if matches!(&this.drop_target, Some(DropTarget::Row { id, .. }) if id == &sid)
                    {
                        None
                    } else {
                        return;
                    };
                    if this.drop_target != target {
                        this.drop_target = target;
                        cx.notify();
                    }
                }
            }))
            .on_drag_move::<DragGroup>(cx.listener({
                let sid = server.id.clone();
                let group = server.group.clone();
                move |this, e: &DragMoveEvent<DragGroup>, _window, cx| {
                    // 与服务器拖拽同理：只认「光标确实在本行」时设置的落点，
                    // 且只清除本行自己设置的落点
                    let hovering = e.bounds.contains(&e.event.position);
                    let target = if hovering {
                        if e.drag(cx).name == group {
                            None
                        } else {
                            Some(DropTarget::GroupOrder {
                                group: group.clone(),
                                after: e.event.position.y >= e.bounds.center().y,
                                origin: Some(sid.clone()),
                            })
                        }
                    } else if matches!(&this.drop_target, Some(DropTarget::GroupOrder { origin: Some(o), .. }) if o == &sid)
                    {
                        None
                    } else {
                        return;
                    };
                    if this.drop_target != target {
                        this.drop_target = target;
                        cx.notify();
                    }
                }
            }))
            .on_drop(cx.listener(|this, drag: &DragServer, window, cx| {
                this.finish_drop(&drag.id, window, cx);
            }))
            .on_drop(cx.listener(|this, drag: &DragGroup, window, cx| {
                this.finish_group_drop(&drag.name, window, cx);
            }))
            .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                this.selected = Some(sid.clone());
                if ev.click_count() >= 2 {
                    this.connect_server(&sid, window, cx);
                }
                cx.notify();
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
                        div()
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
            .when_some(drop_marker, |d, after| {
                // 绝对定位的 2px 指示线：放上面或下面，不挤动内容
                d.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .h(px(2.))
                        .bg(marker_color)
                        .when(after, |d| d.bottom_0())
                        .when(!after, |d| d.top_0()),
                )
            })
    }

    /// 分组重排的插入指示线（画在分组整块的上/下边界处）
    fn group_order_marker(
        &self,
        group: &str,
        after: bool,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let hit = cx.has_active_drag()
            && matches!(
                &self.drop_target,
                Some(DropTarget::GroupOrder { group: g, after: a, .. }) if g == group && *a == after
            );
        hit.then(|| {
            // 负外边距正好抵消多出的一个 gap 与自身高度，指示线出现时列表不跳动
            div()
                .h(px(2.))
                .mb(px(-6.))
                .rounded_full()
                .bg(cx.theme().primary)
                .into_any_element()
        })
    }

    fn render_list(
        &self,
        view: &Entity<Self>,
        window: &mut Window,
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
        let mut list = v_flex()
            .id("server-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.list_scroll)
            .py_2()
            .px_2()
            .gap_1();
        // 分组重排的插入指示线（按整块分组画：本组之前 / 本组之后）
        for group in self.store.group_names() {
            let servers: Vec<&ServerEntry> = self
                .store
                .servers
                .iter()
                .filter(|s| s.group == group)
                .collect();
            if let Some(marker) = self.group_order_marker(&group, false, cx) {
                list = list.child(marker);
            }
            list = list.child(self.render_group_header(view, &group, servers.len(), cx));
            if self.collapsed.contains(&group) {
                if let Some(marker) = self.group_order_marker(&group, true, cx) {
                    list = list.child(marker);
                }
                continue;
            }
            for server in servers {
                list = list.child(self.render_server_row(view, server, window, cx));
            }
            if let Some(marker) = self.group_order_marker(&group, true, cx) {
                list = list.child(marker);
            }
        }
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
        let view = cx.entity();
        v_flex()
            .id("main-root")
            .size_full()
            // 拖拽监听挂在这里，光标在窗口内任何位置都能更新自动滚动状态
            .on_drag_move::<DragServer>(cx.listener(|this, e: &DragMoveEvent<DragServer>, _w, cx| {
                this.update_auto_scroll(e.event.position, cx);
            }))
            .on_drag_move::<DragGroup>(cx.listener(|this, e: &DragMoveEvent<DragGroup>, _w, cx| {
                this.update_auto_scroll(e.event.position, cx);
            }))
            .child(self.render_toolbar(cx))
            .child(self.render_list(&view, window, cx))
    }
}
