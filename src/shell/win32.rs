use std::{
    cell::{Cell, UnsafeCell},
    os::windows::ffi::OsStringExt,
    path::PathBuf,
    pin::Pin,
    u32,
};

use bedrock::{self as br, SurfaceCreateInfo};
use windows::{
    Storage::Pickers::{FileOpenPicker, FileSavePicker},
    Win32::{
        Foundation::{
            E_NOTIMPL, GetLastError, HGLOBAL, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM,
        },
        Graphics::{
            Dwm::{
                DWMWA_USE_IMMERSIVE_DARK_MODE, DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
            },
            Gdi::{
                DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW, GetMonitorInfoW, HBRUSH,
                MONITOR_DEFAULTTOPRIMARY, MONITORINFOEXW, MapWindowPoints, MonitorFromWindow,
            },
        },
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, CoCreateInstance, DVASPECT_CONTENT, FORMATETC, STGMEDIUM,
                TYMED_HGLOBAL,
            },
            LibraryLoader::GetModuleHandleW,
            Memory::{GlobalLock, GlobalUnlock},
            Ole::{
                CF_HDROP, DROPEFFECT_LINK, IDropTarget, IDropTarget_Impl, OleInitialize,
                RegisterDragDrop, ReleaseStgMedium,
            },
            Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
        },
        UI::{
            Controls::MARGINS,
            HiDpi::GetDpiForWindow,
            Input::KeyboardAndMouse::{ReleaseCapture, SetCapture},
            Shell::{
                CLSID_DragDropHelper, DragQueryFileW, HDROP, IDropTargetHelper,
                IInitializeWithWindow,
            },
            WindowsAndMessaging::{
                CREATESTRUCTW, CW_USEDEFAULT, CloseWindow, CreateWindowExW, DefWindowProcW,
                DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetClientRect, GetSystemMetrics,
                GetWindowLongPtrW, GetWindowRect, HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCAPTION,
                HTCLIENT, HTCLOSE, HTLEFT, HTMAXBUTTON, HTMINBUTTON, HTRIGHT, HTTOP, HTTOPLEFT,
                HTTOPRIGHT, IDC_ARROW, IDC_HAND, IDC_SIZEWE, IDI_APPLICATION, IsWindow, IsZoomed,
                LoadCursorW, LoadIconW, MSG, NCCALCSIZE_PARAMS, PM_REMOVE, PeekMessageW,
                RegisterClassExW, SIZE_MAXIMIZED, SIZE_RESTORED, SM_CXSIZEFRAME, SM_CYSIZEFRAME,
                SW_RESTORE, SW_SHOWMAXIMIZED, SW_SHOWNORMAL, SWP_FRAMECHANGED, SetCursor,
                SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage, WM_ACTIVATE,
                WM_CREATE, WM_DESTROY, WM_DPICHANGED, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
                WM_NCCALCSIZE, WM_NCHITTEST, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP, WM_NCMOUSEMOVE,
                WM_SIZE, WNDCLASS_STYLES, WNDCLASSEXW, WS_EX_APPWINDOW, WS_OVERLAPPEDWINDOW,
            },
        },
    },
    core::{Interface, PCWSTR, h, w},
};
use windows_collections::{IIterable_Impl, IVector, IVector_Impl};
use windows_core::{HSTRING, implement};

use crate::{
    AppEvent, AppEventBus,
    base_system::AppBaseSystem,
    hittest::{CursorShape, HitTestTreeManager, Role},
    input::PointerInputManager,
};

struct WindowState<'sys, 'subsystem> {
    app_event_bus: &'sys AppEventBus,
    ui_scale_factor: Cell<f32>,
    pointer_input_manager: UnsafeCell<PointerInputManager>,
    base_sys: *mut AppBaseSystem<'subsystem>,
}

