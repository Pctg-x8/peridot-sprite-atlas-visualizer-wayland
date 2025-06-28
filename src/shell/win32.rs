use std::cell::Cell;

use bedrock::{self as br, SurfaceCreateInfo};
use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
        Graphics::{Dwm::DwmExtendFrameIntoClientArea, Gdi::HBRUSH},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Controls::MARGINS,
            HiDpi::{
                DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow,
                SetProcessDpiAwarenessContext,
            },
            WindowsAndMessaging::{
                CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DispatchMessageW, GWLP_USERDATA,
                GetClientRect, GetSystemMetrics, GetWindowLongPtrW, GetWindowRect, IDC_ARROW,
                IDC_SIZEWE, IDI_APPLICATION, LoadCursorW, LoadIconW, MSG, NCCALCSIZE_PARAMS,
                PM_REMOVE, PeekMessageW, RegisterClassExW, SM_CXSIZEFRAME, SM_CYSIZEFRAME,
                SW_SHOWNORMAL, SWP_FRAMECHANGED, SetCursor, SetWindowLongPtrW, SetWindowPos,
                ShowWindow, TranslateMessage, WINDOW_LONG_PTR_INDEX, WM_ACTIVATE, WM_DESTROY,
                WM_DPICHANGED, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCCALCSIZE,
                WM_NCHITTEST, WM_SIZE, WNDCLASS_STYLES, WNDCLASSEXW, WS_EX_APPWINDOW,
                WS_OVERLAPPEDWINDOW,
            },
        },
    },
    core::{PCWSTR, w},
};

use crate::{AppEvent, AppEventBus, hittest::CursorShape};

pub struct AppShell<'sys> {
    hinstance: HINSTANCE,
    hwnd: HWND,
    ui_scale_factor: core::pin::Pin<Box<Cell<f32>>>,
    app_event_queue: &'sys AppEventBus,
}
impl<'sys> AppShell<'sys> {
    #[tracing::instrument(skip(events))]
    pub fn new(events: &'sys AppEventBus) -> Self {
        let hinstance =
            unsafe { core::mem::transmute::<_, HINSTANCE>(GetModuleHandleW(None).unwrap()) };

        unsafe {
            // TODO: マニフェストで設定する
            SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).unwrap();
        }
        let ui_scale_factor = Box::pin(Cell::new(1.0));

        let wc = WNDCLASSEXW {
            cbSize: core::mem::size_of::<WNDCLASSEXW>() as _,
            style: WNDCLASS_STYLES(0),
            lpfnWndProc: Some(Self::wndproc),
            cbClsExtra: 0,
            cbWndExtra: core::mem::size_of::<*const Cell<f32>>() as _,
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
                None,
            )
            .unwrap()
        };
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, events as *const _ as _);
            SetWindowLongPtrW(
                hwnd,
                WINDOW_LONG_PTR_INDEX(0),
                ui_scale_factor.as_ref().get_ref() as *const _ as _,
            );
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

        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
            // 96dpi as base
            ui_scale_factor
                .as_ref()
                .set(GetDpiForWindow(hwnd) as f32 / 96.0);
        }

        Self {
            hinstance,
            hwnd,
            ui_scale_factor,
            app_event_queue: events,
        }
    }

    #[inline(always)]
    fn app_event_bus<'a>(hwnd: HWND) -> &'a AppEventBus {
        unsafe {
            &*core::ptr::with_exposed_provenance::<AppEventBus>(GetWindowLongPtrW(
                hwnd,
                GWLP_USERDATA,
            ) as _)
        }
    }

    #[inline(always)]
    fn ui_scale_factor_cell<'a>(hwnd: HWND) -> &'a Cell<f32> {
        unsafe {
            &*core::ptr::with_exposed_provenance::<Cell<f32>>(GetWindowLongPtrW(
                hwnd,
                WINDOW_LONG_PTR_INDEX(0),
            ) as _)
        }
    }

    extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if msg == WM_DESTROY {
            Self::app_event_bus(hwnd).push(AppEvent::ToplevelWindowClose);
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
            // TODO: nc hittest
        }

        if msg == WM_DPICHANGED {
            Self::ui_scale_factor_cell(hwnd).set((wparam.0 & 0xffff) as u16 as _);
            return LRESULT(0);
        }

        if msg == WM_SIZE {
            let app_event_bus = Self::app_event_bus(hwnd);
            let ui_scale_factor = Self::ui_scale_factor_cell(hwnd).get();

            // この順番で送ればok(Wayland側の仕様 あれに依存するのやめたいがどうしよう)
            app_event_bus.push(AppEvent::ToplevelWindowConfigure {
                width: ((lparam.0 & 0xffff) as u16 as f32 / ui_scale_factor) as _,
                height: (((lparam.0 >> 16) & 0xffff) as u16 as f32 / ui_scale_factor) as _,
            });
            app_event_bus.push(AppEvent::ToplevelWindowSurfaceConfigure { serial: 0 });
            return LRESULT(0);
        }

        if msg == WM_LBUTTONDOWN {
            Self::app_event_bus(hwnd).push(AppEvent::MainWindowPointerLeftDown { enter_serial: 0 });
            return LRESULT(0);
        }

        if msg == WM_LBUTTONUP {
            Self::app_event_bus(hwnd).push(AppEvent::MainWindowPointerLeftUp { enter_serial: 0 });
            return LRESULT(0);
        }

        if msg == WM_MOUSEMOVE {
            let ui_scale_factor = Self::ui_scale_factor_cell(hwnd).get();

            Self::app_event_bus(hwnd).push(AppEvent::MainWindowPointerMove {
                enter_serial: 0,
                surface_x: (lparam.0 & 0xffff) as i16 as f32 / ui_scale_factor,
                surface_y: ((lparam.0 >> 16) & 0xffff) as i16 as f32 / ui_scale_factor,
            });
            return LRESULT(0);
        }

        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    pub const fn is_floating_window(&self) -> bool {
        true
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
        let ui_scale_factor = self.ui_scale_factor.get();

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

    #[tracing::instrument(skip(self))]
    pub fn flush(&mut self) {}

    #[tracing::instrument(skip(self))]
    pub fn process_pending_events(&mut self) {}

    #[tracing::instrument(skip(self))]
    pub fn prepare_read_events(&mut self) -> std::io::Result<()> {
        let mut msg = core::mem::MaybeUninit::<MSG>::uninit();
        while unsafe { PeekMessageW(msg.as_mut_ptr(), None, 0, 0, PM_REMOVE).0 != 0 } {
            unsafe {
                let _ = TranslateMessage(msg.assume_init_ref());
                DispatchMessageW(msg.assume_init_ref());
            }
        }

        // TODO: いったんあいたタイミングをFrameTimingとする あとで適切にスリープいれてあげたい気持ち
        self.app_event_queue
            .push(AppEvent::ToplevelWindowFrameTiming);
        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn request_next_frame(&mut self) {}

    #[tracing::instrument(skip(self))]
    pub fn post_configure(&mut self, _serial: u32) {}

    #[tracing::instrument(skip(self))]
    pub fn set_cursor_shape(&mut self, _enter_serial: u32, shape: CursorShape) {
        unsafe {
            SetCursor(match shape {
                // TODO: 必要ならキャッシュする
                CursorShape::Default => Some(LoadCursorW(None, IDC_ARROW).unwrap()),
                CursorShape::ResizeHorizontal => Some(LoadCursorW(None, IDC_SIZEWE).unwrap()),
            });
        }
    }

    #[inline]
    pub fn ui_scale_factor(&self) -> f32 {
        self.ui_scale_factor.get()
    }
}
