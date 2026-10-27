mod connect;

/// The one privileged native command: hand Bakery's own window the
/// operator credential for the local node, after proving the node is up
/// and the credential works.
///
/// Scoping, stated precisely: Tauri v2's capability/permission ACL covers
/// core and plugin commands, **not** application-defined ones — an app
/// command is reachable from any webview in the process. So the window
/// check below is the actual enforcement, not decoration: any webview
/// whose label is not [`connect::TRUSTED_WINDOW`] (a preview pane, a
/// second window, anything loaded later) is refused. The app also loads
/// only its own bundled frontend, so there is no remote page in the
/// process to begin with.
///
/// Agents never run in a webview at all — they are node-side processes
/// holding their own MCP-scoped credential, and the operator token is
/// never placed in their environment (`bakery-node`'s
/// `operator_token_never_appears_in_any_child_spawn_env`).
#[tauri::command]
async fn bakery_connect(window: tauri::Window) -> Result<connect::Connected, connect::ConnectError> {
    let label = window.label().to_string();
    if !connect::window_is_trusted(&label) {
        return Err(connect::ConnectError::new(
            connect::ErrorKind::Untrusted,
            format!("Window {label:?} may not request operator access."),
        ));
    }
    let options = connect::Options::desktop()?;
    // Blocking probes (TCP + a file read) belong off the UI thread.
    tauri::async_runtime::spawn_blocking(move || connect::connect(&options))
        .await
        .map_err(|e| {
            connect::ConnectError::new(
                connect::ErrorKind::Misconfigured,
                format!("The connection attempt could not run ({e})."),
            )
        })?
}

/// The Dock and app-switcher icon. A bundled `.app` takes it from its bundle,
/// and Tauri sets it itself only in dev builds; the deployed app is a bare
/// release binary with neither, so macOS showed its generic "exec" icon.
#[cfg(target_os = "macos")]
fn set_dock_icon() {
    use objc2::{AllocAnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    let Some(mtm) = MainThreadMarker::new() else { return };
    let data = NSData::with_bytes(include_bytes!("../icons/icon.png"));
    let Some(icon) = NSImage::initWithData(NSImage::alloc(), &data) else { return };
    unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&icon)) };
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|_app| {
            #[cfg(target_os = "macos")]
            set_dock_icon();
            Ok(())
        })
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![bakery_connect])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