pub struct AppShell<'sys, 'subsystem> {
    hinstance: HINSTANCE,
    hwnd: HWND,
    hwnd_state: Pin<Box<WindowState<'sys, 'subsystem>>>,
    current_display_refresh_rate_hz: core::pin::Pin<Box<Cell<f32>>>,
    perf_counter_freq: i64,
    next_target_frame_timing: Cell<i64>,
}
impl<'sys, 'base_sys, 'subsystem> AppShell<'sys, 'subsystem> {
    #[tracing::instrument(skip(events, base_sys))]
    pub fn new(events: &'sys AppEventBus, base_sys: *mut AppBaseSystem<'subsystem>) -> Self {
        if let Err(e) = unsafe { OleInitialize(None) } {
            tracing::warn!(reason = ?e, "OleInitialize failed");
        }

        let hinstance =
            unsafe { core::mem::transmute::<_, HINSTANCE>(GetModuleHandleW(None).unwrap()) };

        let current_display_refresh_rate_hz = Box::pin(Cell::new(60.0f32));

        let wc = WNDCLASSEXW {
            cbSize: core::mem::size_of::<WNDCLASSEXW>() as _,
            style: WNDCLASS_STYLES(0),
            lpfnWndProc: Some(Self::wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance,
            hIcon: unsafe { LoadIconW(None, IDI_APPLICATION).unwrap() },
            hCursor: unsafe { LoadCursorW(None, IDC_ARROW).unwrap() },
            hbrBackground: HBRUSH(core::ptr::null_mut()),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: w!("AppShell"),
            hIconSm: unsafe { LoadIconW(None, IDI_APPLICATION).unwrap() },
        };
        let atom = unsafe { RegisterClassExW(&wc) };
        if atom == 0 {
            panic!(
                "Failed to register window class: {:?}",
                std::io::Error::last_os_error()
            );
        }

        let hwnd_state = Box::pin(WindowState {
            app_event_bus: events,
            ui_scale_factor: Cell::new(1.0),
            pointer_input_manager: UnsafeCell::new(PointerInputManager::new(640.0, 480.0)),
            base_sys,
        });
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_APPWINDOW,
                PCWSTR(atom as _),
                w!("Peridot SpriteAtlas Visualizer/Editor"),
                WS_OVERLAPPEDWINDOW,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                None,
                None,
                Some(hinstance),
                Some(&*hwnd_state.as_ref() as *const WindowState as _),
            )
            .unwrap()
        };

        // set dark mode preference
        if let Err(e) = unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                &windows::core::BOOL(true as _) as *const _ as _,
                core::mem::size_of::<windows_core::BOOL>() as _,
            )
        } {
            tracing::warn!(reason = ?e, "activating DWM Immersive Dark Mode failed");
        }

        'try_setup_dnd: {
            let dd_helper = match unsafe {
                CoCreateInstance(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER)
            } {
                Ok(x) => x,
                Err(e) => {
                    tracing::warn!(reason = ?e, "creating DragDropHelper failed");
                    break 'try_setup_dnd;
                }
            };

            if let Err(e) = unsafe {
                RegisterDragDrop(
                    hwnd,
                    &IDropTarget::from(DropTargetHandler {
                        bound_hwnd: hwnd,
                        app_event_bus: events,
                        dd_helper,
                    }),
                )
            } {
                tracing::warn!(reason = ?e, "registering drag drop handler failed");
            }
        }

        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        }

        // notify frame change
        let mut rc = core::mem::MaybeUninit::uninit();
        unsafe {
            GetWindowRect(hwnd, rc.as_mut_ptr()).unwrap();
            let rc = rc.assume_init_ref();
            SetWindowPos(
                hwnd,
                None,
                rc.left,
                rc.top,
                rc.right - rc.left,
                rc.bottom - rc.top,
                SWP_FRAMECHANGED,
            )
            .unwrap();
        }

        let hm = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY) };
        let mut mi = core::mem::MaybeUninit::<MONITORINFOEXW>::uninit();
        let mi = unsafe {
            core::ptr::addr_of_mut!((*mi.as_mut_ptr()).monitorInfo.cbSize)
                .write(core::mem::size_of::<MONITORINFOEXW>() as _);
            GetMonitorInfoW(hm, mi.as_mut_ptr() as _).unwrap();
            mi.assume_init_ref()
        };
        let mut current_mode = core::mem::MaybeUninit::<DEVMODEW>::uninit();
        let current_mode = unsafe {
            core::ptr::addr_of_mut!((*current_mode.as_mut_ptr()).dmSize)
                .write(core::mem::size_of::<DEVMODEW>() as _);
            EnumDisplaySettingsW(
                PCWSTR::from_raw(mi.szDevice.as_ptr()),
                ENUM_CURRENT_SETTINGS,
                current_mode.as_mut_ptr(),
            )
            .unwrap();
            current_mode.assume_init_ref()
        };
        tracing::debug!(
            bits_per_pel = current_mode.dmBitsPerPel,
            pels_width = current_mode.dmPelsWidth,
            pels_height = current_mode.dmPelsHeight,
            display_freq = current_mode.dmDisplayFrequency,
            "Current Monitor Settings"
        );
        current_display_refresh_rate_hz
            .as_ref()
            .set(current_mode.dmDisplayFrequency as _);

        // フレームタイミング計算用データを取得
        let mut perf_counter_freq = 0i64;
        let mut current_perf_counter = 0i64;
        unsafe {
            // always success on Windows XP or later: https://learn.microsoft.com/ja-jp/windows/win32/api/profileapi/nf-profileapi-queryperformancecounter
            QueryPerformanceFrequency(&mut perf_counter_freq as _).unwrap_unchecked();
            QueryPerformanceCounter(&mut current_perf_counter as _).unwrap_unchecked();
        }

        Self {
            hinstance,
            hwnd,
            hwnd_state,
            perf_counter_freq,
            next_target_frame_timing: Cell::new(
                current_perf_counter
                    + (perf_counter_freq as f64 / current_display_refresh_rate_hz.get() as f64)
                        as i64,
            ),
            current_display_refresh_rate_hz,
        }
    }

    #[inline]
    fn window_state_ref<'a>(hwnd: HWND) -> &'a WindowState<'sys, 'subsystem> {
        unsafe { &*core::ptr::with_exposed_provenance(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as _) }
    }

    extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if msg == WM_CREATE {
            // setup state
            let state = unsafe {
                &mut *((&*core::ptr::with_exposed_provenance_mut::<CREATESTRUCTW>(
                    lparam.0 as usize,
                ))
                    .lpCreateParams as *mut WindowState)
            };
            // 96dpi as base
            let ui_scale_factor = unsafe { GetDpiForWindow(hwnd) as f32 / 96.0 };
            let mut rc = core::mem::MaybeUninit::uninit();
            let rc = unsafe {
                GetClientRect(hwnd, rc.as_mut_ptr()).unwrap_unchecked();
                rc.assume_init_ref()
            };

            state.ui_scale_factor.set(ui_scale_factor);
            state.pointer_input_manager.get_mut().set_client_size(
                (rc.right - rc.left) as f32 / ui_scale_factor,
                (rc.bottom - rc.top) as f32 / ui_scale_factor,
            );

            // store state ptr
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as *mut _ as _);
            }

            return LRESULT(0);
        }

        if msg == WM_DESTROY {
            Self::window_state_ref(hwnd)
                .app_event_bus
                .push(AppEvent::ToplevelWindowClose);

            return LRESULT(0);
        }

        if msg == WM_ACTIVATE {
            unsafe {
                DwmExtendFrameIntoClientArea(
                    hwnd,
                    &MARGINS {
                        cxLeftWidth: 1,
                        cxRightWidth: 1,
                        cyTopHeight: 1,
                        cyBottomHeight: 1,
                    },
                )
                .unwrap();
            }

            return LRESULT(0);
        }

        if msg == WM_NCCALCSIZE {
            if wparam.0 == 0 {
                // not needed to reply client area
                return LRESULT(0);
            }

            // remove non-client area
            let params = unsafe {
                &mut *core::ptr::with_exposed_provenance_mut::<NCCALCSIZE_PARAMS>(lparam.0 as _)
            };
            let w = unsafe { GetSystemMetrics(SM_CXSIZEFRAME) };
            let h = unsafe { GetSystemMetrics(SM_CYSIZEFRAME) };
            params.rgrc[0].left += w;
            params.rgrc[0].right -= w;
            params.rgrc[0].bottom -= h;
            // topはいじらない（他アプリもそんな感じになってるのでtopは自前でNCHITTESTしてリサイズ判定するらしい）

            return LRESULT(0);
        }

        if msg == WM_NCHITTEST {
            let mut p = [POINT {
                x: (lparam.0 & 0xffff) as i16 as _,
                y: ((lparam.0 >> 16) & 0xffff) as i16 as _,
            }];
            unsafe {
                MapWindowPoints(None, Some(hwnd), &mut p);
            }
            let [
                POINT {
                    x: pointer_x_px,
                    y: pointer_y_px,
                },
            ] = p;

            let mut rc = core::mem::MaybeUninit::uninit();
            unsafe {
                GetClientRect(hwnd, rc.as_mut_ptr()).unwrap();
            }
            let rc = unsafe { rc.assume_init_ref() };

            if 0 > pointer_x_px
                || pointer_x_px > (rc.right - rc.left)
                || 0 > pointer_y_px
                || pointer_y_px > (rc.bottom - rc.top)
            {
                // ウィンドウ範囲外はシステムにおまかせ
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            }

            let resize_h = unsafe { GetSystemMetrics(SM_CYSIZEFRAME) };
            if pointer_y_px < resize_h {
                // global override
                return LRESULT(HTTOP as _);
            }

            let state_ref = Self::window_state_ref(hwnd);
            let ui_scale_factor = state_ref.ui_scale_factor.get();
            let pointer_input_manager = unsafe { &*state_ref.pointer_input_manager.get() };
            let base_sys = unsafe { &mut *state_ref.base_sys };

            return match pointer_input_manager.role(
                pointer_x_px as f32 / ui_scale_factor,
                pointer_y_px as f32 / ui_scale_factor,
                (rc.right - rc.left) as f32 / ui_scale_factor,
                (rc.bottom - rc.top) as f32 / ui_scale_factor,
                &base_sys.hit_tree,
                HitTestTreeManager::ROOT,
            ) {
                None => LRESULT(HTCLIENT as _),
                Some(Role::TitleBar) => LRESULT(HTCAPTION as _),
                Some(Role::ForceClient) => LRESULT(HTCLIENT as _),
                Some(Role::CloseButton) => LRESULT(HTCLOSE as _),
                Some(Role::MaximizeButton) => LRESULT(HTMAXBUTTON as _),
                Some(Role::MinimizeButton) => LRESULT(HTMINBUTTON as _),
                // Windowsだと同じ位置にあるので同じものを返す
                Some(Role::RestoreButton) => LRESULT(HTMAXBUTTON as _),
            };
        }

        if msg == WM_DPICHANGED {
            Self::window_state_ref(hwnd)
                .ui_scale_factor
                .set((wparam.0 & 0xffff) as u16 as f32 / 96.0);
            return LRESULT(0);
        }

        if msg == WM_SIZE {
            let stref = Self::window_state_ref(hwnd);
            let width_px = (lparam.0 & 0xffff) as u16 as u32;
            let height_px = ((lparam.0 >> 16) & 0xffff) as u16 as u32;
            stref.app_event_bus.push(AppEvent::ToplevelWindowNewSize {
                width_px,
                height_px,
            });
            unsafe { &mut *stref.pointer_input_manager.get() }.set_client_size(
                width_px as f32 / stref.ui_scale_factor.get(),
                height_px as f32 / stref.ui_scale_factor.get(),
            );

            if wparam.0 == SIZE_MAXIMIZED as _ {
                stref
                    .app_event_bus
                    .push(AppEvent::MainWindowTiledStateChanged { is_tiled: true });
            }
            if wparam.0 == SIZE_RESTORED as _ {
                stref
                    .app_event_bus
                    .push(AppEvent::MainWindowTiledStateChanged { is_tiled: false });
            }

            return LRESULT(0);
        }

        if (msg == WM_NCMOUSEMOVE || msg == WM_NCLBUTTONDOWN || msg == WM_NCLBUTTONUP)
            && (wparam.0 == HTTOP as _
                || wparam.0 == HTBOTTOM as _
                || wparam.0 == HTLEFT as _
                || wparam.0 == HTRIGHT as _
                || wparam.0 == HTTOPLEFT as _
                || wparam.0 == HTTOPRIGHT as _
                || wparam.0 == HTBOTTOMLEFT as _
                || wparam.0 == HTBOTTOMRIGHT as _)
        {
            // リサイズ境界上の処理はシステムにおまかせ
            return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
        }

        if (msg == WM_NCLBUTTONDOWN || msg == WM_NCLBUTTONUP) && wparam.0 == HTCAPTION as _ {
            // TitleBarの挙動はシステムにおまかせ
            return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
        }

        if msg == WM_LBUTTONDOWN || msg == WM_NCLBUTTONDOWN {
            Self::window_state_ref(hwnd)
                .app_event_bus
                .push(AppEvent::MainWindowPointerLeftDown);
            return LRESULT(0);
        }

        if msg == WM_LBUTTONUP || msg == WM_NCLBUTTONUP {
            Self::window_state_ref(hwnd)
                .app_event_bus
                .push(AppEvent::MainWindowPointerLeftUp);
            return LRESULT(0);
        }

        if msg == WM_MOUSEMOVE {
            let state_ref = Self::window_state_ref(hwnd);
            let ui_scale_factor = state_ref.ui_scale_factor.get();

            state_ref
                .app_event_bus
                .push(AppEvent::MainWindowPointerMove {
                    surface_x: (lparam.0 & 0xffff) as i16 as f32 / ui_scale_factor,
                    surface_y: ((lparam.0 >> 16) & 0xffff) as i16 as f32 / ui_scale_factor,
                });
            return LRESULT(0);
        }

        if msg == WM_NCMOUSEMOVE {
            let mut p = [POINT {
                x: (lparam.0 & 0xffff) as i16 as _,
                y: ((lparam.0 >> 16) & 0xffff) as i16 as _,
            }];
            unsafe {
                MapWindowPoints(None, Some(hwnd), &mut p);
            }

            let state_ref = Self::window_state_ref(hwnd);
            let ui_scale_factor = state_ref.ui_scale_factor.get();

            state_ref
                .app_event_bus
                .push(AppEvent::MainWindowPointerMove {
                    surface_x: p[0].x as f32 / ui_scale_factor,
                    surface_y: p[0].y as f32 / ui_scale_factor,
                });
            // Note: NCMOUSEMOVEはデフォルト動作もさせる
        }

        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    pub const fn needs_window_command_buttons(&self) -> bool {
        // for Windows: window command buttons is always required
        true
    }

    pub const fn server_side_decoration_provided(&self) -> bool {
        // windows always providing server(system)-side decoration
        true
    }

    #[inline]
    pub fn is_tiled(&self) -> bool {
        unsafe { IsZoomed(self.hwnd).as_bool() }
    }

    pub fn pointer_input_manager(&self) -> &UnsafeCell<PointerInputManager> {
        &self.hwnd_state.pointer_input_manager
    }

    pub unsafe fn create_vulkan_surface(
        &mut self,
        instance: &impl br::Instance,
    ) -> br::Result<br::vk::VkSurfaceKHR> {
        unsafe {
            br::Win32SurfaceCreateInfo::new(
                core::mem::transmute(self.hinstance),
                core::mem::transmute(self.hwnd),
            )
            .execute(instance, None)
        }
    }

    pub fn client_size(&self) -> (f32, f32) {
        let ui_scale_factor = self.hwnd_state.ui_scale_factor.get();

        let mut rc = core::mem::MaybeUninit::uninit();
        unsafe {
            GetClientRect(self.hwnd, rc.as_mut_ptr()).unwrap();
        }
        let rc = unsafe { rc.assume_init_ref() };

        (
            (rc.right - rc.left) as f32 / ui_scale_factor,
            (rc.bottom - rc.top) as f32 / ui_scale_factor,
        )
    }

    pub fn client_size_pixels(&self) -> (u32, u32) {
        let mut rc = core::mem::MaybeUninit::uninit();
        unsafe {
            GetClientRect(self.hwnd, rc.as_mut_ptr()).unwrap();
        }
        let rc = unsafe { rc.assume_init_ref() };

        ((rc.right - rc.left) as _, (rc.bottom - rc.top) as _)
    }

    pub fn flush(&mut self) {}

    pub fn process_pending_events(&self) {
        let mut msg = core::mem::MaybeUninit::<MSG>::uninit();
        while unsafe { PeekMessageW(msg.as_mut_ptr(), None, 0, 0, PM_REMOVE).0 != 0 } {
            unsafe {
                let _ = TranslateMessage(msg.assume_init_ref());
                DispatchMessageW(msg.assume_init_ref());
            }
        }
    }

    pub fn request_next_frame(&self) {
        unsafe {
            QueryPerformanceCounter(self.next_target_frame_timing.as_ptr()).unwrap_unchecked();
        }

        self.next_target_frame_timing.update(|v| {
            v + (self.perf_counter_freq as f64 / self.current_display_refresh_rate_hz.get() as f64)
                as i64
        });
    }

    // windows only
    pub fn next_frame_left_ms(&self) -> i64 {
        let mut cur = 0i64;
        unsafe {
            QueryPerformanceCounter(&mut cur as _).unwrap_unchecked();
        }

        ((self.next_target_frame_timing.get() - cur).max(0) as f64 * 1000.0
            / self.perf_counter_freq as f64)
            .trunc() as _
    }

    // windows only
    pub const fn hwnd(&self) -> HWND {
        self.hwnd
    }

    #[tracing::instrument(skip(self))]
    pub fn set_cursor_shape(&self, shape: CursorShape) {
        unsafe {
            SetCursor(match shape {
                // TODO: 必要ならキャッシュする
                CursorShape::Default => Some(LoadCursorW(None, IDC_ARROW).unwrap()),
                CursorShape::Pointer => Some(LoadCursorW(None, IDC_HAND).unwrap()),
                CursorShape::ResizeHorizontal => Some(LoadCursorW(None, IDC_SIZEWE).unwrap()),
            });
        }
    }

    #[inline]
    pub fn ui_scale_factor(&self) -> f32 {
        self.hwnd_state.ui_scale_factor.get()
    }

    #[inline]
    pub fn capture_pointer(&self) {
        unsafe {
            SetCapture(self.hwnd);
        }
    }

    #[inline]
    pub fn release_pointer(&self) {
        if let Err(e) = unsafe { ReleaseCapture() } {
            tracing::warn!(reason = ?e, "ReleaseCapture() failed");
        }
    }

    #[inline]
    pub fn minimize(&self) {
        if let Err(e) = unsafe { CloseWindow(self.hwnd) } {
            tracing::warn!(reason = ?e, "CloseWindow failed");
        }
    }

    #[inline]
    pub fn toggle_maximize_restore(&self) {
        if self.is_tiled() {
            let _ = unsafe { ShowWindow(self.hwnd, SW_RESTORE) };
        } else {
            let _ = unsafe { ShowWindow(self.hwnd, SW_SHOWMAXIMIZED) };
        }
    }

    #[inline]
    pub fn close_safe(&self) {
        if unsafe { !IsWindow(Some(self.hwnd)).as_bool() } {
            // already destroyed
            return;
        }

        if let Err(e) = unsafe { DestroyWindow(self.hwnd) } {
            tracing::warn!(reason = ?e, "DestroyWindow failed");
        }
    }
}

