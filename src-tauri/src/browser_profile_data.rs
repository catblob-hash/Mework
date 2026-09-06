use std::time::Duration;

use crate::chromium_capability::WebView2Permit;

/// A WebView2 profile data category accepted by `ClearBrowsingData`.
///
/// Group categories are intentionally explicit: `AllDomStorage` and
/// `AllSiteData` are WebView2's forward-compatible groups, not aliases expanded
/// by Mework. Clearing an entire profile uses [`clear_all_browsing_data`] so
/// future WebView2 data categories are included automatically.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum BrowserProfileDataKind {
    FileSystems,
    IndexedDb,
    LocalStorage,
    WebSql,
    CacheStorage,
    AllDomStorage,
    Cookies,
    AllSiteData,
    DiskCache,
    DownloadHistory,
    GeneralAutofill,
    PasswordAutosave,
    BrowsingHistory,
    Settings,
    ServiceWorkers,
}

impl BrowserProfileDataKind {
    pub(crate) const ALL: [Self; 15] = [
        Self::FileSystems,
        Self::IndexedDb,
        Self::LocalStorage,
        Self::WebSql,
        Self::CacheStorage,
        Self::AllDomStorage,
        Self::Cookies,
        Self::AllSiteData,
        Self::DiskCache,
        Self::DownloadHistory,
        Self::GeneralAutofill,
        Self::PasswordAutosave,
        Self::BrowsingHistory,
        Self::Settings,
        Self::ServiceWorkers,
    ];

    const fn mask(self) -> i32 {
        match self {
            Self::FileSystems => 0x0001,
            Self::IndexedDb => 0x0002,
            Self::LocalStorage => 0x0004,
            Self::WebSql => 0x0008,
            Self::CacheStorage => 0x0010,
            Self::AllDomStorage => 0x0020,
            Self::Cookies => 0x0040,
            Self::AllSiteData => 0x0080,
            Self::DiskCache => 0x0100,
            Self::DownloadHistory => 0x0200,
            Self::GeneralAutofill => 0x0400,
            Self::PasswordAutosave => 0x0800,
            Self::BrowsingHistory => 0x1000,
            Self::Settings => 0x2000,
            Self::ServiceWorkers => 0x8000,
        }
    }
}

fn selected_data_mask(kinds: &[BrowserProfileDataKind]) -> Result<i32, String> {
    let mask = kinds.iter().fold(0_i32, |mask, kind| mask | kind.mask());
    if mask == 0 {
        Err("至少选择一种要清除的浏览数据".to_owned())
    } else {
        Ok(mask)
    }
}

fn validate_timeout(timeout: Duration) -> Result<(), String> {
    if timeout.is_zero() {
        Err("浏览数据清理超时必须大于 0 毫秒".to_owned())
    } else {
        Ok(())
    }
}

/// Clears selected categories and returns only after WebView2 invokes its
/// completion handler.
///
/// A timeout does not cancel WebView2's operation; it only stops waiting for
/// the completion callback. Returned errors contain an operation stage and an
/// HRESULT, never a URL, profile path, cookie, or credential value.
pub(crate) fn clear_browsing_data(
    page: &tauri::Webview,
    tail_permit: WebView2Permit,
    kinds: &[BrowserProfileDataKind],
    timeout: Duration,
) -> Result<(), String> {
    let mask = selected_data_mask(kinds)?;
    clear_profile_data(page, tail_permit, ClearOperation::Selected(mask), timeout)
}

