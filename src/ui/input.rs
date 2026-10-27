//! The query EDIT: keys it handles itself, Latin input whatever the layout, hotkey parsing.

use super::*;

/// Deletes the word before the caret (Ctrl+Backspace, which a plain EDIT doesn't support).
pub(super) fn delete_word(edit: HWND) {
    unsafe {
        let mut start = 0u32;
        let mut end = 0u32;
        SendMessageW(edit, EM_GETSEL, Some(WPARAM(&mut start as *mut _ as usize)), Some(LPARAM(&mut end as *mut _ as isize)));
        let len = GetWindowTextLengthW(edit);
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(edit, &mut buf) as usize;
        let chars = &buf[..n];
        let mut i = start as usize;
        if start != end {
            i = start as usize;
        } else {
            while i > 0 && chars[i - 1] == b' ' as u16 {
                i -= 1;
            }
            while i > 0 && chars[i - 1] != b' ' as u16 {
                i -= 1;
            }
        }
        SendMessageW(edit, EM_SETSEL, Some(WPARAM(i)), Some(LPARAM(end as isize)));
        let empty = [0u16];
        SendMessageW(edit, EM_REPLACESEL, Some(WPARAM(1)), Some(LPARAM(empty.as_ptr() as isize)));
    }
}

/// The caret is at the end of the query (nothing selected or a selection reaching the end),
/// so Right arrow has nothing left to move over.
pub(super) fn caret_at_end(edit: HWND) -> bool {
    unsafe {
        let mut start = 0u32;
        let mut end = 0u32;
        SendMessageW(edit, EM_GETSEL, Some(WPARAM(&mut start as *mut _ as usize)), Some(LPARAM(&mut end as *mut _ as isize)));
        end as i32 >= GetWindowTextLengthW(edit)
    }
}

pub(super) unsafe extern "system" fn edit_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let parent = unsafe { GetParent(hwnd).unwrap_or_default() };
    let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
    let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
    match msg {
        WM_KEYDOWN => match VIRTUAL_KEY(wp.0 as u16) {
            VK_DOWN if ctrl => return { shift_pin(parent, 1); LRESULT(0) },
            VK_UP if ctrl => return { shift_pin(parent, -1); LRESULT(0) },
            VK_DOWN => return { move_selection(parent, 1); LRESULT(0) },
            VK_UP => return { move_selection(parent, -1); LRESULT(0) },
            VIRTUAL_KEY(0x50) if ctrl => return { toggle_pin(parent); LRESULT(0) }, // Ctrl+P
            VK_NEXT => return { move_selection(parent, 5); LRESULT(0) },
            VK_PRIOR => return { move_selection(parent, -5); LRESULT(0) },
            VK_RETURN => return { activate(parent, None, ctrl && shift); LRESULT(0) },
            VK_ESCAPE => return { if !toggle_menu(parent, false) { hide(parent) }; LRESULT(0) },
            VK_LEFT if with(|s| s.menu.is_some()).unwrap_or(false) => return { toggle_menu(parent, false); LRESULT(0) },
            VK_RIGHT if !ctrl && !shift && caret_at_end(hwnd) => {
                if toggle_menu(parent, true) {
                    return LRESULT(0);
                }
            }
            VK_TAB => {
                // Tab completes a keyword hint or the selected title.
                let q = with(|s| {
                    s.items.get(s.sel).map(|it| match &it.action {
                        Action::SetQuery(q) => q.clone(),
                        _ => it.title.clone(),
                    })
                })
                .flatten();
                if let Some(q) = q {
                    set_query(parent, &q);
                }
                return LRESULT(0);
            }
            _ => {}
        },
        // Swallow the chars of keys handled above so the EDIT doesn't beep or insert them.
        WM_CHAR => match wp.0 as u32 {
            0x0D | 0x1B | 0x09 | 0x10 => return LRESULT(0), // 0x10: Ctrl+P
            0x7F => return { delete_word(hwnd); LRESULT(0) },
            0x01 => return { unsafe { SendMessageW(hwnd, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1))) }; LRESULT(0) },
            c => {
                if let Some(en) = english_char(c, lp) {
                    let prev = EDIT_PROC.load(Ordering::Relaxed);
                    return unsafe { CallWindowProcW(std::mem::transmute::<isize, WNDPROC>(prev), hwnd, msg, WPARAM(en as usize), lp) };
                }
            }
        },
        // Alt+1..9: run the N-th visible row (the badge on its right).
        WM_SYSKEYDOWN if (0x31..=0x39).contains(&wp.0) => {
            let n = wp.0 - 0x31;
            if let Some(i) = with(|s| (n < s.shortcut_rows()).then_some(n)).flatten() {
                with(|s| s.select(i));
                activate(parent, Some(i), false);
            }
            return LRESULT(0);
        }
        // Its WM_SYSCHAR would otherwise beep.
        WM_SYSCHAR if (0x31..=0x39).contains(&wp.0) => return LRESULT(0),
        WM_MOUSEWHEEL => {
            unsafe { SendMessageW(parent, msg, Some(wp), Some(lp)) };
            return LRESULT(0);
        }
        _ => {}
    }
    let _ = shift;
    let prev = EDIT_PROC.load(Ordering::Relaxed);
    unsafe { CallWindowProcW(std::mem::transmute::<isize, WNDPROC>(prev), hwnd, msg, wp, lp) }
}

