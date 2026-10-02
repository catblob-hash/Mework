//! The file pane's hand-offs to this computer's own desktop: showing a file or
//! a folder in the file manager, and opening a file in another program.
//!
//! Each is one of the pane's menu items, chosen by the reader; nothing here is
//! reachable from a path the model wrote (those only ask the pane to show a
//! file). The programs on offer are the system's own answer — Launch Services
//! on macOS, the shell's association handlers on Windows — and the one opened
//! must be one of them: the renderer names a program by the id this module
//! handed it, and the id is checked against a fresh answer before anything
//! starts, so no other program can be launched through it.

use std::path::{Path, PathBuf};

use git_core::text;
use serde::Serialize;

/// A program the system offers for a file.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OpenWithApp {
    /// What [`open_with`] is given back to name this program.
    pub id: String,
    pub name: String,
    /// A small picture of the program, as a `data:` URL, when the system has one.
    pub icon: Option<String>,
    /// Whether this is what opening the file without choosing would use.
    pub default: bool,
}

/// The programs on offer, and whether the system has a chooser of its own for
/// the rest.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OpenWithChoices {
    pub apps: Vec<OpenWithApp>,
    pub chooser: bool,
}

/// The most programs the menu lists.
const MAX_APPS: usize = 16;

/// `raw` as a path on this computer that exists, with `..` and links resolved.
fn existing(raw: &str) -> Result<PathBuf, String> {
    crate::reveal_path::resolve_reveal_path(raw, None)
}

/// Shows `raw` in the file manager: a directory opened as itself, a file
/// selected in its folder.
pub(crate) fn open_in_file_manager(raw: &str) -> Result<(), String> {
    let path = existing(raw)?;
    #[cfg(target_os = "macos")]
    if path.is_dir() {
        return macos::open_directory(&path);
    }
    crate::reveal_path::reveal(&path)
}

pub(crate) fn choices(raw: &str) -> Result<OpenWithChoices, String> {
    let path = existing(raw)?;
    platform_choices(&path)
}

/// Opens `raw` in the program `app` names, or in the default one.
pub(crate) fn open_with(raw: &str, app: Option<&str>) -> Result<(), String> {
    let path = existing(raw)?;
    match app {
        None => platform_open_default(&path),
        Some(app) => {
            let offered = platform_choices(&path)?;
            if !offered.apps.iter().any(|candidate| candidate.id == app) {
                return Err(text!(
                    "系统没有为这个文件提供这个程序",
                    "The system does not offer that program for this file"
                ));
            }
            platform_open_with(&path, app)
        }
    }
}

/// The system's own "Open with" chooser, where it has one.
pub(crate) fn choose(raw: &str) -> Result<(), String> {
    let path = existing(raw)?;
    platform_choose(&path)
}

