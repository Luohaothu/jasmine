use super::types::{CallSupportInfo, WebRTCPlatformInfo};

#[tauri::command]
pub fn check_call_support() -> Result<CallSupportInfo, String> {
    Ok(check_call_support_impl())
}

pub(crate) fn check_call_support_impl() -> CallSupportInfo {
    let platform = std::env::consts::OS.to_string();

    if cfg!(target_os = "linux") {
        return CallSupportInfo {
            supported: false,
            platform,
            reason: Some("Linux calling support is not enabled yet".to_string()),
        };
    }

    CallSupportInfo {
        supported: true,
        platform,
        reason: None,
    }
}

#[tauri::command]
pub fn get_webrtc_platform_info() -> Result<WebRTCPlatformInfo, String> {
    Ok(get_webrtc_platform_info_impl())
}

pub(crate) fn get_webrtc_platform_info_impl() -> WebRTCPlatformInfo {
    WebRTCPlatformInfo {
        platform: std::env::consts::OS.to_string(),
        webview: current_webview_backend().to_string(),
        webview_version: None,
    }
}

pub(crate) fn current_webview_backend() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        return "webview2";
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        return "wkwebview";
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    ))]
    {
        "webkitgtk"
    }

    #[cfg(target_os = "android")]
    {
        return "android-webview";
    }

    #[cfg(not(any(
        target_os = "windows",
        target_os = "macos",
        target_os = "ios",
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "android"
    )))]
    {
        "unknown"
    }
}
