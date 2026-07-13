//! Raw Win32 selection overlay: one WS_POPUP topmost window spanning the entire
//! virtual screen, painting a pre-frozen desktop snapshot dimmed, with the live
//! selection (drag rectangle or hovered window) shown undimmed and outlined.
//! GDI double-buffered; runs its own GetMessage loop on the CALLING thread, so the
//! caller must not be the Tauri main thread (spawn_blocking).
//!
//! All coordinates are physical pixels in virtual-screen space (the process is
//! per-monitor-DPI-aware via Tauri's embedded manifest, so GetSystemMetrics, mouse
//! messages, and GDI all agree).

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};

use image::RgbaImage;
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateDIBSection,
    CreateSolidBrush, DeleteDC, DeleteObject, EndPaint, FrameRect, GdiFlush, GetDC,
    InvalidateRect, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT,
    DIB_RGB_COLORS, HBITMAP, HBRUSH, HDC, PAINTSTRUCT, SRCCOPY,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, SetFocus, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetSystemMetrics, LoadCursorW, PostQuitMessage, RegisterClassW,
    SetForegroundWindow, ShowWindow, TranslateMessage, IDC_CROSS, MSG, SM_CXVIRTUALSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_SHOW, WM_DESTROY, WM_ERASEBKGND,
    WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_RBUTTONUP, WNDCLASSW,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

/// Plain global-space rectangle (avoids leaking windows-rs types to lib.rs).
#[derive(Clone, Copy)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    Region,
    WindowPick,
}

pub enum Outcome {
    Cancelled,
    /// Global-space region (x, y, w, h).
    Region { x: i32, y: i32, w: i32, h: i32 },
    /// Index into the pick-rect list passed to `run`.
    Window(usize),
}

/// Frozen BGRA snapshot of the whole virtual screen (top-down rows).
pub struct Frozen {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    bgra: Vec<u8>,
}

impl Frozen {
    /// Crop a global-space rect out of the snapshot as RGBA (clamped to bounds).
    pub fn crop_global(&self, gx: i32, gy: i32, gw: i32, gh: i32) -> Result<RgbaImage, String> {
        let x0 = (gx - self.x).clamp(0, self.w);
        let y0 = (gy - self.y).clamp(0, self.h);
        let x1 = (gx - self.x + gw).clamp(0, self.w);
        let y1 = (gy - self.y + gh).clamp(0, self.h);
        let (cw, ch) = (x1 - x0, y1 - y0);
        if cw < 1 || ch < 1 {
            return Err("Selection is empty.".into());
        }
        let mut out = Vec::with_capacity((cw as usize) * (ch as usize) * 4);
        for row in y0..y1 {
            let start = ((row as usize) * (self.w as usize) + x0 as usize) * 4;
            let end = start + (cw as usize) * 4;
            for px in self.bgra[start..end].chunks_exact(4) {
                out.extend_from_slice(&[px[2], px[1], px[0], 255]);
            }
        }
        RgbaImage::from_raw(cw as u32, ch as u32, out)
            .ok_or_else(|| "Snapshot crop failed.".into())
    }
}

/// BitBlt the entire virtual screen (all monitors, physical pixels) into memory.
/// CAPTUREBLT includes layered windows — this is the pre-overlay "what you see" image.
pub fn freeze_screen() -> Result<Frozen, String> {
    unsafe {
        let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let w = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let h = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        if w <= 0 || h <= 0 {
            return Err("No displays found.".into());
        }

        let screen_dc = GetDC(None);
        if screen_dc.is_invalid() {
            return Err("GetDC failed.".into());
        }
        let mem_dc = CreateCompatibleDC(Some(screen_dc));

        let mut bits: *mut c_void = std::ptr::null_mut();
        let bi = bitmap_info(w, h);
        let result = CreateDIBSection(Some(mem_dc), &bi, DIB_RGB_COLORS, &mut bits, None, 0)
            .map_err(|e| format!("CreateDIBSection failed: {e}"))
            .and_then(|bmp| {
                let old = SelectObject(mem_dc, bmp.into());
                let blt = BitBlt(
                    mem_dc,
                    0,
                    0,
                    w,
                    h,
                    Some(screen_dc),
                    x,
                    y,
                    SRCCOPY | CAPTUREBLT,
                )
                .map_err(|e| format!("BitBlt failed: {e}"));
                let _ = GdiFlush();
                let pixels = blt.map(|_| {
                    std::slice::from_raw_parts(bits as *const u8, (w as usize) * (h as usize) * 4)
                        .to_vec()
                });
                SelectObject(mem_dc, old);
                let _ = DeleteObject(bmp.into());
                pixels
            });

        let _ = DeleteDC(mem_dc);
        ReleaseDC(None, screen_dc);

        Ok(Frozen { x, y, w, h, bgra: result? })
    }
}