/// Clears every browsing-data category associated with this WebView2 profile
/// and returns only after WebView2 invokes its completion handler.
pub(crate) fn clear_all_browsing_data(
    page: &tauri::Webview,
    tail_permit: WebView2Permit,
    timeout: Duration,
) -> Result<(), String> {
    clear_profile_data(page, tail_permit, ClearOperation::All, timeout)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClearOperation {
    Selected(i32),
    All,
}

#[cfg(windows)]
fn clear_profile_data(
    page: &tauri::Webview,
    tail_permit: WebView2Permit,
    operation: ClearOperation,
    timeout: Duration,
) -> Result<(), String> {
    use std::sync::mpsc;

    use webview2_com::{
        ClearBrowsingDataCompletedHandler,
        Microsoft::Web::WebView2::Win32::{
            ICoreWebView2Profile2, ICoreWebView2_13, COREWEBVIEW2_BROWSING_DATA_KINDS,
        },
    };

    validate_timeout(timeout)?;

    let (sender, receiver) = mpsc::sync_channel::<Result<(), String>>(1);
    let scheduling_sender = sender.clone();
    let callback_token = tail_permit.callback_token();
    page.with_webview(move |platform| {
        // This blocking permit covers only Tauri's queue and native registration. WebView2 is
        // allowed to never invoke a completion handler, so the dormant callback retains only a
        // revocable generation token.
        let _dispatch_permit = tail_permit;
        let completion_sender = sender.clone();
        let scheduled = (|| -> Result<(), String> {
            let controller = platform.controller();
            let core = unsafe { controller.CoreWebView2() }
                .map_err(|error| hresult_error("取得 WebView2 核心", error.code().0))?;
            let core_13 =
                unsafe { query_interface::<_, ICoreWebView2_13>(&core, &IID_CORE_WEBVIEW2_13) }
                    .map_err(|code| hresult_error("取得 WebView2 Profile 接口", code))?;
            let profile = unsafe { core_13.Profile() }
                .map_err(|error| hresult_error("取得 WebView2 Profile", error.code().0))?;
            let profile_2 = unsafe {
                query_interface::<_, ICoreWebView2Profile2>(&profile, &IID_CORE_WEBVIEW2_PROFILE2)
            }
            .map_err(|code| hresult_error("取得 WebView2 浏览数据接口", code))?;

            let callback = ClearBrowsingDataCompletedHandler::create(Box::new(move |status| {
                let Ok(_callback_permit) = callback_token.permit() else {
                    return Ok(());
                };
                let result = status
                    .map_err(|error| hresult_error("WebView2 浏览数据清理未完成", error.code().0));
                let _ = completion_sender.try_send(result);
                Ok(())
            }));
            unsafe {
                match operation {
                    ClearOperation::Selected(mask) => profile_2
                        .ClearBrowsingData(COREWEBVIEW2_BROWSING_DATA_KINDS(mask), &callback),
                    ClearOperation::All => profile_2.ClearBrowsingDataAll(&callback),
                }
            }
            .map_err(|error| hresult_error("启动 WebView2 浏览数据清理", error.code().0))
        })();

        if let Err(error) = scheduled {
            let _ = scheduling_sender.try_send(Err(error));
        }
    })
    .map_err(|_| "无法调度 WebView2 浏览数据清理".to_owned())?;

    receiver
        .recv_timeout(timeout)
        .map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => format!(
                "等待 WebView2 完成浏览数据清理超时（{} 毫秒；后台操作可能仍在继续）",
                timeout.as_millis()
            ),
            mpsc::RecvTimeoutError::Disconnected => {
                "WebView2 浏览数据清理结果通道已关闭".to_owned()
            }
        })?
}

#[cfg(not(windows))]
fn clear_profile_data(
    _page: &tauri::Webview,
    _tail_permit: WebView2Permit,
    _operation: ClearOperation,
    timeout: Duration,
) -> Result<(), String> {
    validate_timeout(timeout)?;
    Err("当前平台不支持 WebView2 浏览数据清理".to_owned())
}

#[cfg(windows)]
fn hresult_error(stage: &str, code: i32) -> String {
    format!("{stage}失败（HRESULT 0x{:08X}）", code as u32)
}

// `webview2-com` exposes the generated WebView2 interfaces but does not
// re-export its `windows-core::Interface` trait. Mework deliberately avoids a
// second direct windows-core dependency, so this small adapter performs the
// same ABI-defined QueryInterface operation. The IIDs below are from the
// webview2-com 0.38.2 generated bindings used by this crate.
#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RawGuid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

#[cfg(windows)]
impl RawGuid {
    const fn from_u128(value: u128) -> Self {
        Self {
            data1: (value >> 96) as u32,
            data2: (value >> 80) as u16,
            data3: (value >> 64) as u16,
            data4: (value as u64).to_be_bytes(),
        }
    }
}

