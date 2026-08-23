//! Red frame around a recorded region — the Windows counterpart of the macOS
//! RecordingFrameWindow. A click-through, non-activating, topmost window whose
//! window REGION is just the 3 px frame (the interior is not part of the
//! window at all), excluded from capture via WDA_EXCLUDEFROMCAPTURE so it
//! never appears in the recording.

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::mpsc;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CombineRgn, CreateRectRgn, CreateSolidBrush, SetWindowRgn, RGN_DIFF,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, PostMessageW,
    RegisterClassW, SetLayeredWindowAttributes, SetWindowDisplayAffinity, ShowWindow,
    TranslateMessage, LWA_ALPHA,
    CS_HREDRAW, CS_VREDRAW, MSG, SW_SHOWNOACTIVATE, WDA_EXCLUDEFROMCAPTURE, WM_CLOSE, WM_DESTROY,
    WNDCLASSW, WS_DISABLED, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP,
};

const FRAME: i32 = 3;
/// Same red as the macOS frame (BGR order for COLORREF): #FF3B30.
const FRAME_COLOR: u32 = 0x0030_3BFF;

static BORDER_HWND: AtomicIsize = AtomicIsize::new(0);

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_DESTROY => {
            unsafe { windows::Win32::UI::WindowsAndMessaging::PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, w, l) },
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Shows the frame around `rect` (global virtual-desktop px) on its own thread
/// until [`hide`] is called. Failures are silently ignored — the frame is a
/// courtesy, never worth failing a recording over.
pub fn show(x: i32, y: i32, w: i32, h: i32) {
    let (tx, rx) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        unsafe {
            let class_name = wide("TeilRecordingFrame");
            let instance = GetModuleHandleW(None).unwrap_or_default();
            let class = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(wndproc),
                hInstance: instance.into(),
                lpszClassName: PCWSTR(class_name.as_ptr()),
                hbrBackground: CreateSolidBrush(COLORREF(FRAME_COLOR)),
                ..Default::default()
            };
            let _ = RegisterClassW(&class); // idempotent — re-registration fails harmlessly

            let (ox, oy, ow, oh) = (x - FRAME, y - FRAME, w + 2 * FRAME, h + 2 * FRAME);
            let Ok(hwnd) = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_LAYERED,
                PCWSTR(class_name.as_ptr()),
                PCWSTR(wide("recording frame").as_ptr()),
                WS_POPUP | WS_DISABLED,
                ox, oy, ow, oh,
                None, None, Some(instance.into()), None,
            ) else {
                let _ = tx.send(());
                return;
            };

            // A WS_EX_LAYERED window renders NOTHING until its layering
            // attributes are set — without this call the frame is invisible.
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);

            // Window region = outer rect minus interior → only the frame exists.
            let outer = CreateRectRgn(0, 0, ow, oh);
            let inner = CreateRectRgn(FRAME, FRAME, ow - FRAME, oh - FRAME);
            let frame_rgn = CreateRectRgn(0, 0, 0, 0);
            let _ = CombineRgn(Some(frame_rgn), Some(outer), Some(inner), RGN_DIFF);
            let _ = SetWindowRgn(hwnd, Some(frame_rgn), true);

            // Never in the recording (Win10 2004+). If unsupported, the OS
            // border already marks the capture — cosmetic either way.
            let _ = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);

            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            BORDER_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
            let _ = tx.send(());

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            BORDER_HWND.store(0, Ordering::SeqCst);
            let _ = DestroyWindow(hwnd);
        }
    });
    // Wait for the window to exist so a fast stop can still close it.
    let _ = rx.recv_timeout(std::time::Duration::from_secs(2));
}

/// Closes the frame window if one is up. Safe to call repeatedly.
pub fn hide() {
    let hwnd = BORDER_HWND.swap(0, Ordering::SeqCst);
    if hwnd != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}