// ---- Overlay window --------------------------------------------------------

/// Per-overlay state, reachable from the wndproc. One overlay per thread; entry is
/// additionally serialized process-wide by IN_PROGRESS.
struct State {
    origin: (i32, i32),
    size: (i32, i32),
    mode: Mode,
    /// Global-space hover-pick candidates, front-to-back.
    pick_rects: Vec<Rect>,
    /// Region-drag anchor (client coords) while the button is down.
    drag_from: Option<(i32, i32)>,
    /// Current selection to draw, client coords. None = nothing highlighted.
    sel: Option<RECT>,
    hover: Option<usize>,
    outcome: Option<Outcome>,
    // GDI resources (owned; released in `run` after the message loop ends).
    orig_dc: HDC,
    orig_bmp: HBITMAP,
    dim_dc: HDC,
    dim_bmp: HBITMAP,
    back_dc: HDC,
    back_bmp: HBITMAP,
    border: HBRUSH,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

static IN_PROGRESS: AtomicBool = AtomicBool::new(false);

/// Show the overlay over `frozen` and block until the user selects or cancels.
/// `pick_rects` is only used in WindowPick mode (global coords, front-to-back).
pub fn run(frozen: &Frozen, mode: Mode, pick_rects: &[Rect]) -> Result<Outcome, String> {
    if IN_PROGRESS.swap(true, Ordering::SeqCst) {
        return Err("A capture is already in progress.".into());
    }
    let result = run_inner(frozen, mode, pick_rects);
    IN_PROGRESS.store(false, Ordering::SeqCst);
    result
}

fn run_inner(frozen: &Frozen, mode: Mode, pick_rects: &[Rect]) -> Result<Outcome, String> {
    unsafe {
        let instance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let class = w!("TeilCaptureOverlay");

        // Idempotent: fails with ERROR_CLASS_ALREADY_EXISTS after the first capture — fine.
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_CROSS).unwrap_or_default(),
            lpszClassName: class,
            ..Default::default()
        };
        let _ = RegisterClassW(&wc);

        // Build the paint sources: the frozen snapshot verbatim + a dimmed copy.
        let screen_dc = GetDC(None);
        let (orig_dc, orig_bmp) = dib_dc(screen_dc, frozen.w, frozen.h, &frozen.bgra)?;
        let dimmed: Vec<u8> = frozen
            .bgra
            .iter()
            .enumerate()
            .map(|(i, &v)| if i % 4 == 3 { v } else { (v as u32 * 115 / 255) as u8 })
            .collect();
        let (dim_dc, dim_bmp) = dib_dc(screen_dc, frozen.w, frozen.h, &dimmed)?;
        // Back buffer for flicker-free WM_PAINT composition.
        let back_dc = CreateCompatibleDC(Some(screen_dc));
        let back_bmp = CreateCompatibleBitmap(screen_dc, frozen.w, frozen.h);
        SelectObject(back_dc, back_bmp.into());
        ReleaseDC(None, screen_dc);

