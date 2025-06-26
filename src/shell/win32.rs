use std::cell::Cell;

use bedrock::{self as br, SurfaceCreateInfo};
use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
        Graphics::Gdi::HBRUSH,
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::{
                DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow,
                SetProcessDpiAwarenessContext,
            },
            WindowsAndMessaging::{
                CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DispatchMessageW, GWLP_USERDATA,
                GetClientRect, GetWindowLongPtrW, IDC_ARROW, IDC_SIZEWE, IDI_APPLICATION,
                LoadCursorW, LoadIconW, MSG, PM_REMOVE, PeekMessageW, PostQuitMessage,
                RegisterClassExW, SW_SHOWNORMAL, SetCursor, SetWindowLongPtrW, ShowWindow,
                TranslateMessage, WINDOW_LONG_PTR_INDEX, WM_DESTROY, WM_DPICHANGED, WM_LBUTTONDOWN,
                WM_LBUTTONUP, WM_MOUSEMOVE, WM_SIZE, WNDCLASS_STYLES, WNDCLASSEXW, WS_EX_APPWINDOW,
                WS_EX_NOREDIRECTIONBITMAP, WS_OVERLAPPEDWINDOW,
            },
        },
    },
    core::{PCWSTR, w},
};

use crate::{AppEvent, AppEventBus, hittest::CursorShape};

pub struct AppShell<'a> {
    hinstance: HINSTANCE,
    hwnd: HWND,
    ui_scale_factor: core::pin::Pin<Box<Cell<f32>>>,
    app_event_queue: &'a AppEventBus,
}
impl<'a> AppShell<'a> {
    #[tracing::instrument(skip(events))]
    pub fn new(events: &'a AppEventBus) -> Self {
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

    extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if msg == WM_DESTROY {
            let app_event_bus = unsafe {
                &*(core::ptr::with_exposed_provenance::<AppEventBus>(GetWindowLongPtrW(
                    hwnd,
                    GWLP_USERDATA,
                ) as _))
            };

            app_event_bus.push(AppEvent::ToplevelWindowClose);
            return LRESULT(0);
        }

        if msg == WM_DPICHANGED {
            let ui_scale_factor = unsafe {
                &*(core::ptr::with_exposed_provenance::<Cell<f32>>(GetWindowLongPtrW(
                    hwnd,
                    WINDOW_LONG_PTR_INDEX(0),
                ) as _))
            };

            ui_scale_factor.set((wparam.0 & 0xffff) as u16 as _);
            return LRESULT(0);
        }

        if msg == WM_SIZE {
            let app_event_bus = unsafe {
                &*(core::ptr::with_exposed_provenance::<AppEventBus>(GetWindowLongPtrW(
                    hwnd,
                    GWLP_USERDATA,
                ) as _))
            };
            let ui_scale_factor = unsafe {
                &*(core::ptr::with_exposed_provenance::<Cell<f32>>(GetWindowLongPtrW(
                    hwnd,
                    WINDOW_LONG_PTR_INDEX(0),
                ) as _))
            };

            // この順番で送ればok(Wayland側の仕様 あれに依存するのやめたいがどうしよう)
            app_event_bus.push(AppEvent::ToplevelWindowConfigure {
                width: ((lparam.0 & 0xffff) as u16 as f32 / ui_scale_factor.get()) as _,
                height: (((lparam.0 >> 16) & 0xffff) as u16 as f32 / ui_scale_factor.get()) as _,
            });
            app_event_bus.push(AppEvent::ToplevelWindowSurfaceConfigure { serial: 0 });
            return LRESULT(0);
        }

        if msg == WM_LBUTTONDOWN {
            let app_event_bus = unsafe {
                &*(core::ptr::with_exposed_provenance::<AppEventBus>(GetWindowLongPtrW(
                    hwnd,
                    GWLP_USERDATA,
                ) as _))
            };

            app_event_bus.push(AppEvent::MainWindowPointerLeftDown { enter_serial: 0 });
            return LRESULT(0);
        }

        if msg == WM_LBUTTONUP {
            let app_event_bus = unsafe {
                &*(core::ptr::with_exposed_provenance::<AppEventBus>(GetWindowLongPtrW(
                    hwnd,
                    GWLP_USERDATA,
                ) as _))
            };

            app_event_bus.push(AppEvent::MainWindowPointerLeftUp { enter_serial: 0 });
            return LRESULT(0);
        }

        if msg == WM_MOUSEMOVE {
            let app_event_bus = unsafe {
                &*(core::ptr::with_exposed_provenance::<AppEventBus>(GetWindowLongPtrW(
                    hwnd,
                    GWLP_USERDATA,
                ) as _))
            };
            let ui_scale_factor = unsafe {
                &*(core::ptr::with_exposed_provenance::<Cell<f32>>(GetWindowLongPtrW(
                    hwnd,
                    WINDOW_LONG_PTR_INDEX(0),
                ) as _))
            };

            app_event_bus.push(AppEvent::MainWindowPointerMove {
                enter_serial: 0,
                surface_x: (lparam.0 & 0xffff) as i16 as f32 / ui_scale_factor.get(),
                surface_y: ((lparam.0 >> 16) & 0xffff) as i16 as f32 / ui_scale_factor.get(),
            });
            return LRESULT(0);
        }

        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
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
        unsafe {
            let r = rc.assume_init_ref();

            (
                (r.right - r.left) as f32 / ui_scale_factor,
                (r.bottom - r.top) as f32 / ui_scale_factor,
            )
        }
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
                TranslateMessage(msg.assume_init_ref());
                DispatchMessageW(msg.assume_init_ref());
            }
        }

        // TODO: いったんあいたタイミングをFrameTimingとする あとで適切にスリープいれてあげたい気持ち
        self.app_event_queue
            .push(AppEvent::ToplevelWindowFrameTiming);
        Ok(())
    }

    pub fn cancel_read_events(&mut self) {}

    #[tracing::instrument(skip(self))]
    pub fn read_and_process_events(&mut self) -> std::io::Result<()> {
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
