// 发布版（Release）不弹出控制台黑窗口；Debug 版保留控制台，方便看日志输出
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use gpui_kit::assets::AllAssets as Assets;
use gpui_kit::*;

mod crypto;
mod main_view;
mod model;
mod rdp;
mod store;
mod transfer;

fn main() {
    let app = gpui_kit::application().with_assets(Assets);
    app.run(move |cx| {
        gpui_kit::init(cx);
        // 清理上次异常退出遗留的 TERMSRV 凭据
        rdp::cleanup_leftover_credentials();
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(1000.), px(680.)), cx)),
            titlebar: Some(TitlebarOptions {
                title: Some("RDM Rust 远程桌面管理".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| main_view::MainView::new(window, cx))
        })
        .expect("无法创建主窗口");
    });
}
