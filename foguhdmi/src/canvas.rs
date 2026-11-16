//! Canvas: uma DIB section 32 bpp num DC de memória. As primitivas de
//! `pix` escrevem direto nos pixels; o texto usa o GDI (fontes do sistema).

use std::mem::{size_of, zeroed};
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::RECT;
use windows_sys::Win32::Graphics::Gdi::*;

use crate::pix::{Paint, Pix};

pub struct Canvas {
    pub w: i32,
    pub h: i32,
    dc: HDC,
    bmp: HBITMAP,
    old_bmp: HGDIOBJ,
    px: *mut u32,
    face: String,
    bold: bool,
    font: HFONT,
    font_key: (String, bool, i32),
    old_font: HGDIOBJ,
}

// O canvas só é usado com o Mutex da superfície travado.
unsafe impl Send for Canvas {}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

impl Canvas {
    pub fn new(w: i32, h: i32) -> Result<Canvas, String> {
        if w <= 0 || h <= 0 || w > 16384 || h > 16384 {
            return Err(format!("invalid canvas size {w}x{h}"));
        }
        unsafe {
            let dc = CreateCompatibleDC(null_mut());
            if dc.is_null() {
                return Err("CreateCompatibleDC failed".into());
            }
            let mut bi: BITMAPINFO = zeroed();
            bi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
            bi.bmiHeader.biWidth = w;
            bi.bmiHeader.biHeight = -h; // de cima para baixo
            bi.bmiHeader.biPlanes = 1;
            bi.bmiHeader.biBitCount = 32;
            bi.bmiHeader.biCompression = BI_RGB;
            let mut bits = null_mut();
            let bmp = CreateDIBSection(dc, &bi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            if bmp.is_null() || bits.is_null() {
                DeleteDC(dc);
                return Err(format!("CreateDIBSection {w}x{h} failed"));
            }
            let old_bmp = SelectObject(dc, bmp);
            SetBkMode(dc, TRANSPARENT as _);
            let px = bits as *mut u32;
            std::ptr::write_bytes(px, 0, (w * h) as usize);
            Ok(Canvas {
                w,
                h,
                dc,
                bmp,
                old_bmp,
                px,
                face: "Segoe UI".into(),
                bold: false,
                font: null_mut(),
                font_key: (String::new(), false, 0),
                old_font: null_mut(),
            })
        }
    }

    pub fn pixels(&mut self) -> &mut [u32] {
        unsafe {
            GdiFlush();
            std::slice::from_raw_parts_mut(self.px, (self.w * self.h) as usize)
        }
    }

    pub fn pix(&mut self) -> Pix<'_> {
        let (w, h) = (self.w, self.h);
        Pix::new(self.pixels(), w, h)
    }

    pub fn set_font(&mut self, face: &str, bold: bool) {
        if !face.is_empty() {
            self.face = face.to_string();
        }
        self.bold = bold;
    }

    fn select_font(&mut self, size: i32) {
        let key = (self.face.clone(), self.bold, size);
        if !self.font.is_null() && self.font_key == key {
            return;
        }
        unsafe {
            let face = wide(&self.face);
            let font = CreateFontW(
                -size.max(1),
                0,
                0,
                0,
                if self.bold { FW_BOLD as i32 } else { FW_NORMAL as i32 },
                0,
                0,
                0,
                DEFAULT_CHARSET as _,
                OUT_DEFAULT_PRECIS as _,
                CLIP_DEFAULT_PRECIS as _,
                ANTIALIASED_QUALITY as _,
                (DEFAULT_PITCH as u32 | FF_DONTCARE as u32) as _,
                face.as_ptr(),
            );
            let prev = SelectObject(self.dc, font);
            if self.old_font.is_null() {
                self.old_font = prev;
            }
            if !self.font.is_null() {
                DeleteObject(self.font);
            }
            self.font = font;
            self.font_key = key;
        }
    }

    /// Texto (aceita \n) com o canto superior esquerdo em (x, y).
    pub fn text(&mut self, x: i32, y: i32, s: &str, paint: Paint, size: i32) {
        self.select_font(size);
        let c = paint.rgb;
        let colorref = ((c >> 16) & 0xFF) | (c & 0xFF00) | ((c & 0xFF) << 16);
        let mut ws: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            SetTextColor(self.dc, colorref);
            let mut r = RECT { left: x, top: y, right: x + 100_000, bottom: y + 100_000 };
            DrawTextW(self.dc, ws.as_mut_ptr(), ws.len() as i32, &mut r, DT_NOCLIP | DT_NOPREFIX | DT_LEFT | DT_TOP);
            GdiFlush();
        }
    }

    /// Tamanho (largura, altura) do texto em pixels.
    pub fn text_size(&mut self, s: &str, size: i32) -> (i32, i32) {
        self.select_font(size);
        let mut ws: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
            DrawTextW(self.dc, ws.as_mut_ptr(), ws.len() as i32, &mut r, DT_CALCRECT | DT_NOPREFIX | DT_LEFT | DT_TOP);
            (r.right - r.left, r.bottom - r.top)
        }
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe {
            if !self.old_font.is_null() {
                SelectObject(self.dc, self.old_font);
            }
            if !self.font.is_null() {
                DeleteObject(self.font);
            }
            SelectObject(self.dc, self.old_bmp);
            DeleteObject(self.bmp);
            DeleteDC(self.dc);
        }
    }
}