        let border_color = match mode {
            Mode::Region => COLORREF(0x00FFFFFF),    // white
            Mode::WindowPick => COLORREF(0x00D77800), // Windows accent blue (BGR)
        };
        STATE.with(|s| {
            *s.borrow_mut() = Some(State {
                origin: (frozen.x, frozen.y),
                size: (frozen.w, frozen.h),
                mode,
                pick_rects: pick_rects.to_vec(),
                drag_from: None,
                sel: None,
                hover: None,
                outcome: None,
                orig_dc,
                orig_bmp,
                dim_dc,
                dim_bmp,
                back_dc,
                back_bmp,
                border: CreateSolidBrush(border_color),
            });
        });

        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            class,
            w!("capture"),
            WS_POPUP,
            frozen.x,
            frozen.y,
            frozen.w,
            frozen.h,
            None,
            None,
            Some(instance.into()),
            None,
        );

        let outcome = match hwnd {
            Ok(hwnd) => {
                let _ = ShowWindow(hwnd, SW_SHOW);
                let _ = SetForegroundWindow(hwnd);
                let _ = SetFocus(Some(hwnd));

                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                Ok(())
            }
            Err(e) => Err(format!("Overlay window creation failed: {e}")),
        };

        // Tear down state + GDI resources regardless of how the loop ended.
        let state = STATE.with(|s| s.borrow_mut().take());
        if let Some(st) = state {
            let _ = DeleteDC(st.orig_dc);
            let _ = DeleteObject(st.orig_bmp.into());
            let _ = DeleteDC(st.dim_dc);
            let _ = DeleteObject(st.dim_bmp.into());
            let _ = DeleteDC(st.back_dc);
            let _ = DeleteObject(st.back_bmp.into());
            let _ = DeleteObject(st.border.into());
            outcome.map(|_| st.outcome.unwrap_or(Outcome::Cancelled))
        } else {
            outcome.map(|_| Outcome::Cancelled)
        }
    }
}

/// Memory DC + top-down 32bpp DIB pre-filled with `bgra`.
unsafe fn dib_dc(screen_dc: HDC, w: i32, h: i32, bgra: &[u8]) -> Result<(HDC, HBITMAP), String> {
    let dc = CreateCompatibleDC(Some(screen_dc));
    let mut bits: *mut c_void = std::ptr::null_mut();
    let bi = bitmap_info(w, h);
    let bmp = CreateDIBSection(Some(dc), &bi, DIB_RGB_COLORS, &mut bits, None, 0)
        .map_err(|e| format!("CreateDIBSection failed: {e}"))?;
    SelectObject(dc, bmp.into());
    std::ptr::copy_nonoverlapping(bgra.as_ptr(), bits as *mut u8, bgra.len());
    Ok((dc, bmp))
}