#[implement(IDropTarget)]
pub struct DropTargetHandler<'sys> {
    bound_hwnd: HWND,
    app_event_bus: &'sys AppEventBus,
    dd_helper: IDropTargetHelper,
}
impl IDropTarget_Impl for DropTargetHandler_Impl<'_> {
    fn DragEnter(
        &self,
        pdataobj: windows_core::Ref<'_, windows::Win32::System::Com::IDataObject>,
        _grfkeystate: windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS,
        pt: &windows::Win32::Foundation::POINTL,
        pdweffect: *mut windows::Win32::System::Ole::DROPEFFECT,
    ) -> windows_core::Result<()> {
        unsafe {
            self.dd_helper.DragEnter(
                self.bound_hwnd,
                pdataobj.unwrap(),
                &POINT { x: pt.x, y: pt.y },
                core::ptr::read(pdweffect),
            )?;
        }
        self.app_event_bus.push(AppEvent::UIShowDragAndDropOverlay);
        unsafe {
            core::ptr::write(pdweffect, DROPEFFECT_LINK);
        }

        Ok(())
    }

    fn DragLeave(&self) -> windows_core::Result<()> {
        unsafe {
            self.dd_helper.DragLeave()?;
        }
        self.app_event_bus.push(AppEvent::UIHideDragAndDropOverlay);

        Ok(())
    }

    fn DragOver(
        &self,
        _grfkeystate: windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS,
        pt: &windows::Win32::Foundation::POINTL,
        pdweffect: *mut windows::Win32::System::Ole::DROPEFFECT,
    ) -> windows_core::Result<()> {
        unsafe {
            self.dd_helper
                .DragOver(&POINT { x: pt.x, y: pt.y }, core::ptr::read(pdweffect))?;
            core::ptr::write(pdweffect, DROPEFFECT_LINK);
        }

        Ok(())
    }

    fn Drop(
        &self,
        pdataobj: windows_core::Ref<'_, windows::Win32::System::Com::IDataObject>,
        _grfkeystate: windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS,
        pt: &windows::Win32::Foundation::POINTL,
        pdweffect: *mut windows::Win32::System::Ole::DROPEFFECT,
    ) -> windows_core::Result<()> {
        let data = OwnedStgMedium(unsafe {
            pdataobj
                .unwrap()
                .GetData(&FORMATETC {
                    cfFormat: CF_HDROP.0,
                    ptd: core::ptr::null_mut(),
                    dwAspect: DVASPECT_CONTENT.0,
                    lindex: -1,
                    tymed: TYMED_HGLOBAL.0 as _,
                })
                .unwrap()
        });
        let gl = unsafe { LockedHGLOBAL::acquire(data.hglobal_unchecked()).unwrap() };
        let hdrop: HDROP = unsafe { core::mem::transmute(gl.ptr) };
        let file_count = unsafe { DragQueryFileW(hdrop, 0xffff_ffff, None) };
        let mut file_paths = Vec::with_capacity(file_count as _);
        for n in 0..file_count {
            let len = unsafe { DragQueryFileW(hdrop, n, None) };
            let mut path = Vec::with_capacity((len + 1) as _);
            unsafe {
                path.set_len(path.capacity());
            }
            if unsafe { DragQueryFileW(hdrop, n, Some(&mut path)) } == 0 {
                tracing::error!("DragQueryFileW(querying file path) failed");
                panic!("cannot continue");
            }

            file_paths.push(PathBuf::from(std::ffi::OsString::from_wide(
                &path[..path.len() - 1],
            )));
        }
        drop((gl, data));
        self.app_event_bus
            .push(AppEvent::AddSpriteByPathList(file_paths));

        unsafe {
            self.dd_helper.Drop(
                pdataobj.unwrap(),
                &POINT { x: pt.x, y: pt.y },
                core::ptr::read(pdweffect),
            )?;
        }
        self.app_event_bus.push(AppEvent::UIHideDragAndDropOverlay);
        unsafe {
            core::ptr::write(pdweffect, DROPEFFECT_LINK);
        }

        Ok(())
    }
}

