/// 调试输出门控：dev 构建走 stderr；release 编译为空（参数只求值类型、不执行）。
/// 终端数据/会话路径里的日志（zmodem 帧、PTY 字节流等）不得裸用 `eprintln!`，
/// 否则发布版会把用户终端内容泄进 stderr。名字用 `debug!`（非 `dbg!`）：
/// 根 prelude 的 `std::dbg!` 是 2021 内置宏，同名会把它遮蔽掉。
/// macro_rules! 按文本顺序生效，必须定义在所有 `mod` 之前才对全 crate 可见。
#[cfg(debug_assertions)]
macro_rules! debug {
    ($($arg:tt)*) => { eprintln!($($arg)*) };
}

#[cfg(not(debug_assertions))]
macro_rules! debug {
    // `cfg!` 是编译期常量，release 下整个 if 被消除；不引用任何捕获变量，
    // 无拼写检查/死存储负担，也避开 `format_args!` 不能作语句的限制。
    ($($arg:tt)*) => { if ::core::cfg!(debug_assertions) { ::core::panic!($($arg)*) } };
}

mod ai;
// mem0 在 ai 模块内
mod commands;
mod data_location;
mod db;
mod error;
mod files;
mod local;
mod lock;
mod mosh;
mod platform;
mod plugin;
mod sanitize;
mod sealed;
mod serial;
mod session;
mod ssh;
mod ssh_config;
mod state;
mod telnet;
mod vault;
mod watchdog;
mod zmodem;

use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

use state::AppState;

/// 主窗口 label：与 tauri.conf.json `app.windows[0]` 同源（未显式写 label 时 Tauri 默认 `main`），
/// 单实例回调与 setup 里的图标设置都按这个名字找窗。
const MAIN_WINDOW: &str = "main";

