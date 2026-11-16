//! Monitores conectados: posição/tamanho (EnumDisplayMonitors) e o tipo de
//! conexão de cada um (QueryDisplayConfig: HDMI, DisplayPort, interno...).

use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};

use serde_json::{Value, json};
use windows_sys::Win32::Devices::Display::*;
use windows_sys::Win32::Foundation::{BOOL, LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext};

#[derive(Clone, Debug)]
pub struct Monitor {
    pub index: usize,
    pub device: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub primary: bool,
    /// Um alvo por tela física (mais de um quando a imagem está duplicada).
    pub targets: Vec<(String, String)>, // (nome do monitor, conexão)
}

impl Monitor {
    pub fn is_hdmi(&self) -> bool {
        self.targets.iter().any(|(_, o)| o == "hdmi")
    }

    pub fn to_json(&self) -> Value {
        let names: Vec<&str> = self.targets.iter().map(|(n, _)| n.as_str()).filter(|n| !n.is_empty()).collect();
        let outputs: Vec<&str> = self.targets.iter().map(|(_, o)| o.as_str()).collect();
        json!({
            "index": self.index,
            "device": self.device,
            "name": names.join(" + "),
            "output": outputs.first().copied().unwrap_or("unknown"),
            "outputs": outputs,
            "hdmi": self.is_hdmi(),
            "primary": self.primary,
            "x": self.x, "y": self.y, "width": self.w, "height": self.h,
        })
    }
}

fn wide_to_string(w: &[u16]) -> String {
    let n = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..n])
}

pub fn output_name(tech: i32) -> &'static str {
    match tech as u32 {
        0 => "vga",
        1 => "svideo",
        2 => "composite",
        3 => "component",
        4 => "dvi",
        5 => "hdmi",
        6 => "lvds",
        8 => "d-jpn",
        9 => "sdi",
        10 => "displayport",
        11 => "displayport-embedded",
        12 => "udi",
        13 => "udi-embedded",
        14 => "sdtv-dongle",
        15 => "miracast",
        16 => "indirect-wired",
        17 => "indirect-virtual",
        0x8000_0000 => "internal",
        _ => "other",
    }
}

/// gdi device name (\\.\DISPLAY1) -> [(nome amigável, conexão)]
fn display_config() -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    unsafe {
        let (mut np, mut nm) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) != 0 {
            return out;
        }
        let mut paths: Vec<DISPLAYCONFIG_PATH_INFO> = vec![zeroed(); np as usize];
        let mut modes: Vec<DISPLAYCONFIG_MODE_INFO> = vec![zeroed(); nm as usize];
        if QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), null_mut()) != 0 {
            return out;
        }
        for p in &paths[..np as usize] {
            let mut src: DISPLAYCONFIG_SOURCE_DEVICE_NAME = zeroed();
            src.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            src.header.size = size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            src.header.adapterId = p.sourceInfo.adapterId;
            src.header.id = p.sourceInfo.id;
            if DisplayConfigGetDeviceInfo(&mut src.header) != 0 {
                continue;
            }
            let mut tgt: DISPLAYCONFIG_TARGET_DEVICE_NAME = zeroed();
            tgt.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
            tgt.header.size = size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
            tgt.header.adapterId = p.targetInfo.adapterId;
            tgt.header.id = p.targetInfo.id;
            let (name, tech) = if DisplayConfigGetDeviceInfo(&mut tgt.header) == 0 {
                (wide_to_string(&tgt.monitorFriendlyDeviceName), tgt.outputTechnology)
            } else {
                (String::new(), p.targetInfo.outputTechnology)
            };
            out.push((wide_to_string(&src.viewGdiDeviceName), name, output_name(tech).to_string()));
        }
    }
    out
}

unsafe extern "system" fn enum_cb(hmon: HMONITOR, _dc: HDC, _r: *mut RECT, data: LPARAM) -> BOOL {
    unsafe {
        let list = &mut *(data as *mut Vec<Monitor>);
        let mut mi: MONITORINFOEXW = zeroed();
        mi.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(hmon, &mut mi as *mut MONITORINFOEXW as *mut MONITORINFO) != 0 {
            let r = mi.monitorInfo.rcMonitor;
            list.push(Monitor {
                index: list.len(),
                device: wide_to_string(&mi.szDevice),
                x: r.left,
                y: r.top,
                w: r.right - r.left,
                h: r.bottom - r.top,
                primary: mi.monitorInfo.dwFlags & 1 != 0, // MONITORINFOF_PRIMARY,
                targets: Vec::new(),
            });
        }
        1
    }
}