/// Waits for a launcher on a thread of its own, so it is not left a zombie
/// and the caller does not wait for it.
#[cfg(not(windows))]
fn reap(mut child: std::process::Child) {
    let _ = std::thread::Builder::new()
        .name("open-with-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        });
}

#[cfg(target_os = "macos")]
mod macos {
    use std::path::Path;

    use base64::Engine as _;
    use git_core::text;
    use objc2::rc::Retained;
    use objc2::{sel, AllocAnyThread};
    use objc2_app_kit::{
        NSBitmapImageFileType, NSBitmapImageRep, NSCompositingOperation, NSDeviceRGBColorSpace,
        NSGraphicsContext, NSImage, NSWorkspace,
    };
    use objc2_foundation::{
        NSDictionary, NSFileManager, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSURL,
    };

    use super::{OpenWithApp, MAX_APPS};

    /// Icons are drawn at twice the menu's 16 points, for a Retina screen.
    const ICON_PIXELS: isize = 32;

    pub(super) fn open_directory(path: &Path) -> Result<(), String> {
        let path = path.to_string_lossy();
        // A viewer rooted at the folder shows what is in it; `open` would start
        // a bundle (an `.app` is a folder) instead of showing it.
        if NSWorkspace::sharedWorkspace()
            .selectFile_inFileViewerRootedAtPath(None, &NSString::from_str(&path))
        {
            Ok(())
        } else {
            Err(text!("访达没有打开 {path}", "Finder did not open {path}"))
        }
    }

    pub(super) fn apps(path: &Path) -> Vec<OpenWithApp> {
        let workspace = NSWorkspace::sharedWorkspace();
        let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        let default = workspace
            .URLForApplicationToOpenURL(&url)
            .and_then(|app| app.path())
            .map(|path| path.to_string());
        // `URLsForApplicationsToOpenURL:` is macOS 12; before it only the
        // default is known.
        let mut paths: Vec<String> = Vec::new();
        if workspace.respondsToSelector(sel!(URLsForApplicationsToOpenURL:)) {
            for app in workspace.URLsForApplicationsToOpenURL(&url).iter() {
                if let Some(path) = app.path() {
                    paths.push(path.to_string());
                }
            }
        }
        if let Some(default) = &default {
            paths.retain(|path| path != default);
            paths.insert(0, default.clone());
        }
        let mut seen = std::collections::HashSet::new();
        paths.retain(|path| seen.insert(path.clone()));
        paths.truncate(MAX_APPS);
        let manager = NSFileManager::defaultManager();
        paths
            .into_iter()
            .map(|app| {
                let name = manager
                    .displayNameAtPath(&NSString::from_str(&app))
                    .to_string();
                OpenWithApp {
                    name: name.strip_suffix(".app").unwrap_or(&name).to_owned(),
                    icon: icon(&workspace.iconForFile(&NSString::from_str(&app))),
                    default: default.as_deref() == Some(app.as_str()),
                    id: app,
                }
            })
            .collect()
    }

    /// `image` drawn into a small bitmap and encoded as a PNG `data:` URL.
    fn icon(image: &NSImage) -> Option<String> {
        // SAFETY: a null planes pointer asks the representation to allocate its
        // own buffer; the colour space name is AppKit's own constant.
        let bitmap: Retained<NSBitmapImageRep> = unsafe {
            NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
                NSBitmapImageRep::alloc(),
                std::ptr::null_mut(),
                ICON_PIXELS,
                ICON_PIXELS,
                8,
                4,
                true,
                false,
                NSDeviceRGBColorSpace,
                0,
                0,
            )
        }?;
        let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&bitmap)?;
        // The current context is per thread; saving and restoring around the
        // draw leaves whatever this thread had untouched.
        NSGraphicsContext::saveGraphicsState_class();
        NSGraphicsContext::setCurrentContext(Some(&context));
        let side = ICON_PIXELS as f64;
        image.drawInRect_fromRect_operation_fraction(
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(side, side)),
            NSRect::ZERO,
            NSCompositingOperation::SourceOver,
            1.0,
        );
        NSGraphicsContext::restoreGraphicsState_class();
        // SAFETY: an empty dictionary is a valid set of encoding properties.
        let data = unsafe {
            bitmap.representationUsingType_properties(
                NSBitmapImageFileType::PNG,
                &NSDictionary::new(),
            )
        }?;
        Some(format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(data.to_vec())
        ))
    }
}

#[cfg(target_os = "macos")]
fn platform_choices(path: &Path) -> Result<OpenWithChoices, String> {
    Ok(OpenWithChoices {
        apps: macos::apps(path),
        chooser: false,
    })
}

#[cfg(target_os = "macos")]
fn platform_open_default(path: &Path) -> Result<(), String> {
    let child = std::process::Command::new("open")
        .arg("--")
        .arg(path)
        .spawn()
        .map_err(|error| text!("无法打开文件：{error}", "Could not open the file: {error}"))?;
    reap(child);
    Ok(())
}

#[cfg(target_os = "macos")]
fn platform_open_with(path: &Path, app: &str) -> Result<(), String> {
    let child = std::process::Command::new("open")
        .arg("-a")
        .arg(app)
        .arg("--")
        .arg(path)
        .spawn()
        .map_err(|error| text!("无法打开文件：{error}", "Could not open the file: {error}"))?;
    reap(child);
    Ok(())
}