/// 二次启动（仅桌面端）：把已开的主窗口唤到前台；主窗口已被关完但进程仍在收尾时按配置重建一个。
///
/// 走的是与「确认弹窗时把主窗口叫到前台」同一条路径（unminimize → show → set_focus），
/// 聚焦被系统策略挡住时由 WebView2/Windows 自行退化为任务栏按钮闪烁。
#[cfg(desktop)]
fn focus_or_recreate_main(app: &tauri::AppHandle) {
    if let Some(win) = app.get_webview_window(MAIN_WINDOW) {
        let _ = win.unminimize();
        let _ = win.show();
        let _ = win.set_focus();
        return;
    }
    let _ = WebviewWindowBuilder::new(app, MAIN_WINDOW, WebviewUrl::default())
        .title("SailTerm")
        .inner_size(1280.0, 800.0)
        .min_inner_size(960.0, 600.0)
        .decorations(false)
        .shadow(true)
        // drag-drop 保持默认开启（与 tauri.conf.json 同义），不显式设：
        // builder 的对应方法只在 Windows 编入，跨平台写法反而多余。
        .build();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut builder = tauri::Builder::default();
    // 单实例锁必须**最先**注册（官方要求：先于其他插件运行，避免被干扰）。它只在首个实例里
    // 持有 OS 级命名互斥体；后续启动的进程在回调里直接退出，压根不创建 WebView，因此不会再
    // 出现「第二个进程抢同一份 EBWebView 数据目录 → 白屏卡数十秒」，也不会并发读写同一份
    // SQLite / 保险库。
    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            focus_or_recreate_main(app);
        }));
    }
    builder
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        // OS 通知横幅（macOS 通知中心）：应用内 toast 之外的「离屏也能看到」通道，前端经 lib/tauri/notify 消费
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        // 启动即装 ring 作为 rustls 全局 crypto provider（reqwest 用 rustls-no-provider 编译），
        // 必须先于任何 rig provider client 的构造；再打开数据库（跑迁移）与保险库，托管为全局状态。
        .setup(|app| {
            ai::install_crypto_provider();
            local::suppress_child_error_dialogs();
            local::warmup_conpty();
            // 外部编辑的临时副本可能含远端配置/密钥，启动时清扫 7 天前的残留
            commands::file::sweep_edit_tmp();
            // 打包上传崩溃遗留的本机临时包（sailterm_*.tgz），启动时清扫 1 小时前的残留
            files::bundle::sweep_local_tmp();
            // 无边框窗口在部分 Windows 环境（远程桌面/任务栏缓存）下任务栏按钮图标可能不显示，
            // 显式把打包图标设到主窗口，保证任务栏图标稳定。
            if let Some(icon) = app.default_window_icon().cloned() {
                if let Some(win) = app.get_webview_window(MAIN_WINDOW) {
                    let _ = win.set_icon(icon);
                }
            }
            let default_dir = app.path().app_data_dir()?;
            // 解析生效数据目录：若有待执行的迁移（pending），此处（开库前）完成拷贝并改指。
            let dir = data_location::resolve(&default_dir);
            app.manage(AppState::open(&dir)?);
            // 锁屏空闲看门狗（见 commands::lock）：聚合各窗口心跳，达阈值全窗一并锁定。
            commands::lock::start_watch(app.handle().clone());
            // 冻结诊断看门狗（观测 + 死锁自救）：双心跳区分「主线程被钉」与「tokio worker
            // 被同步阻塞耗尽」，命中即写 <数据目录>/sailterm-hang.log；「仅 tokio 停」持续 15s
            // 且主线程健康时（死锁特征），经主线程弹原生对话框供用户一键重启（见 watchdog.rs）。
            watchdog::start(app.handle(), &dir);
            Ok(())
        })
        // 包装一层 invoke 分发：在交给生成的路由前登记命令派发（供冻结看门狗在「两者皆停」
        // 时点名钉死源）。登记仅一次 map 更新，不改变任何既有分发行为。
        .invoke_handler({
            let ipc: Box<dyn Fn(tauri::ipc::Invoke<tauri::Wry>) -> bool + Send + Sync + 'static> =
                Box::new(tauri::generate_handler![
            commands::app::app_info,
            commands::app::security_status,
            commands::app::set_devtools,
            commands::app::data_dir_check,
            commands::app::data_dir_move,
            commands::app::open_data_dir,
            commands::app::reveal_local_path,
            commands::app::open_local_path,
            commands::app::restart_app,
            commands::profiles::profile_list,
            commands::profiles::ssh_config_parse,
            commands::profiles::profile_get,
            commands::profiles::profile_save,
            commands::profiles::profile_delete,
            commands::profiles::profile_duplicate,
            commands::profiles::group_list,
            commands::profiles::group_create,
            commands::profiles::group_delete,
            commands::profiles::group_rename,
            commands::profiles::profile_reorder,
            commands::profiles::group_reorder,
            commands::profiles::profile_export_file,
            commands::profiles::profile_import_file,
            commands::profiles::profile_backup_inspect,
            commands::sessions::session_connect,
            commands::sessions::session_connect_direct,
            commands::sessions::session_connect_local,
            commands::sessions::session_reconnect,
            commands::sessions::session_list,
            commands::sessions::session_info,
            commands::sessions::serial_ports,
            commands::sessions::wsl_list,
            commands::sessions::session_rtt,
            commands::sessions::session_write,
            commands::sessions::session_write_bytes,
            commands::sessions::session_snapshot,
            commands::sessions::session_cwd,
            commands::sessions::session_ring_b64,
            commands::monitor::monitor_sample,
            commands::process::process_list,
            commands::process::process_kill,
            commands::service::service_list,
            commands::service::service_control,
            commands::trigger::trigger_list,
            commands::trigger::trigger_save,
            commands::trigger::trigger_delete,
            commands::record::record_start,
            commands::record::record_stop,
            commands::record::record_status,
            commands::record::record_list,
            commands::record::record_read,
            commands::record::record_delete,
            commands::record::record_open_dir,
            commands::zmodem::zmodem_download,
            commands::zmodem::zmodem_upload,
            commands::zmodem::zmodem_probe,
            commands::zmodem::zmodem_terminal_start,
            commands::zmodem::zmodem_terminal_cancel,
            commands::zmodem::zmodem_pick,
            commands::dialog::dialog_pick_file,
            commands::dialog::dialog_pick_folder,
            commands::dialog::dialog_save_file,
            commands::plugin::plugin_list,
            commands::plugin::plugin_reload,
            commands::plugin::plugin_locale_get,
            commands::plugin::plugin_install,
            commands::plugin::plugin_pick_dir,
            commands::plugin::plugin_uninstall,
            commands::plugin::plugin_settings_get,
            commands::plugin::plugin_setting_set,
            commands::plugin::plugin_run_command,
            commands::plugin::plugin_on_output,
            commands::sessions::session_resize,
            commands::sessions::session_close,
            commands::ftps_cert::ftps_cert_respond,
            commands::ftps_cert::ftps_cert_list,
            commands::ftps_cert::ftps_cert_forget,
            commands::ssh::host_key_respond,
            commands::ssh::ssh_kbd_respond,
            commands::ssh::host_key_list,
            commands::ssh::host_key_forget,
            commands::history::history_list,
            commands::history::history_record,
            commands::history::history_delete,
            commands::history::history_clear,
            commands::lock::lock_status,
            commands::lock::lock_touch,
            commands::lock::lock_now,
            commands::lock::lock_unlock,
            commands::lock::lock_pin_set,
            commands::lock::lock_pin_clear,
            commands::lock::lock_secq_question,
            commands::lock::lock_pin_reset,
            commands::lock::lock_config_set,
            commands::file::file_cwd,
            commands::file::file_list,
            commands::file::file_mkdir,
            commands::file::file_remove,
            commands::file::file_rename,
            commands::file::file_chmod,
            commands::file::file_read,
            commands::file::file_write,
            commands::file::file_edit_external,
            commands::file::file_open_remote,
            commands::file::file_edit_preview,
            commands::file::file_edit_commit,
            commands::file::local_list,
            commands::file::local_read,
            commands::file::local_write,
            commands::file::local_save_text,
            commands::file::local_mkdir,
            commands::file::local_remove,
            commands::file::local_rename,
            commands::transfer::transfer_upload,
            commands::transfer::transfer_download,
            commands::transfer::transfer_upload_dir,
            commands::transfer::transfer_download_dir,
            commands::transfer::transfer_cancel,
            commands::transfer::transfer_retry,
            commands::transfer::transfer_list,
            commands::transfer::transfer_clear,
            commands::settings::transfer_bundle_get,
            commands::settings::transfer_bundle_set,
            commands::ai::ai_provider_list,
            commands::ai::ai_provider_get,
            commands::ai::ai_provider_save,
            commands::ai::ai_provider_delete,
            commands::ai::ai_provider_set_default,
            commands::ai::ai_provider_test,
            commands::ai::ai_provider_test_draft,
            commands::ai::ai_diagnose_connect,
            commands::ai::ai_conversation_list,
            commands::ai::ai_conversation_create,
            commands::ai::ai_conversation_rename,
            commands::ai::ai_conversation_delete,
            commands::ai::ai_message_list,
            commands::ai::ai_stage_image,
            commands::ai::ai_probe_attachment,
            commands::ai::ai_get_attachment,
            commands::ai::ai_usage_stats,
            commands::ai::ai_send_message,
            commands::ai::ai_respond_approval,
            commands::ai::ai_list_pending_approvals,
            commands::ai::ai_list_pending_questions,
            commands::ai::ai_respond_question,
            commands::ai::ai_cancel_turn,
            commands::ai::ai_prompt_get,
            commands::ai::ai_prompt_set,
            commands::ai::ai_prompt_reset,
            commands::ai::ai_prompt_preview,
            commands::settings::settings_get,
            commands::settings::settings_set,
            commands::settings::settings_reset,
            commands::settings::appearance_get,
            commands::settings::appearance_set,
            commands::settings::appearance_reset,
            commands::settings::theme_import_file,
            commands::settings::theme_scan_dir,
            commands::settings::terminal_get,
            commands::settings::terminal_set,
            commands::settings::terminal_reset,
            commands::audit::audit_list,
            commands::audit::audit_clear,
            commands::policy::policy_rule_list,
            commands::policy::policy_rule_save,
            commands::policy::policy_rule_delete,
            commands::memory::memory_list,
            commands::memory::memory_save,
            commands::memory::memory_delete,
            commands::memory::memory_clear,
            commands::snippet::snippet_list,
            commands::snippet::snippet_save,
            commands::snippet::snippet_delete,
            commands::snippet::snippet_render,
            commands::ssh_key::ssh_key_list,
            commands::ssh_key::ssh_key_import,
            commands::ssh_key::ssh_key_generate,
            commands::ssh_key::ssh_key_export_file,
            commands::ssh_key::ssh_key_delete,
            commands::ssh_key::ssh_keygen_generate,
            commands::ssh_key::ssh_keygen_save_private,
            commands::ssh_key::ssh_keygen_save_public,
            commands::system::system_info,
            commands::tunnel::tunnel_list,
            commands::tunnel::tunnel_save,
            commands::tunnel::tunnel_delete,
            commands::tunnel::tunnel_open,
            commands::tunnel::tunnel_close,
            commands::tunnel::tunnel_active,
            // 网络小工具（工具启动器专用，自包含于 commands/net.rs，见其模块头「可分离纪律」）
            commands::net::net_listening_ports,
            commands::net::net_port_scan,
            commands::net::net_scan_cancel,
            commands::net::net_ping,
            commands::net::net_dns_lookup,
            commands::net::net_traceroute,
            commands::net::net_public_ip,
            commands::net::net_open_local_network_settings
            ]);
            move |invoke: tauri::ipc::Invoke<tauri::Wry>| {
                watchdog::note_dispatch(invoke.message.command());
                ipc(invoke)
            }
        })
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|_app, _event| {});
}