/// Keyboard layout of the first keystroke since the window was shown (0: none yet).
pub(super) static SESSION_LAYOUT: AtomicIsize = AtomicIsize::new(0);

/// SESSION_LAYOUT value once the user switched layouts in this session.
pub(super) const LAYOUT_SWITCHED: isize = -1;

pub(super) fn is_english(hkl: HKL) -> bool {
    hkl.0 as usize & 0x3FF == 0x09 // LANG_ENGLISH in the low word
}

/// With `force_english`, the character the same key would type in an English layout:
/// typing "туфлиц" in a Russian layout gives "telegram". The system layout is left alone.
/// Switching the layout while the window is open turns this off until it is shown again,
/// so Cyrillic can still be typed on purpose.
pub(super) fn english_char(c: u32, lp: LPARAM) -> Option<u16> {
    if c < 0x20 || !with(|s| s.engine.cfg.force_english).unwrap_or(false) {
        return None;
    }
    unsafe {
        let current = GetKeyboardLayout(0);
        let session = SESSION_LAYOUT.compare_exchange(0, current.0 as isize, Ordering::Relaxed, Ordering::Relaxed).unwrap_or_else(|v| v);
        if session == LAYOUT_SWITCHED {
            return None;
        }
        if session != 0 && session != current.0 as isize {
            // Switched on purpose; stays off even if they switch back.
            SESSION_LAYOUT.store(LAYOUT_SWITCHED, Ordering::Relaxed);
            return None;
        }
        if is_english(current) {
            return None;
        }
        let mut layouts = [HKL::default(); 32];
        let n = GetKeyboardLayoutList(Some(&mut layouts)) as usize;
        let en = layouts[..n.min(32)].iter().copied().find(|&l| is_english(l))?;
        let scan = ((lp.0 >> 16) & 0xFF) as u32;
        let vk = MapVirtualKeyExW(scan, MAPVK_VSC_TO_VK_EX, Some(en));
        let mut state = [0u8; 256];
        GetKeyboardState(&mut state).ok()?;
        let mut buf = [0u16; 4];
        // Flag 4: don't touch the kernel's dead-key state.
        (ToUnicodeEx(vk, scan, &state, &mut buf, 4, Some(en)) == 1 && buf[0] >= 0x20).then_some(buf[0])
    }
}

/// "Alt+Space" -> (MOD_ALT, VK_SPACE).
pub(super) fn parse_hotkey(s: &str) -> Option<(HOT_KEY_MODIFIERS, u32)> {
    let mut mods = MOD_NOREPEAT;
    let mut vk = None;
    for part in s.split('+').map(|p| p.trim().to_lowercase()) {
        match part.as_str() {
            "alt" => mods |= MOD_ALT,
            "ctrl" | "control" => mods |= MOD_CONTROL,
            "shift" => mods |= MOD_SHIFT,
            "win" => mods |= MOD_WIN,
            "space" => vk = Some(VK_SPACE.0 as u32),
            "tab" => vk = Some(VK_TAB.0 as u32),
            "enter" => vk = Some(VK_RETURN.0 as u32),
            "`" | "oem3" => vk = Some(VK_OEM_3.0 as u32),
            k if k.len() == 1 && k.chars().all(|c| c.is_ascii_alphanumeric()) => vk = Some(k.to_ascii_uppercase().as_bytes()[0] as u32),
            k if k.starts_with('f') && k[1..].parse::<u32>().is_ok_and(|n| (1..=24).contains(&n)) => {
                vk = Some(VK_F1.0 as u32 + k[1..].parse::<u32>().unwrap() - 1)
            }
            _ => return None,
        }
    }
    vk.map(|v| (mods, v))
}