#[repr(transparent)]
struct OwnedStgMedium(pub STGMEDIUM);
impl Drop for OwnedStgMedium {
    #[inline]
    fn drop(&mut self) {
        unsafe {
            ReleaseStgMedium(&mut self.0);
        }
    }
}
impl OwnedStgMedium {
    pub const unsafe fn hglobal_unchecked(&self) -> HGLOBAL {
        unsafe { self.0.u.hGlobal }
    }
}

struct LockedHGLOBAL {
    handle: HGLOBAL,
    ptr: *mut core::ffi::c_void,
}
impl Drop for LockedHGLOBAL {
    fn drop(&mut self) {
        if let Err(e) = unsafe { GlobalUnlock(self.handle) }
            && e.code() != windows::Win32::Foundation::S_OK
        {
            // なぜかErrなのに中身S_OKで返ってくることがあるらしい
            tracing::warn!(reason = ?e, "GlobalUnlock failed");
        }
    }
}
impl LockedHGLOBAL {
    pub unsafe fn acquire(handle: HGLOBAL) -> windows::core::Result<Self> {
        let ptr = unsafe { GlobalLock(handle) };
        if ptr.is_null() {
            return Err(unsafe { GetLastError().into() });
        }

        Ok(Self { handle, ptr })
    }
}

