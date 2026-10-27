//! Icons through IShellItemImageFactory: one path for apps (shell:AppsFolder\...), exes and images.
//! Converted to premultiplied top-down 32-bit DIBs for AlphaBlend, cached by key.

use std::collections::HashMap;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Shell::*;
use windows::core::HSTRING;

pub struct Icons {
    size: i32,
    cache: HashMap<String, Option<HBITMAP>>,
}

impl Icons {
    pub fn new(size: i32) -> Self {
        Self { size, cache: HashMap::new() }
    }

    /// Drops everything (DPI change).
    pub fn reset(&mut self, size: i32) {
        for bmp in self.cache.drain().filter_map(|(_, b)| b) {
            unsafe {
                let _ = DeleteObject(bmp.into());
            }
        }
        self.size = size;
    }

    pub fn get(&mut self, key: &str) -> Option<HBITMAP> {
        if let Some(b) = self.cache.get(key) {
            return *b;
        }
        let bmp = load(key, self.size);
        self.cache.insert(key.to_string(), bmp);
        bmp
    }

    pub fn draw(&mut self, hdc: HDC, key: &str, x: i32, y: i32) -> bool {
        self.draw_sized(hdc, key, x, y, self.size)
    }

    /// Draws the cached icon scaled to `size` px.
    pub fn draw_sized(&mut self, hdc: HDC, key: &str, x: i32, y: i32, size: i32) -> bool {
        let Some(bmp) = self.get(key) else { return false };
        unsafe {
            let mem = CreateCompatibleDC(Some(hdc));
            let old = SelectObject(mem, bmp.into());
            let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
            let _ = AlphaBlend(hdc, x, y, size, size, mem, 0, 0, self.size, self.size, blend);
            SelectObject(mem, old);
            let _ = DeleteDC(mem);
        }
        true
    }
}

fn load(key: &str, size: i32) -> Option<HBITMAP> {
    let lower = key.to_lowercase();
    let is_image = [".png", ".jpg", ".jpeg", ".ico", ".bmp"].iter().any(|e| lower.ends_with(e));
    let flags = if is_image { SIIGBF_RESIZETOFIT } else { SIIGBF_ICONONLY };
    unsafe {
        let factory: IShellItemImageFactory = SHCreateItemFromParsingName(&HSTRING::from(key), None).ok()?;
        let src = factory.GetImage(SIZE { cx: size, cy: size }, flags).ok()?;
        let out = normalize(src, size);
        let _ = DeleteObject(src.into());
        out
    }
}

/// Copies `src` into a size x size top-down DIB, centered, with premultiplied alpha.
fn normalize(src: HBITMAP, size: i32) -> Option<HBITMAP> {
    unsafe {
        let mut bm = BITMAP::default();
        if GetObjectW(src.into(), size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as _)) == 0 {
            return None;
        }
        let (w, h) = (bm.bmWidth, bm.bmHeight.abs());
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut px = vec![0u8; (w * h * 4) as usize];
        let screen = GetDC(None);
        let lines = GetDIBits(screen, src, 0, h as u32, Some(px.as_mut_ptr() as _), &mut info, DIB_RGB_COLORS);
        ReleaseDC(None, screen);
        if lines == 0 {
            return None;
        }
        let has_alpha = px.chunks(4).any(|p| p[3] != 0);
        // Icons usually come premultiplied, thumbnails not; a channel above alpha means straight alpha.
        let straight = has_alpha && px.chunks(4).any(|p| p[0] > p[3] || p[1] > p[3] || p[2] > p[3]);
        for p in px.chunks_mut(4) {
            if !has_alpha {
                p[3] = 255;
            } else if straight {
                let a = p[3] as u32;
                for c in &mut p[..3] {
                    *c = (*c as u32 * a / 255) as u8;
                }
            }
        }

        info.bmiHeader.biWidth = size;
        info.bmiHeader.biHeight = -size;
        let mut bits = std::ptr::null_mut();
        let dib = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        let dst = std::slice::from_raw_parts_mut(bits as *mut u8, (size * size * 4) as usize);
        dst.fill(0);
        let (ox, oy) = ((size - w.min(size)) / 2, (size - h.min(size)) / 2);
        for y in 0..h.min(size) {
            for x in 0..w.min(size) {
                let s = ((y * w + x) * 4) as usize;
                let d = (((y + oy) * size + x + ox) * 4) as usize;
                dst[d..d + 4].copy_from_slice(&px[s..s + 4]);
            }
        }
        Some(dib)
    }
}