/// Telas fisicamente conectadas, ativas ou não: (id, nome, conexão, ativa).
/// Uma tela HDMI conectada mas inativa ("Somente tela do PC") aparece aqui
/// e não em `monitors()`.
pub fn connected() -> Vec<(u32, String, String, bool)> {
    let mut out: Vec<(u32, String, String, bool)> = Vec::new();
    unsafe {
        let (mut np, mut nm) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ALL_PATHS, &mut np, &mut nm) != 0 {
            return out;
        }
        let mut paths: Vec<DISPLAYCONFIG_PATH_INFO> = vec![zeroed(); np as usize];
        let mut modes: Vec<DISPLAYCONFIG_MODE_INFO> = vec![zeroed(); nm as usize];
        if QueryDisplayConfig(QDC_ALL_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), null_mut()) != 0 {
            return out;
        }
        for p in &paths[..np as usize] {
            if p.targetInfo.targetAvailable == 0 {
                continue;
            }
            let active = p.flags & DISPLAYCONFIG_PATH_ACTIVE != 0;
            if let Some(e) = out.iter_mut().find(|e| e.0 == p.targetInfo.id) {
                e.3 |= active;
                continue;
            }
            let mut tgt: DISPLAYCONFIG_TARGET_DEVICE_NAME = zeroed();
            tgt.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
            tgt.header.size = size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
            tgt.header.adapterId = p.targetInfo.adapterId;
            tgt.header.id = p.targetInfo.id;
            let (name, tech) = if DisplayConfigGetDeviceInfo(&mut tgt.header) == 0 {
                (wide_to_string(&tgt.monitorFriendlyDeviceName), tgt.outputTechnology)
            } else {
                (String::new(), p.targetInfo.outputTechnology)
            };
            out.push((p.targetInfo.id, name, output_name(tech).to_string(), active));
        }
    }
    out
}

/// Muda o modo de projeção (como Win+P): 1 só a tela do PC, 2 duplicar,
/// 4 estender, 8 só a segunda tela.
pub fn set_topology(mode: u32) -> Result<(), String> {
    if ![1, 2, 4, 8].contains(&mode) {
        return Err(format!("invalid topology {mode} (1 internal, 2 clone, 4 extend, 8 external)"));
    }
    let rc = unsafe { SetDisplayConfig(0, null(), 0, null(), SDC_APPLY | mode) };
    if rc != 0 {
        return Err(format!("SetDisplayConfig failed ({rc})"));
    }
    Ok(())
}

/// Modo de projeção atual (1, 2, 4 ou 8; 0 se desconhecido).
pub fn topology() -> u32 {
    unsafe {
        let (mut np, mut nm) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_DATABASE_CURRENT, &mut np, &mut nm) != 0 {
            return 0;
        }
        let mut paths: Vec<DISPLAYCONFIG_PATH_INFO> = vec![zeroed(); np as usize];
        let mut modes: Vec<DISPLAYCONFIG_MODE_INFO> = vec![zeroed(); nm as usize];
        let mut topo: DISPLAYCONFIG_TOPOLOGY_ID = 0;
        if QueryDisplayConfig(QDC_DATABASE_CURRENT, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), &mut topo) != 0 {
            return 0;
        }
        topo as u32
    }
}

/// Lista os monitores em pixels físicos (independente da escala do Windows).
pub fn monitors() -> Vec<Monitor> {
    let mut list: Vec<Monitor> = Vec::new();
    unsafe {
        let prev = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        EnumDisplayMonitors(null_mut(), null(), Some(enum_cb), &mut list as *mut Vec<Monitor> as LPARAM);
        if !prev.is_null() {
            SetThreadDpiAwarenessContext(prev);
        }
    }
    let cfg = display_config();
    for m in list.iter_mut() {
        for (dev, name, out) in &cfg {
            if dev.eq_ignore_ascii_case(&m.device) {
                m.targets.push((name.clone(), out.clone()));
            }
        }
    }
    list
}
