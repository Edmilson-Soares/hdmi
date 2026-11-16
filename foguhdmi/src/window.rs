//! Janela que mostra o canvas num monitor. Roda numa thread própria com o
//! loop de mensagens do Windows (o fogu:ffi não tem callbacks); o Fogu só
//! desenha no canvas e chama present(), que copia o quadro para `front`.

use std::collections::VecDeque;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, Once};
use std::thread::JoinHandle;

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{SetFocus, VK_ESCAPE};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub struct Shared {
    pub front: Vec<u32>,
    pub w: i32,
    pub h: i32,
    pub open: bool,
    pub keys: VecDeque<i64>,
    pub mouse: (i32, i32, i32),
}

pub struct Window {
    hwnd: isize,
    pub shared: Arc<Mutex<Shared>>,
    pub w: i32,
    pub h: i32,
    thread: Option<JoinHandle<()>>,
}

const CLASS: &str = "FoguHdmiWindow";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn lock(s: &Mutex<Shared>) -> std::sync::MutexGuard<'_, Shared> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        let shared = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Mutex<Shared>;
        match msg {
            WM_PAINT => {
                let mut ps: PAINTSTRUCT = zeroed();
                let dc = BeginPaint(hwnd, &mut ps);
                if !shared.is_null() {
                    let s = lock(&*shared);
                    let mut bi: BITMAPINFO = zeroed();
                    bi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
                    bi.bmiHeader.biWidth = s.w;
                    bi.bmiHeader.biHeight = -s.h;
                    bi.bmiHeader.biPlanes = 1;
                    bi.bmiHeader.biBitCount = 32;
                    bi.bmiHeader.biCompression = BI_RGB;
                    SetDIBitsToDevice(dc, 0, 0, s.w as u32, s.h as u32, 0, 0, 0, s.h as u32, s.front.as_ptr() as *const _, &bi, DIB_RGB_COLORS);
                }
                EndPaint(hwnd, &ps);
                0
            }
            WM_ERASEBKGND => 1,
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                if wp == VK_ESCAPE as usize {
                    DestroyWindow(hwnd);
                    return 0;
                }
                if !shared.is_null() {
                    let mut s = lock(&*shared);
                    if s.keys.len() >= 64 {
                        s.keys.pop_front();
                    }
                    s.keys.push_back(wp as i64);
                }
                if msg == WM_SYSKEYDOWN { DefWindowProcW(hwnd, msg, wp, lp) } else { 0 }
            }
            WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MBUTTONDOWN | WM_MBUTTONUP => {
                if !shared.is_null() {
                    let x = (lp & 0xFFFF) as u16 as i16 as i32;
                    let y = ((lp >> 16) & 0xFFFF) as u16 as i16 as i32;
                    let mut b = 0;
                    if wp & 0x0001 != 0 { b += 1; }
                    if wp & 0x0002 != 0 { b += 2; }
                    if wp & 0x0010 != 0 { b += 4; }
                    lock(&*shared).mouse = (x, y, b);
                }
                0
            }
            WM_DESTROY => {
                if !shared.is_null() {
                    lock(&*shared).open = false;
                }
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn register_class() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        let name = wide(CLASS);
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: GetModuleHandleW(null()),
            hIcon: null_mut(),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: null_mut(),
            lpszMenuName: null(),
            lpszClassName: name.as_ptr(),
            hIconSm: null_mut(),
        };
        RegisterClassExW(&wc);
    });
}

impl Window {
    /// fullscreen: janela sem borda cobrindo (x, y, w, h) — o monitor inteiro.
    /// Senão: janela normal com área útil w x h centralizada nesse retângulo.
    pub fn open(x: i32, y: i32, w: i32, h: i32, fullscreen: bool, title: &str) -> Result<Window, String> {
        let (tx, rx) = mpsc::channel::<Result<(isize, i32, i32, Arc<Mutex<Shared>>), String>>();
        let title = title.to_string();
        let thread = std::thread::spawn(move || unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            register_class();
            let class = wide(CLASS);
            let wtitle = wide(&title);
            let (style, ex) = if fullscreen {
                (WS_POPUP | WS_VISIBLE, WS_EX_APPWINDOW)
            } else {
                (WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_VISIBLE, WS_EX_APPWINDOW)
            };
            let (rx0, ry0, rw, rh) = (x, y, w, h);
            let hwnd = CreateWindowExW(ex, class.as_ptr(), wtitle.as_ptr(), style, rx0, ry0, rw, rh, null_mut(), null_mut(), GetModuleHandleW(null()), null());
            if hwnd.is_null() {
                let _ = tx.send(Err(format!("CreateWindowExW failed ({})", GetLastError())));
                return;
            }
            let mut cr: RECT = zeroed();
            GetClientRect(hwnd, &mut cr);
            let (cw, ch) = (cr.right - cr.left, cr.bottom - cr.top);
            let shared = Arc::new(Mutex::new(Shared {
                front: vec![0; (cw.max(1) * ch.max(1)) as usize],
                w: cw.max(1),
                h: ch.max(1),
                open: true,
                keys: VecDeque::new(),
                mouse: (0, 0, 0),
            }));
            let raw = Arc::into_raw(shared.clone());
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, raw as isize);
            SetForegroundWindow(hwnd);
            SetFocus(hwnd);
            let _ = tx.send(Ok((hwnd as isize, cw.max(1), ch.max(1), shared)));
            let mut msg: MSG = zeroed();
            while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            drop(Arc::from_raw(raw));
        });
        match rx.recv() {
            Ok(Ok((hwnd, cw, ch, shared))) => Ok(Window { hwnd, shared, w: cw, h: ch, thread: Some(thread) }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => Err("window thread died".into()),
        }
    }

    /// Ajusta (x, y, w, h) de área útil para o retângulo da janela com borda.
    pub fn outer_rect(x: i32, y: i32, w: i32, h: i32) -> (i32, i32, i32, i32) {
        let mut r = RECT { left: 0, top: 0, right: w, bottom: h };
        unsafe {
            AdjustWindowRectEx(&mut r, WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX, 0, WS_EX_APPWINDOW);
        }
        (x, y, r.right - r.left, r.bottom - r.top)
    }

    pub fn is_open(&self) -> bool {
        lock(&self.shared).open
    }

    pub fn present(&self, pixels: &[u32]) {
        {
            let mut s = lock(&self.shared);
            if !s.open {
                return;
            }
            if s.front.len() == pixels.len() {
                s.front.copy_from_slice(pixels);
            }
        }
        unsafe {
            InvalidateRect(self.hwnd as HWND, null(), 0);
        }
    }

    pub fn key(&self) -> i64 {
        lock(&self.shared).keys.pop_front().unwrap_or(0)
    }

    pub fn mouse(&self) -> (i32, i32, i32) {
        lock(&self.shared).mouse
    }

    pub fn close(&mut self) {
        unsafe {
            PostMessageW(self.hwnd as HWND, WM_CLOSE, 0, 0);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        self.close();
    }
}