#[cfg(target_os = "macos")]
fn platform_choose(_path: &Path) -> Result<(), String> {
    Err(text!(
        "这个系统没有“打开方式”选择器",
        "This system has no Open With chooser"
    ))
}

#[cfg(windows)]
mod windows {
    //! The shell's association handlers, through the two COM interfaces the
    //! listing needs. `windows-sys` carries no interface definitions, so the
    //! vtables are spelled here, in the order the SDK headers declare them.

    use std::ffi::c_void;
    use std::path::Path;

    use git_core::text;
    use windows_sys::core::{GUID, HRESULT, PCWSTR, PWSTR};
    use windows_sys::Win32::System::Com::{
        CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_APARTMENTTHREADED,
        COINIT_DISABLE_OLE1DDE,
    };
    use windows_sys::Win32::UI::Shell::{
        BHID_DataObject, SHAssocEnumHandlers, SHCreateItemFromParsingName, SHOpenWithDialog,
        ASSOC_FILTER_RECOMMENDED, OAIF_EXEC, OPENASINFO,
    };

    use super::{OpenWithApp, MAX_APPS};

    const IID_ISHELLITEM: GUID = GUID::from_u128(0x43826d1e_e718_42ee_bc55_a1e261c37bfe);
    const IID_IDATAOBJECT: GUID = GUID::from_u128(0x0000010e_0000_0000_c000_000000000046);

    #[repr(C)]
    struct UnknownVtbl {
        query_interface:
            unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
        add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
    }