fn bitmap_info(w: i32, h: i32) -> BITMAPINFO {
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn loword_x(lparam: LPARAM) -> i32 {
    (lparam.0 & 0xffff) as u16 as i16 as i32
}

fn hiword_y(lparam: LPARAM) -> i32 {
    ((lparam.0 >> 16) & 0xffff) as u16 as i16 as i32
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => LRESULT(1), // everything is painted in WM_PAINT
        WM_PAINT => {
            STATE.with(|s| {
                if let Some(st) = s.borrow().as_ref() {
                    paint(hwnd, st);
                }
            });
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (cx, cy) = (loword_x(lparam), hiword_y(lparam));
            STATE.with(|s| {
                if let Some(st) = s.borrow_mut().as_mut() {
                    update_selection(st, cx, cy);
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (cx, cy) = (loword_x(lparam), hiword_y(lparam));
            STATE.with(|s| {
                if let Some(st) = s.borrow_mut().as_mut() {
                    if st.mode == Mode::Region {
                        st.drag_from = Some((cx, cy));
                        SetCapture(hwnd);
                    }
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (cx, cy) = (loword_x(lparam), hiword_y(lparam));
            let done = STATE.with(|s| {
                let mut b = s.borrow_mut();
                let Some(st) = b.as_mut() else { return false };
                match st.mode {
                    Mode::Region => {
                        let _ = ReleaseCapture();
                        let Some((ax, ay)) = st.drag_from.take() else { return false };
                        let (l, r) = (ax.min(cx), ax.max(cx) + 1);
                        let (t, b_) = (ay.min(cy), ay.max(cy) + 1);
                        // A click without a real drag cancels instead of capturing 1px.
                        st.outcome = Some(if r - l >= 3 && b_ - t >= 3 {
                            Outcome::Region {
                                x: st.origin.0 + l,
                                y: st.origin.1 + t,
                                w: r - l,
                                h: b_ - t,
                            }
                        } else {
                            Outcome::Cancelled
                        });
                        true
                    }
                    Mode::WindowPick => {
                        update_selection(st, cx, cy);
                        match st.hover {
                            Some(idx) => {
                                st.outcome = Some(Outcome::Window(idx));
                                true
                            }
                            None => false,
                        }
                    }
                }
            });
            if done {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            cancel(hwnd);
            LRESULT(0)
        }
        WM_KEYDOWN if wparam.0 as u16 == VK_ESCAPE.0 => {
            cancel(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn cancel(hwnd: HWND) {
    STATE.with(|s| {
        if let Some(st) = s.borrow_mut().as_mut() {
            st.outcome = Some(Outcome::Cancelled);
        }
    });
    let _ = DestroyWindow(hwnd);
}

/// Recompute the highlighted rect (client coords) for the cursor position.
fn update_selection(st: &mut State, cx: i32, cy: i32) {
    match st.mode {
        Mode::Region => {
            st.sel = st.drag_from.map(|(ax, ay)| RECT {
                left: ax.min(cx),
                top: ay.min(cy),
                right: ax.max(cx) + 1,
                bottom: ay.max(cy) + 1,
            });
        }
        Mode::WindowPick => {
            let (gx, gy) = (st.origin.0 + cx, st.origin.1 + cy);
            st.hover = st
                .pick_rects
                .iter()
                .position(|r| gx >= r.left && gx < r.right && gy >= r.top && gy < r.bottom);
            st.sel = st.hover.map(|i| {
                let r = st.pick_rects[i];
                RECT {
                    left: (r.left - st.origin.0).clamp(0, st.size.0),
                    top: (r.top - st.origin.1).clamp(0, st.size.1),
                    right: (r.right - st.origin.0).clamp(0, st.size.0),
                    bottom: (r.bottom - st.origin.1).clamp(0, st.size.1),
                }
            });
        }
    }
}

/// Compose dim background + undimmed selection + border into the back buffer,
/// then blit once to the window.
unsafe fn paint(hwnd: HWND, st: &State) {
    let (w, h) = st.size;
    let _ = BitBlt(st.back_dc, 0, 0, w, h, Some(st.dim_dc), 0, 0, SRCCOPY);
    if let Some(sel) = st.sel {
        let (sw, sh) = (sel.right - sel.left, sel.bottom - sel.top);
        if sw > 0 && sh > 0 {
            let _ = BitBlt(
                st.back_dc,
                sel.left,
                sel.top,
                sw,
                sh,
                Some(st.orig_dc),
                sel.left,
                sel.top,
                SRCCOPY,
            );
            // 2px border: FrameRect twice (it draws 1px inside the rect).
            let _ = FrameRect(st.back_dc, &sel, st.border);
            let inner = RECT {
                left: sel.left + 1,
                top: sel.top + 1,
                right: sel.right - 1,
                bottom: sel.bottom - 1,
            };
            if inner.right > inner.left && inner.bottom > inner.top {
                let _ = FrameRect(st.back_dc, &inner, st.border);
            }
        }
    }

    let mut ps = PAINTSTRUCT::default();
    let dc = BeginPaint(hwnd, &mut ps);
    let _ = BitBlt(dc, 0, 0, w, h, Some(st.back_dc), 0, 0, SRCCOPY);
    let _ = EndPaint(hwnd, &ps);
}