#[cfg(windows)]
const IID_CORE_WEBVIEW2_13: RawGuid = RawGuid::from_u128(0xf75f09a8_667e_4983_88d6_c8773f315e84);
#[cfg(windows)]
const IID_CORE_WEBVIEW2_PROFILE2: RawGuid =
    RawGuid::from_u128(0xfa740d4b_5eae_4344_a8ad_74be31925397);

#[cfg(windows)]
#[repr(C)]
struct RawIUnknownVtable {
    query_interface: unsafe extern "system" fn(
        this: *mut std::ffi::c_void,
        iid: *const RawGuid,
        interface: *mut *mut std::ffi::c_void,
    ) -> i32,
    add_ref: unsafe extern "system" fn(this: *mut std::ffi::c_void) -> u32,
    release: unsafe extern "system" fn(this: *mut std::ffi::c_void) -> u32,
}

#[cfg(windows)]
unsafe fn query_interface<Source, Target>(source: &Source, iid: &RawGuid) -> Result<Target, i32> {
    use std::{ffi::c_void, mem};

    // Generated windows-core COM interfaces are repr(transparent) one-pointer
    // owners. Keep `source` alive while querying and transfer QueryInterface's
    // returned AddRef directly into the generated target owner.
    assert_eq!(mem::size_of::<Source>(), mem::size_of::<*mut c_void>());
    assert_eq!(mem::size_of::<Target>(), mem::size_of::<*mut c_void>());
    let source_pointer = unsafe { mem::transmute_copy::<Source, *mut c_void>(source) };
    if source_pointer.is_null() {
        return Err(0x8000_4003_u32 as i32); // E_POINTER
    }
    let vtable = unsafe { *(source_pointer as *const *const RawIUnknownVtable) };
    if vtable.is_null() {
        return Err(0x8000_4003_u32 as i32);
    }

    let mut target_pointer = std::ptr::null_mut();
    let status = unsafe { ((*vtable).query_interface)(source_pointer, iid, &mut target_pointer) };
    if status < 0 {
        return Err(status);
    }
    if target_pointer.is_null() {
        return Err(0x8000_4003_u32 as i32);
    }
    Ok(unsafe { mem::transmute_copy::<*mut c_void, Target>(&target_pointer) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_supported_kind_has_the_documented_webview2_flag() {
        let expected = [
            0x0001, 0x0002, 0x0004, 0x0008, 0x0010, 0x0020, 0x0040, 0x0080, 0x0100, 0x0200, 0x0400,
            0x0800, 0x1000, 0x2000, 0x8000,
        ];
        assert_eq!(
            BrowserProfileDataKind::ALL.map(BrowserProfileDataKind::mask),
            expected
        );
    }

    #[test]
    fn selected_kinds_are_combined_and_duplicates_are_idempotent() {
        assert_eq!(
            selected_data_mask(&[
                BrowserProfileDataKind::Cookies,
                BrowserProfileDataKind::DiskCache,
                BrowserProfileDataKind::Cookies,
                BrowserProfileDataKind::ServiceWorkers,
            ]),
            Ok(0x8140)
        );
    }

    #[test]
    fn an_empty_selected_clear_is_rejected() {
        assert_eq!(
            selected_data_mask(&[]),
            Err("至少选择一种要清除的浏览数据".to_owned())
        );
    }

    #[test]
    fn timeout_must_be_positive() {
        assert!(validate_timeout(Duration::from_millis(1)).is_ok());
        assert_eq!(
            validate_timeout(Duration::ZERO),
            Err("浏览数据清理超时必须大于 0 毫秒".to_owned())
        );
    }

    #[cfg(windows)]
    #[test]
    fn generated_binding_flags_match_the_local_platform_neutral_mapping() {
        use webview2_com::Microsoft::Web::WebView2::Win32::{
            COREWEBVIEW2_BROWSING_DATA_KINDS_ALL_DOM_STORAGE,
            COREWEBVIEW2_BROWSING_DATA_KINDS_ALL_SITE,
            COREWEBVIEW2_BROWSING_DATA_KINDS_BROWSING_HISTORY,
            COREWEBVIEW2_BROWSING_DATA_KINDS_CACHE_STORAGE,
            COREWEBVIEW2_BROWSING_DATA_KINDS_COOKIES, COREWEBVIEW2_BROWSING_DATA_KINDS_DISK_CACHE,
            COREWEBVIEW2_BROWSING_DATA_KINDS_DOWNLOAD_HISTORY,
            COREWEBVIEW2_BROWSING_DATA_KINDS_FILE_SYSTEMS,
            COREWEBVIEW2_BROWSING_DATA_KINDS_GENERAL_AUTOFILL,
            COREWEBVIEW2_BROWSING_DATA_KINDS_INDEXED_DB,
            COREWEBVIEW2_BROWSING_DATA_KINDS_LOCAL_STORAGE,
            COREWEBVIEW2_BROWSING_DATA_KINDS_PASSWORD_AUTOSAVE,
            COREWEBVIEW2_BROWSING_DATA_KINDS_SERVICE_WORKERS,
            COREWEBVIEW2_BROWSING_DATA_KINDS_SETTINGS, COREWEBVIEW2_BROWSING_DATA_KINDS_WEB_SQL,
        };

        let generated = [
            COREWEBVIEW2_BROWSING_DATA_KINDS_FILE_SYSTEMS.0,
            COREWEBVIEW2_BROWSING_DATA_KINDS_INDEXED_DB.0,
            COREWEBVIEW2_BROWSING_DATA_KINDS_LOCAL_STORAGE.0,
            COREWEBVIEW2_BROWSING_DATA_KINDS_WEB_SQL.0,
            COREWEBVIEW2_BROWSING_DATA_KINDS_CACHE_STORAGE.0,
            COREWEBVIEW2_BROWSING_DATA_KINDS_ALL_DOM_STORAGE.0,
            COREWEBVIEW2_BROWSING_DATA_KINDS_COOKIES.0,
            COREWEBVIEW2_BROWSING_DATA_KINDS_ALL_SITE.0,
            COREWEBVIEW2_BROWSING_DATA_KINDS_DISK_CACHE.0,
            COREWEBVIEW2_BROWSING_DATA_KINDS_DOWNLOAD_HISTORY.0,
            COREWEBVIEW2_BROWSING_DATA_KINDS_GENERAL_AUTOFILL.0,
            COREWEBVIEW2_BROWSING_DATA_KINDS_PASSWORD_AUTOSAVE.0,
            COREWEBVIEW2_BROWSING_DATA_KINDS_BROWSING_HISTORY.0,
            COREWEBVIEW2_BROWSING_DATA_KINDS_SETTINGS.0,
            COREWEBVIEW2_BROWSING_DATA_KINDS_SERVICE_WORKERS.0,
        ];
        assert_eq!(
            BrowserProfileDataKind::ALL.map(BrowserProfileDataKind::mask),
            generated
        );
    }

    #[cfg(windows)]
    #[test]
    fn queried_interface_iids_match_the_generated_webview2_0_38_bindings() {
        assert_eq!(
            IID_CORE_WEBVIEW2_13,
            RawGuid {
                data1: 0xf75f09a8,
                data2: 0x667e,
                data3: 0x4983,
                data4: [0x88, 0xd6, 0xc8, 0x77, 0x3f, 0x31, 0x5e, 0x84],
            }
        );
        assert_eq!(
            IID_CORE_WEBVIEW2_PROFILE2,
            RawGuid {
                data1: 0xfa740d4b,
                data2: 0x5eae,
                data3: 0x4344,
                data4: [0xa8, 0xad, 0x74, 0xbe, 0x31, 0x92, 0x53, 0x97],
            }
        );
    }

    #[cfg(windows)]
    #[test]
    fn hresult_errors_are_bounded_and_contain_no_dynamic_browser_data() {
        assert_eq!(
            hresult_error("启动 WebView2 浏览数据清理", 0x8000_4005_u32 as i32),
            "启动 WebView2 浏览数据清理失败（HRESULT 0x80004005）"
        );
    }
}
