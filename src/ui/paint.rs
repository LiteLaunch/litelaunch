//! Layout and GDI painting of the query band and the result rows.

use super::*;

// Layout in DIPs.
pub(super) const PAD: f32 = 8.0;

/// Query band proportions measured on Flow Launcher (16 pt query, 71 px band at 125%):
/// the band from the window top to the separator is 3.55 em tall, and the text baseline
/// sits at 62% of it — caps 30 px from the top, baseline 27 px above the separator.
pub(super) const BAND_EM: f32 = 3.55;

pub(super) const BASELINE_AT: f32 = 0.62;

pub(super) const ROW_H: f32 = 52.0;

/// Rows without a subtitle (apps, games) are shorter.
pub(super) const ROW_H_SHORT: f32 = 42.0;

pub(super) const ICON: f32 = 32.0;

pub(super) const FONT_TITLE: f32 = 15.0;

pub(super) const FONT_SUB: f32 = 12.0;

/// Left edge of the icon column and of the text column (query and result titles), from the window edge.
pub(super) const ICON_X: f32 = PAD + 14.0;

pub(super) const TEXT_X: f32 = ICON_X + ICON + 14.0;

impl State {
    pub(super) fn px(&self, dip: f32) -> i32 {
        (dip * self.scale).round() as i32
    }

    /// Results reachable with Alt+1..N: the first `max_results` of the list (at most 9),
    /// wherever the list is scrolled.
    pub(super) fn shortcut_rows(&self) -> usize {
        self.items.len().min(self.engine.cfg.max_results).min(9)
    }

    pub(super) fn visible_rows(&self) -> usize {
        self.items.len().min(self.engine.cfg.max_results)
    }

    pub(super) fn height(&self) -> i32 {
        let rows = self.visible_rows();
        if rows == 0 {
            self.query_band().1
        } else {
            self.row_top(rows) + self.px(PAD)
        }
    }

    /// Height of result `i`: shorter when it has no subtitle.
    pub(super) fn row_h(&self, i: usize) -> i32 {
        let short = self.items.get(i).is_some_and(|it| it.sub.is_empty());
        self.px(if short { ROW_H_SHORT } else { ROW_H })
    }

    /// Top of the `visible_index`-th visible row (rows can differ in height).
    pub(super) fn row_top(&self, visible_index: usize) -> i32 {
        let first = self.px(PAD + self.input_h() + 1.0 + PAD);
        first + (0..visible_index).map(|k| self.row_h(self.top + k)).sum::<i32>()
    }

    pub(super) fn row_at(&self, y: i32) -> Option<usize> {
        (0..self.visible_rows())
            .find(|&vi| {
                let top = self.row_top(vi);
                y >= top && y < top + self.row_h(self.top + vi)
            })
            .map(|vi| self.top + vi)
    }