#[implement(IVector<T>)]
#[repr(transparent)]
pub struct ReadOnlySliceAsVector<'xs, T>(pub &'xs [T])
where
    T: windows_core::RuntimeType + 'static;
impl<'xs, T: windows_core::RuntimeType + 'static> IIterable_Impl<T>
    for ReadOnlySliceAsVector_Impl<'xs, T>
{
    fn First(&self) -> windows_core::Result<windows_collections::IIterator<T>> {
        Err(E_NOTIMPL.into())
    }
}
impl<'xs, T: windows_core::RuntimeType + 'static> IVector_Impl<T>
    for ReadOnlySliceAsVector_Impl<'xs, T>
{
    fn Append(&self, _value: windows_core::Ref<'_, T>) -> windows_core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn Clear(&self) -> windows_core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn GetAt(&self, index: u32) -> windows_core::Result<T> {
        Ok(self.0[index as usize].clone())
    }

    fn GetMany(
        &self,
        _start_index: u32,
        _items: &mut [<T as windows_core::Type<T>>::Default],
    ) -> windows_core::Result<u32> {
        Err(E_NOTIMPL.into())
    }

    fn GetView(&self) -> windows_core::Result<windows_collections::IVectorView<T>> {
        Err(E_NOTIMPL.into())
    }

    fn IndexOf(
        &self,
        _value: windows_core::Ref<'_, T>,
        _index: &mut u32,
    ) -> windows_core::Result<bool> {
        Err(E_NOTIMPL.into())
    }

    fn InsertAt(&self, _index: u32, _value: windows_core::Ref<'_, T>) -> windows_core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn RemoveAt(&self, _index: u32) -> windows_core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn RemoveAtEnd(&self) -> windows_core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn ReplaceAll(
        &self,
        _items: &[<T as windows_core::Type<T>>::Default],
    ) -> windows_core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn SetAt(&self, _index: u32, _value: windows_core::Ref<'_, T>) -> windows_core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn Size(&self) -> windows_core::Result<u32> {
        Ok(self.0.len() as _)
    }
}
