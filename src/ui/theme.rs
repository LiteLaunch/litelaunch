//! Colors (light, dark, AMOLED, Windows accent) and fonts.

use super::*;

/// Colors as COLORREF (0x00BBGGRR).
#[derive(Clone, Copy)]
pub(super) struct Theme {
    pub(super) bg: u32,
    pub(super) sel: u32,
    pub(super) text: u32,
    pub(super) muted: u32,
    pub(super) accent: u32,
    pub(super) line: u32,
}

pub(super) const fn rgb(c: u32) -> u32 {
    ((c & 0xFF) << 16) | (c & 0xFF00) | ((c >> 16) & 0xFF)
}

pub(super) const LIGHT: Theme = Theme {
    bg: rgb(0xF9F9F9),
    sel: rgb(0xE9E9E9),
    text: rgb(0x1B1B1B),
    muted: rgb(0x6E6E6E),
    accent: rgb(0x005FB8),
    line: rgb(0xE0E0E0),
};

pub(super) const DARK: Theme = Theme {
    bg: rgb(0x202020),
    sel: rgb(0x2F2F2F),
    text: rgb(0xF0F0F0),
    muted: rgb(0x9A9A9A),
    accent: rgb(0x60CDFF),
    line: rgb(0x333333),
};

pub(super) const AMOLED: Theme = Theme {
    bg: rgb(0x000000),
    sel: rgb(0x161616),
    text: rgb(0xEDEDED),
    muted: rgb(0x8C8C8C),
    accent: rgb(0x60CDFF),
    line: rgb(0x1C1C1C),
};

/// Light or dark base tinted with the Windows accent color: the selection bar gets the accent
/// shade Windows itself uses on that background, the selected row a faint wash of it.
pub(super) fn windows_theme(dark: bool) -> Theme {
    let base = if dark { DARK } else { LIGHT };
    let Some(palette) = accent_palette() else { return base };
    // AccentPalette: Light3, Light2, Light1, Accent, Dark1, Dark2, Dark3 (+1 unused).
    let accent = if dark { palette[1] } else { palette[4] };
    Theme { accent, sel: mix(palette[3], base.bg, if dark { 0.22 } else { 0.16 }), ..base }
}

/// The system accent palette as COLORREFs, from Explorer's registry key.
pub(super) fn accent_palette() -> Option<[u32; 8]> {
    let mut raw = [0u8; 32];
    let mut size = raw.len() as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Accent"),
            w!("AccentPalette"),
            RRF_RT_REG_BINARY,
            None,
            Some(raw.as_mut_ptr() as _),
            Some(&mut size),
        )
    };
    if r.is_err() || size != 32 {
        return None;
    }
    // Stored as R, G, B, A — which is COLORREF byte order once alpha is dropped.
    Some(std::array::from_fn(|i| u32::from_le_bytes([raw[i * 4], raw[i * 4 + 1], raw[i * 4 + 2], 0])))
}

/// `a` over `b` with weight `t` (COLORREFs).
pub(super) fn mix(a: u32, b: u32, t: f32) -> u32 {
    let ch = |s: u32| {
        let (x, y) = (((a >> s) & 0xFF) as f32, ((b >> s) & 0xFF) as f32);
        ((x * t + y * (1.0 - t)).round() as u32) << s
    };
    ch(0) | ch(8) | ch(16)
}

pub(super) struct Fonts {
    pub(super) input: HFONT,
    pub(super) title: HFONT,
    pub(super) sub: HFONT,
    /// For a text `search_icon` (symbols and emoji).
    pub(super) glyph: HFONT,
    /// Segoe Fluent Icons, for the pin mark.
    pub(super) icons: HFONT,
    /// Segoe Fluent Icons at icon size, for the actions menu rows.
    pub(super) actions: HFONT,
}

impl Fonts {
    pub(super) fn new(scale: f32, input: f32) -> Self {
        let make = |px: f32, weight: i32, face: PCWSTR| unsafe {
            CreateFontW(
                -(px * scale).round() as i32,
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                0,
                face,
            )
        };
        let ui = w!("Segoe UI");
        Self {
            input: make(input, 400, ui),
            title: make(FONT_TITLE, 400, ui),
            sub: make(FONT_SUB, 400, ui),
            glyph: make(input, 400, w!("Segoe UI Emoji")),
            icons: make(FONT_SUB, 400, w!("Segoe Fluent Icons")),
            actions: make(20.0, 400, w!("Segoe Fluent Icons")),
        }
    }

    pub(super) fn delete(&self) {
        unsafe {
            for f in [self.input, self.title, self.sub, self.glyph, self.icons, self.actions] {
                let _ = DeleteObject(f.into());
            }
        }
    }
}

/// Mid-tone between two COLORREFs, for hover.
pub(super) fn blend(a: u32, b: u32) -> u32 {
    let ch = |s: u32| (((a >> s) & 0xFF) + ((b >> s) & 0xFF)) / 2;
    ch(0) | (ch(8) << 8) | (ch(16) << 16)
}

pub(super) fn apps_use_light_theme() -> bool {
    let mut v = 1u32;
    let mut size = 4u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut _ as _),
            Some(&mut size),
        )
    };
    r.is_err() || v != 0
}

impl State {
    pub(super) fn apply_theme(&mut self) {
        let system_dark = !apps_use_light_theme();
        let (theme, dark) = match self.engine.cfg.theme.as_str() {
            "light" => (LIGHT, false),
            "dark" => (DARK, true),
            "amoled" => (AMOLED, true),
            "windows" => (windows_theme(system_dark), system_dark),
            _ => (if system_dark { DARK } else { LIGHT }, system_dark),
        };
        self.theme = theme;
        unsafe {
            let _ = DeleteObject(self.bg_brush.into());
            self.bg_brush = CreateSolidBrush(COLORREF(self.theme.bg));
            EDIT_TEXT.store(self.theme.text, Ordering::Relaxed);
            EDIT_BG.store(self.theme.bg, Ordering::Relaxed);
            EDIT_BRUSH.store(self.bg_brush.0 as isize, Ordering::Relaxed);
            let on: BOOL = dark.into();
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &on as *const _ as _, size_of::<BOOL>() as u32);
            // The 1px window border in the theme's line color (a grey frame breaks AMOLED black).
            let border = self.theme.line;
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWA_BORDER_COLOR, &border as *const _ as _, 4);
        }
    }

    pub(super) fn apply_dpi(&mut self) {
        let dpi = unsafe { GetDpiForWindow(self.hwnd) };
        self.scale = dpi as f32 / 96.0;
        self.fonts.delete();
        self.fonts = Fonts::new(self.scale, self.engine.cfg.input_font_size);
        self.icons.reset(self.px(ICON));
    }
}