    pub(super) fn paint(&mut self, hdc: HDC) {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
            let mem = CreateCompatibleDC(Some(hdc));
            let bmp = CreateCompatibleBitmap(hdc, rc.right, rc.bottom);
            let old_bmp = SelectObject(mem, bmp.into());
            FillRect(mem, &rc, self.bg_brush);
            SetBkMode(mem, TRANSPARENT);

            self.paint_search_icon(mem);

            if !self.items.is_empty() {
                let y = self.px(PAD + self.input_h());
                let line = CreateSolidBrush(COLORREF(self.theme.line));
                FillRect(mem, &RECT { left: 0, top: y, right: rc.right, bottom: y + self.px(1.0).max(1) }, line);
                let _ = DeleteObject(line.into());
            }

            let icon = self.px(ICON);
            for vi in 0..self.visible_rows() {
                let i = self.top + vi;
                let row_h = self.row_h(i);
                let Some(item) = self.items.get(i) else { break };
                let top = self.row_top(vi);
                let left = self.px(PAD);
                let right = rc.right - self.px(PAD);
                if i == self.sel || Some(i) == self.hover {
                    let color = if i == self.sel { self.theme.sel } else { blend(self.theme.sel, self.theme.bg) };
                    let brush = CreateSolidBrush(COLORREF(color));
                    let old_b = SelectObject(mem, brush.into());
                    let old_p = SelectObject(mem, GetStockObject(NULL_PEN));
                    let r = self.px(8.0);
                    let _ = RoundRect(mem, left, top, right + 1, top + row_h + 1, r, r);
                    SelectObject(mem, old_p);
                    SelectObject(mem, old_b);
                    let _ = DeleteObject(brush.into());
                    if i == self.sel {
                        let accent = CreateSolidBrush(COLORREF(self.theme.accent));
                        let bar = self.px(3.0);
                        let inset = self.px(16.0);
                        let ab = SelectObject(mem, accent.into());
                        let ap = SelectObject(mem, GetStockObject(NULL_PEN));
                        let _ = RoundRect(mem, left, top + inset, left + bar + 1, top + row_h - inset + 1, bar, bar);
                        SelectObject(mem, ap);
                        SelectObject(mem, ab);
                        let _ = DeleteObject(accent.into());
                    }
                }

                let ix = self.px(ICON_X);
                let iy = top + (row_h - icon) / 2;
                if let Some(g) = item.glyph {
                    let mut text: Vec<u16> = g.to_string().encode_utf16().collect();
                    let mut gr = RECT { left: ix, top: iy, right: ix + icon, bottom: iy + icon };
                    SelectObject(mem, self.fonts.actions.into());
                    SetTextColor(mem, COLORREF(self.theme.text));
                    DrawTextW(mem, &mut text, &mut gr, DT_SINGLELINE | DT_CENTER | DT_VCENTER | DT_NOPREFIX);
                }
                let drawn = item.glyph.is_some() || item.icon.as_deref().is_some_and(|k| self.icons.draw(mem, k, ix, iy));
                if !drawn {
                    self.icons.draw(mem, &std::env::current_exe().unwrap_or_default().to_string_lossy(), ix, iy);
                }

                // "Alt+N" badge on the right: Alt+digit runs the N-th visible row.
                let mut text_right = right - self.px(12.0);
                if i < self.shortcut_rows() {
                    let row_bg = if i == self.sel {
                        self.theme.sel
                    } else if Some(i) == self.hover {
                        blend(self.theme.sel, self.theme.bg)
                    } else {
                        self.theme.bg
                    };
                    let mut label = wide(&format!("Alt+{}", i + 1));
                    label.pop();
                    SelectObject(mem, self.fonts.sub.into());
                    let mut m = RECT::default();
                    DrawTextW(mem, &mut label, &mut m, DT_SINGLELINE | DT_CALCRECT | DT_NOPREFIX);
                    let (pw, ph) = (self.px(6.0), self.px(3.0));
                    let (bw, bh) = (m.right + 2 * pw, m.bottom + 2 * ph);
                    let badge = RECT { left: text_right - bw, top: top + (row_h - bh) / 2, right: text_right, bottom: top + (row_h + bh) / 2 };
                    let brush = CreateSolidBrush(COLORREF(mix(self.theme.text, row_bg, 0.08)));
                    let ob = SelectObject(mem, brush.into());
                    let op = SelectObject(mem, GetStockObject(NULL_PEN));
                    let r = self.px(4.0);
                    let _ = RoundRect(mem, badge.left, badge.top, badge.right + 1, badge.bottom + 1, r, r);
                    SelectObject(mem, op);
                    SelectObject(mem, ob);
                    let _ = DeleteObject(brush.into());
                    let mut br = badge;
                    SetTextColor(mem, COLORREF(self.theme.muted));
                    DrawTextW(mem, &mut label, &mut br, DT_SINGLELINE | DT_CENTER | DT_VCENTER | DT_NOPREFIX);
                    text_right = badge.left - self.px(12.0);
                }
                if item.pinned {
                    let mut pin: Vec<u16> = "\u{E718}".encode_utf16().collect();
                    let w = self.px(16.0);
                    let mut pr = RECT { left: text_right - w, top, right: text_right, bottom: top + row_h };
                    SelectObject(mem, self.fonts.icons.into());
                    SetTextColor(mem, COLORREF(self.theme.accent));
                    DrawTextW(mem, &mut pin, &mut pr, DT_SINGLELINE | DT_CENTER | DT_VCENTER | DT_NOPREFIX);
                    text_right = pr.left - self.px(8.0);
                }

                let tx = self.px(TEXT_X);
                let mut title = wide(&item.title);
                title.pop();
                let has_sub = !item.sub.is_empty();
                let mut tr = if has_sub {
                    RECT { left: tx, top: top + self.px(7.0), right: text_right, bottom: top + self.px(29.0) }
                } else {
                    RECT { left: tx, top, right: text_right, bottom: top + row_h }
                };
                SelectObject(mem, self.fonts.title.into());
                SetTextColor(mem, COLORREF(self.theme.text));
                DrawTextW(mem, &mut title, &mut tr, DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX);
                if has_sub {
                    let mut sub = wide(&item.sub);
                    sub.pop();
                    let mut sr = RECT { left: tx, top: top + self.px(28.0), right: text_right, bottom: top + self.px(46.0) };
                    SelectObject(mem, self.fonts.sub.into());
                    SetTextColor(mem, COLORREF(self.theme.muted));
                    DrawTextW(mem, &mut sub, &mut sr, DT_SINGLELINE | DT_VCENTER | DT_PATH_ELLIPSIS | DT_NOPREFIX);
                }
            }

            let _ = BitBlt(hdc, 0, 0, rc.right, rc.bottom, Some(mem), 0, 0, SRCCOPY);
            SelectObject(mem, old_bmp);
            let _ = DeleteObject(bmp.into());
            let _ = DeleteDC(mem);
        }
    }

    /// Height of the query band below the top padding, DIPs: `input_height` if set,
    /// otherwise Flow's proportions for the font size (see BAND_EM).
    pub(super) fn input_h(&self) -> f32 {
        let cfg = &self.engine.cfg;
        let band = if cfg.input_height > 0.0 { cfg.input_height } else { cfg.input_font_size * BAND_EM };
        band - PAD
    }

    /// Top and bottom of the area the query is centered in, px: from the window's top edge to
    /// the separator (or the bottom edge when there are no results), so the gaps match visually.
    pub(super) fn query_band(&self) -> (i32, i32) {
        (0, self.px(PAD + self.input_h()))
    }

    /// `search_icon` from the config, in the icon column: an image/exe/ico path is drawn as
    /// an icon, any other text (a symbol, an emoji) as a glyph; empty draws nothing.
    pub(super) fn paint_search_icon(&mut self, hdc: HDC) {
        let spec = self.engine.cfg.search_icon.clone();
        if spec.is_empty() {
            return;
        }
        let band = self.query_band();
        if std::path::Path::new(&spec).exists() {
            let size = self.px(ICON.min(self.engine.cfg.input_font_size * 1.4));
            let x = self.px(ICON_X) + (self.px(ICON) - size) / 2;
            self.icons.draw_sized(hdc, &spec, x, band.0 + (band.1 - band.0 - size) / 2, size);
            return;
        }
        let mut text: Vec<u16> = spec.encode_utf16().collect();
        let mut r = RECT { left: self.px(ICON_X), top: band.0, right: self.px(ICON_X + ICON), bottom: band.1 };
        unsafe {
            SelectObject(hdc, self.fonts.glyph.into());
            SetTextColor(hdc, COLORREF(self.theme.muted));
            DrawTextW(hdc, &mut text, &mut r, DT_SINGLELINE | DT_CENTER | DT_VCENTER | DT_NOPREFIX);
        }
    }
}