    #[repr(C)]
    struct EnumAssocHandlersVtbl {
        base: UnknownVtbl,
        next: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void, *mut u32) -> HRESULT,
    }

    #[repr(C)]
    struct AssocHandlerVtbl {
        base: UnknownVtbl,
        get_name: unsafe extern "system" fn(*mut c_void, *mut PWSTR) -> HRESULT,
        get_ui_name: unsafe extern "system" fn(*mut c_void, *mut PWSTR) -> HRESULT,
        get_icon_location: unsafe extern "system" fn(*mut c_void, *mut PWSTR, *mut i32) -> HRESULT,
        is_recommended: unsafe extern "system" fn(*mut c_void) -> HRESULT,
        make_default: unsafe extern "system" fn(*mut c_void, PCWSTR) -> HRESULT,
        invoke: unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT,
    }

    #[repr(C)]
    struct ShellItemVtbl {
        base: UnknownVtbl,
        bind_to_handler: unsafe extern "system" fn(
            *mut c_void,
            *mut c_void,
            *const GUID,
            *const GUID,
            *mut *mut c_void,
        ) -> HRESULT,
    }

    /// An interface pointer, released when dropped.
    struct Com(*mut c_void);

    impl Com {
        /// SAFETY: the object's vtable must start with `V`'s layout.
        unsafe fn vtable<V>(&self) -> &V {
            &**(self.0 as *mut *const V)
        }
    }

    impl Drop for Com {
        fn drop(&mut self) {
            // SAFETY: every `Com` holds one reference it owns.
            unsafe { (self.vtable::<UnknownVtbl>().release)(self.0) };
        }
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// A string the shell allocated, copied out and freed.
    unsafe fn take(text: PWSTR) -> Option<String> {
        if text.is_null() {
            return None;
        }
        let length = (0..).take_while(|&index| *text.add(index) != 0).count();
        let value = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
        CoTaskMemFree(text as *const c_void);
        Some(value)
    }

    /// Runs `work` on a thread with a single-threaded apartment of its own, so
    /// apartment state never leaks into a reused worker thread.
    fn in_apartment<T: Send + 'static>(
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, String> {
        std::thread::Builder::new()
            .name("mework-open-with".into())
            .spawn(move || {
                // SAFETY: paired with `CoUninitialize` only when it succeeded.
                let com = unsafe {
                    CoInitializeEx(
                        std::ptr::null(),
                        (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
                    )
                };
                let value = work();
                if com >= 0 {
                    unsafe { CoUninitialize() };
                }
                value
            })
            .map_err(|error| format!("{error}"))?
            .join()
            .map_err(|_| text!("打开方式的线程异常退出", "The Open With thread failed"))
    }

    /// Each recommended handler for `extension`, with its id and name.
    unsafe fn handlers(extension: &str) -> Vec<(Com, String, String)> {
        let extension = wide(extension);
        let mut raw = std::ptr::null_mut();
        if SHAssocEnumHandlers(extension.as_ptr(), ASSOC_FILTER_RECOMMENDED, &mut raw) < 0
            || raw.is_null()
        {
            return Vec::new();
        }
        let list = Com(raw);
        let mut found = Vec::new();
        while found.len() < MAX_APPS {
            let mut handler = std::ptr::null_mut();
            let mut fetched = 0u32;
            let status = (list.vtable::<EnumAssocHandlersVtbl>().next)(
                list.0,
                1,
                &mut handler,
                &mut fetched,
            );
            if status != 0 || fetched == 0 || handler.is_null() {
                break;
            }
            let handler = Com(handler);
            let vtable = handler.vtable::<AssocHandlerVtbl>();
            let mut name = std::ptr::null_mut();
            let mut label = std::ptr::null_mut();
            let id = if (vtable.get_name)(handler.0, &mut name) >= 0 {
                take(name)
            } else {
                None
            };
            let ui = if (vtable.get_ui_name)(handler.0, &mut label) >= 0 {
                take(label)
            } else {
                None
            };
            if let (Some(id), Some(ui)) = (id, ui) {
                found.push((handler, id, ui));
            }
        }
        found
    }

    fn extension_of(path: &Path) -> Option<String> {
        path.extension()
            .map(|extension| format!(".{}", extension.to_string_lossy()))
    }

    pub(super) fn apps(path: &Path) -> Result<Vec<OpenWithApp>, String> {
        let Some(extension) = extension_of(path) else {
            return Ok(Vec::new());
        };
        if path.is_dir() {
            return Ok(Vec::new());
        }
        in_apartment(move || {
            // The shell lists the default handler first, the order Explorer's own
            // Open With menu shows; `AssocQueryString` names the classic
            // association instead, which a Store app chosen as the default is not.
            // SAFETY: the handlers are used and released on this thread.
            unsafe { handlers(&extension) }
                .into_iter()
                .enumerate()
                .map(|(index, (_, id, name))| OpenWithApp {
                    id,
                    name,
                    icon: None,
                    default: index == 0,
                })
                .collect()
        })
    }

    pub(super) fn open_with(path: &Path, app: &str) -> Result<(), String> {
        let extension = extension_of(path)
            .ok_or_else(|| text!("这个文件没有扩展名", "This file has no extension"))?;
        let target = wide(&path.to_string_lossy());
        let app = app.to_owned();
        in_apartment(move || unsafe {
            let Some((handler, _, _)) = handlers(&extension)
                .into_iter()
                .find(|(_, id, _)| *id == app)
            else {
                return Err(text!(
                    "系统没有为这个文件提供这个程序",
                    "The system does not offer that program for this file"
                ));
            };
            let mut item = std::ptr::null_mut();
            if SHCreateItemFromParsingName(
                target.as_ptr(),
                std::ptr::null_mut(),
                &IID_ISHELLITEM,
                &mut item,
            ) < 0
                || item.is_null()
            {
                return Err(text!("无法打开这个文件", "Could not open this file"));
            }
            let item = Com(item);
            let mut data = std::ptr::null_mut();
            let bound = (item.vtable::<ShellItemVtbl>().bind_to_handler)(
                item.0,
                std::ptr::null_mut(),
                &BHID_DataObject,
                &IID_IDATAOBJECT,
                &mut data,
            );
            if bound < 0 || data.is_null() {
                return Err(text!("无法打开这个文件", "Could not open this file"));
            }
            let data = Com(data);
            let status = (handler.vtable::<AssocHandlerVtbl>().invoke)(handler.0, data.0);
            if status < 0 {
                return Err(text!(
                    "程序没有打开这个文件（{status:#x}）",
                    "The program did not open this file ({status:#x})"
                ));
            }
            Ok(())
        })?
    }

    pub(super) fn choose(path: &Path) -> Result<(), String> {
        let target = wide(&path.to_string_lossy());
        in_apartment(move || {
            let info = OPENASINFO {
                pcszFile: target.as_ptr(),
                pcszClass: std::ptr::null(),
                oaifInFlags: OAIF_EXEC,
            };
            // SAFETY: `target` outlives the modal call; no owner window is passed.
            let status = unsafe { SHOpenWithDialog(std::ptr::null_mut(), &info) };
            // Cancelling the dialog is the reader's answer, not a failure.
            if status < 0 && status != 0x800704C7u32 as i32 {
                return Err(text!(
                    "无法显示“打开方式”（{status:#x}）",
                    "Could not show Open With ({status:#x})"
                ));
            }
            Ok(())
        })?
    }
}

#[cfg(windows)]
fn platform_choices(path: &Path) -> Result<OpenWithChoices, String> {
    Ok(OpenWithChoices {
        apps: windows::apps(path)?,
        chooser: !path.is_dir(),
    })
}

#[cfg(windows)]
fn platform_open_default(path: &Path) -> Result<(), String> {
    // A folder is shown, a file opened in its default program: what a double
    // click in Explorer does.
    if path.is_dir() {
        return crate::reveal_path::reveal(path);
    }
    let offered = windows::apps(path)?;
    match offered.first() {
        Some(app) => windows::open_with(path, &app.id),
        None => windows::choose(path),
    }
}

#[cfg(windows)]
fn platform_open_with(path: &Path, app: &str) -> Result<(), String> {
    windows::open_with(path, app)
}

#[cfg(windows)]
fn platform_choose(path: &Path) -> Result<(), String> {
    windows::choose(path)
}

#[cfg(not(any(target_os = "macos", windows)))]
fn platform_choices(_path: &Path) -> Result<OpenWithChoices, String> {
    // The desktop's default is the one program every Linux desktop can name.
    Ok(OpenWithChoices {
        apps: Vec::new(),
        chooser: false,
    })
}

#[cfg(not(any(target_os = "macos", windows)))]
fn platform_open_default(path: &Path) -> Result<(), String> {
    let child = std::process::Command::new("xdg-open")
        .arg(path)
        .spawn()
        .map_err(|error| text!("无法打开文件：{error}", "Could not open the file: {error}"))?;
    reap(child);
    Ok(())
}

#[cfg(not(any(target_os = "macos", windows)))]
fn platform_open_with(_path: &Path, _app: &str) -> Result<(), String> {
    Err(text!(
        "这个系统没有可选的程序",
        "This system offers no programs to choose from"
    ))
}

#[cfg(not(any(target_os = "macos", windows)))]
fn platform_choose(_path: &Path) -> Result<(), String> {
    Err(text!(
        "这个系统没有“打开方式”选择器",
        "This system has no Open With chooser"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_the_system_did_not_offer_is_refused() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let file = directory.path().join("note.txt");
        std::fs::write(&file, "x").expect("write");
        let error = open_with(&file.to_string_lossy(), Some("/bin/sh")).expect_err("refused");
        assert!(
            error.contains("does not offer") || error.contains("没有为这个文件提供"),
            "{error}"
        );
    }

    #[test]
    fn a_missing_path_is_refused_before_the_desktop_sees_it() {
        assert!(open_in_file_manager("/mework-does-not-exist/x").is_err());
        assert!(choices("relative/path").is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn launch_services_offers_programs_for_a_text_file() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let file = directory.path().join("note.txt");
        std::fs::write(&file, "x").expect("write");
        let offered = choices(&file.to_string_lossy()).expect("choices");
        assert!(!offered.apps.is_empty(), "TextEdit at least opens a .txt");
        assert!(offered.apps[0].default);
        assert!(offered.apps.iter().all(|app| app
            .icon
            .as_deref()
            .is_some_and(|icon| icon.starts_with("data:image/png;base64,"))));
    }
}
