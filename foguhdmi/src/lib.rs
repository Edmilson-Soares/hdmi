//! foguhdmi — desenhar em tela cheia num monitor (HDMI ou outro) a partir do
//! Fogu, com API C para o `fogu:ffi`.
//!
//! Convenções (Fogu 0.7+: qualquer número de argumentos e callbacks, ver
//! `cb.rs` e `fhd_set_sink`):
//! - Superfícies (janela num monitor, ou canvas fora da tela) são handles i64 > 0.
//! - Funções de status retornam 0 em sucesso e -1 em erro; a mensagem fica em
//!   `fhd_last_error`.
//! - Textos de saída (JSON) vão para um buffer do chamador, terminados em NUL;
//!   o retorno é o tamanho escrito, -(tamanho necessário) se não couber, -1 em erro.
//! - Cores: i64 `0xRRGGBB` (opaca) ou `0x1_AARRGGBB` (com alfa).
//! - Desenha-se num canvas em memória; `fhd_present` mostra o quadro.

mod cb;
pub mod image;
pub mod pix;

#[cfg(windows)]
pub mod audio;
#[cfg(windows)]
mod canvas;
#[cfg(windows)]
pub mod display;
#[cfg(windows)]
mod window;

use std::collections::HashMap;
use std::ffi::{CStr, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use pix::Paint;

static LAST_ERROR: Mutex<String> = Mutex::new(String::new());

fn set_error(msg: impl Into<String>) {
    *LAST_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = msg.into();
}

/// Executa `f` sem deixar um panic atravessar a fronteira C.
fn guard<T>(on_error: T, f: impl FnOnce() -> Result<T, String>) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            set_error(e);
            on_error
        }
        Err(_) => {
            set_error("internal panic in foguhdmi");
            on_error
        }
    }
}

unsafe fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

fn write_out(s: &str, buf: *mut u8, cap: i64) -> i64 {
    if let Some(n) = cb::try_sink(buf, cap, s.as_bytes()) {
        return n;
    }
    let need = s.len() as i64 + 1;
    if buf.is_null() || cap < need {
        return -need;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(s.as_ptr(), buf, s.len());
        *buf.add(s.len()) = 0;
    }
    s.len() as i64
}

/// BMP 24 bits (de baixo para cima) a partir de pixels 0x00RRGGBB.
pub fn encode_bmp(px: &[u32], w: i32, h: i32) -> Vec<u8> {
    let row = ((w * 3 + 3) / 4 * 4) as usize;
    let size = 54 + row * h as usize;
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(size as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&h.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&[0u8; 24]);
    for y in (0..h).rev() {
        let start = out.len();
        for x in 0..w {
            let c = px[(y * w + x) as usize];
            out.extend_from_slice(&[(c & 0xFF) as u8, ((c >> 8) & 0xFF) as u8, ((c >> 16) & 0xFF) as u8]);
        }
        out.resize(start + row, 0);
    }
    out
}

static RNG: AtomicU64 = AtomicU64::new(0);

fn next_random() -> u64 {
    let mut x = RNG.load(Ordering::Relaxed);
    if x == 0 {
        x = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1) | 1;
    }
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    RNG.store(x, Ordering::Relaxed);
    x
}

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[unsafe(no_mangle)]
pub extern "C" fn fhd_version(buf: *mut u8, cap: i64) -> i64 {
    write_out(VERSION, buf, cap)
}

#[unsafe(no_mangle)]
pub extern "C" fn fhd_last_error(buf: *mut u8, cap: i64) -> i64 {
    let e = LAST_ERROR.lock().unwrap_or_else(|e| e.into_inner()).clone();
    write_out(&e, buf, cap)
}

#[unsafe(no_mangle)]
pub extern "C" fn fhd_sin(x: f64) -> f64 {
    x.sin()
}

#[unsafe(no_mangle)]
pub extern "C" fn fhd_cos(x: f64) -> f64 {
    x.cos()
}

#[unsafe(no_mangle)]
pub extern "C" fn fhd_sqrt(x: f64) -> f64 {
    x.sqrt()
}

/// Inteiro aleatório em [0, n) (n <= 0 → 0).
#[unsafe(no_mangle)]
pub extern "C" fn fhd_random(n: i64) -> i64 {
    if n <= 0 {
        return 0;
    }
    (next_random() % n as u64) as i64
}

#[cfg(windows)]
pub use win::*;

#[cfg(windows)]
mod win {
    use super::*;
    use crate::canvas::Canvas;
    use crate::window::Window;

    pub struct Surface {
        canvas: Canvas,
        window: Option<Window>,
        stored: Vec<u32>,
    }

    /// Desenha uma imagem BMP em base64 (o que `img.toBase64("bmp")` do
    /// fogu:image produz) em (x, y) com tamanho w x h (<= 0: original).
    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_image(h: i64, b64: *const c_char, x: i64, y: i64, w: i64, hh: i64) -> i32 {
        guard(-1, || {
            if b64.is_null() {
                return Err("empty image".into());
            }
            let s = unsafe { CStr::from_ptr(b64) }.to_bytes();
            let s = std::str::from_utf8(s).map_err(|_| "image: base64 is not ASCII".to_string())?;
            let bytes = crate::image::decode_base64(s)?;
            let img = crate::image::decode_bmp(&bytes)?;
            with(h, |s| {
                s.canvas.pix().image(&img, x as i32, y as i32, w as i32, hh as i32);
                Ok(0)
            })
        })
    }

    // ---------------- áudio ----------------

    static SOUNDS: LazyLock<Mutex<HashMap<i64, Arc<Mutex<crate::audio::Sound>>>>> = LazyLock::new(Default::default);

    fn add_sound(s: crate::audio::Sound) -> i64 {
        let h = NEXT.fetch_add(1, Ordering::Relaxed);
        SOUNDS.lock().unwrap().insert(h, Arc::new(Mutex::new(s)));
        h
    }

    fn with_sound<T>(h: i64, f: impl FnOnce(&mut crate::audio::Sound) -> Result<T, String>) -> Result<T, String> {
        let s = SOUNDS.lock().unwrap().get(&h).cloned().ok_or_else(|| format!("invalid sound handle {h}"))?;
        let mut s = s.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut s)
    }

    fn vol(v: i64) -> f32 {
        v.clamp(0, 400) as f32 / 100.0
    }

    /// Saídas de som ativas: [{ id, name, display, default }]. display = áudio de
    /// monitor/TV (HDMI ou DisplayPort).
    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_audio_devices(buf: *mut u8, cap: i64) -> i64 {
        guard(-1, || {
            let list: Vec<_> = crate::audio::devices()?
                .into_iter()
                .map(|d| serde_json::json!({ "id": d.id, "name": d.name, "display": d.display, "default": d.default }))
                .collect();
            Ok(write_out(&serde_json::Value::Array(list).to_string(), buf, cap))
        })
    }

    /// Volume do Windows da saída: which 0 = volume (0..100), 1 = sem som (1/0),
    /// 2 = pico do sinal saindo agora (0..1000).
    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_audio_level(device: *const c_char, which: i64) -> i64 {
        let device = unsafe { cstr(device) };
        guard(-1, || {
            if which == 2 {
                return Ok((crate::audio::endpoint_peak(&device)? * 1000.0).round() as i64);
            }
            let (v, m) = crate::audio::endpoint_volume(&device)?;
            Ok(if which == 0 { (v * 100.0).round() as i64 } else { m as i64 })
        })
    }

    /// Muda o volume do Windows da saída (volume < 0: não muda; mute: -1 não
    /// muda, 0 com som, 1 sem som).
    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_audio_set_level(device: *const c_char, volume: i64, mute: i64) -> i32 {
        let device = unsafe { cstr(device) };
        guard(-1, || {
            let level = if volume < 0 { -1.0 } else { volume.min(100) as f32 / 100.0 };
            let m = if mute < 0 { None } else { Some(mute != 0) };
            crate::audio::set_endpoint_volume(&device, level, m).map(|_| 0)
        })
    }

    /// Vozes instaladas (SAPI + OneCore): [{ id, name, language }].
    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_voices(buf: *mut u8, cap: i64) -> i64 {
        guard(-1, || {
            let list: Vec<_> = crate::audio::voices()
                .into_iter()
                .map(|v| serde_json::json!({ "id": v.id, "name": v.name, "language": v.language }))
                .collect();
            Ok(write_out(&serde_json::Value::Array(list).to_string(), buf, cap))
        })
    }

    /// Fala `text` no dispositivo `device` ("" = padrão). voice: nome, idioma
    /// ("pt") ou id; rate -10..10; volume 0..400 (%); flags: 1 = texto em XML/SSML.
    /// Devolve o handle do som (já tocando).
    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_say(device: *const c_char, text: *const c_char, voice: *const c_char, rate: i64, volume: i64, flags: i64) -> i64 {
        let (device, text, voice) = unsafe { (cstr(device), cstr(text), cstr(voice)) };
        guard(-1, || {
            let pcm = crate::audio::synth(&text, &voice, rate as i32, flags & 1 != 0)?;
            let data = pcm.iter().map(|&s| s as f32 / 32768.0).collect();
            let spec = crate::audio::Spec::Pcm { data, channels: 1, rate: crate::audio::TTS_RATE };
            Ok(add_sound(crate::audio::play(&device, spec, vol(volume), false)?))
        })
    }

    /// Grava a fala num arquivo .wav, .mp3 ou .m4a (pela extensão).
    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_tts_save(text: *const c_char, voice: *const c_char, rate: i64, path: *const c_char, flags: i64) -> i32 {
        let (text, voice, path) = unsafe { (cstr(text), cstr(voice), cstr(path)) };
        guard(-1, || {
            let pcm = crate::audio::synth(&text, &voice, rate as i32, flags & 1 != 0)?;
            if let Some(dir) = std::path::Path::new(&path).parent() {
                if !dir.as_os_str().is_empty() {
                    std::fs::create_dir_all(dir).map_err(|e| format!("{path}: {e}"))?;
                }
            }
            if path.to_lowercase().ends_with(".wav") {
                std::fs::write(&path, crate::audio::encode_wav(&pcm, 1, crate::audio::TTS_RATE)).map_err(|e| format!("{path}: {e}"))?;
            } else {
                crate::audio::encode_compressed(&pcm, crate::audio::TTS_RATE, &path)?;
            }
            Ok(0)
        })
    }

    /// Toca um arquivo (wav, mp3, m4a/aac, wma, flac…). loop != 0 repete.
    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_play(device: *const c_char, path: *const c_char, volume: i64, looped: i64) -> i64 {
        let (device, path) = unsafe { (cstr(device), cstr(path)) };
        guard(-1, || Ok(add_sound(crate::audio::play(&device, crate::audio::Spec::File(path), vol(volume), looped != 0)?)))
    }

    /// Tom senoidal de `freq` Hz por `ms` milissegundos.
    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_tone(device: *const c_char, freq: i64, ms: i64, volume: i64) -> i64 {
        let device = unsafe { cstr(device) };
        guard(-1, || Ok(add_sound(crate::audio::play(&device, crate::audio::tone(freq as f32, ms.max(1) as u32), vol(volume), false)?)))
    }

    /// which: 0 tocando (1/0), 1 posição ms, 2 duração ms (-1 desconhecida),
    /// 3 canais, 4 taxa de amostragem, 5 terminou com erro (1/0).
    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_sound_info(h: i64, which: i64) -> i64 {
        guard(-1, || {
            with_sound(h, |s| {
                Ok(match which {
                    0 => s.playing() as i64,
                    1 => s.position_ms(),
                    2 => {
                        let d = s.state.duration_ms.load(Ordering::Relaxed);
                        if d == u64::MAX { -1 } else { d as i64 }
                    }
                    3 => s.state.channels.load(Ordering::Relaxed) as i64,
                    4 => s.state.rate.load(Ordering::Relaxed) as i64,
                    _ => (!s.state.error.lock().unwrap().is_empty()) as i64,
                })
            })
        })
    }

    /// Espera até `timeout_ms` o som terminar. Retorna 1 se terminou, 0 se
    /// ainda está tocando quando o tempo acabou, -1 em erro. Quem chama em
    /// fatias curtas (som.fg) não trava as outras tarefas fg.
    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_sound_wait(h: i64, timeout_ms: i64) -> i64 {
        guard(-1, || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms.max(0) as u64);
            loop {
                if !with_sound(h, |s| Ok(s.playing()))? {
                    return Ok(1);
                }
                let now = std::time::Instant::now();
                if now >= deadline {
                    return Ok(0);
                }
                std::thread::sleep((deadline - now).min(std::time::Duration::from_millis(2)));
            }
        })
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_sound_error(h: i64, buf: *mut u8, cap: i64) -> i64 {
        guard(-1, || with_sound(h, |s| Ok(write_out(&s.state.error.lock().unwrap(), buf, cap))))
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_sound_volume(h: i64, volume: i64) -> i32 {
        guard(-1, || {
            with_sound(h, |s| {
                s.set_volume(vol(volume));
                Ok(0)
            })
        })
    }

    /// Para o som (se ainda tocar) e libera o handle.
    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_sound_stop(h: i64) -> i32 {
        guard(-1, || {
            let s = SOUNDS.lock().unwrap().remove(&h).ok_or_else(|| format!("invalid sound handle {h}"))?;
            s.lock().unwrap_or_else(|e| e.into_inner()).stop();
            Ok(0)
        })
    }

    /// Segundos desde a meia-noite no fuso local (com milissegundos: valor * 1000).
    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_local_millis() -> i64 {
        let mut st: windows_sys::Win32::Foundation::SYSTEMTIME = unsafe { std::mem::zeroed() };
        unsafe { windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut st) };
        ((st.wHour as i64 * 60 + st.wMinute as i64) * 60 + st.wSecond as i64) * 1000 + st.wMilliseconds as i64
    }

    /// Guarda uma cópia do canvas (ex.: o fundo estático de uma animação).
    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_store(h: i64) -> i32 {
        draw(h, |s| {
            let px = s.canvas.pixels().to_vec();
            s.stored = px;
        })
    }

    /// Copia de volta o que fhd_store guardou (erro se nada foi guardado).
    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_recall(h: i64) -> i32 {
        guard(-1, || {
            with(h, |s| {
                if s.stored.is_empty() {
                    return Err("nothing stored (call fhd_store first)".into());
                }
                let stored = std::mem::take(&mut s.stored);
                s.canvas.pixels().copy_from_slice(&stored);
                s.stored = stored;
                Ok(0)
            })
        })
    }

    static SURFACES: LazyLock<Mutex<HashMap<i64, Arc<Mutex<Surface>>>>> = LazyLock::new(Default::default);
    static NEXT: AtomicI64 = AtomicI64::new(1);

    fn add(s: Surface) -> i64 {
        let h = NEXT.fetch_add(1, Ordering::Relaxed);
        SURFACES.lock().unwrap().insert(h, Arc::new(Mutex::new(s)));
        h
    }

    fn with<T>(h: i64, f: impl FnOnce(&mut Surface) -> Result<T, String>) -> Result<T, String> {
        let s = SURFACES.lock().unwrap().get(&h).cloned().ok_or_else(|| format!("invalid screen handle {h}"))?;
        let mut s = s.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut s)
    }

    fn draw(h: i64, f: impl FnOnce(&mut Surface)) -> i32 {
        guard(-1, || {
            with(h, |s| {
                f(s);
                Ok(0)
            })
        })
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_monitors(buf: *mut u8, cap: i64) -> i64 {
        guard(-1, || {
            let list: Vec<_> = crate::display::monitors().iter().map(|m| m.to_json()).collect();
            Ok(write_out(&serde_json::Value::Array(list).to_string(), buf, cap))
        })
    }

    /// Telas conectadas, ativas ou não: [{ id, name, output, hdmi, active }].
    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_connected(buf: *mut u8, cap: i64) -> i64 {
        guard(-1, || {
            let list: Vec<_> = crate::display::connected()
                .into_iter()
                .map(|(id, name, output, active)| {
                    serde_json::json!({ "id": id, "name": name, "hdmi": output == "hdmi", "output": output, "active": active })
                })
                .collect();
            Ok(write_out(&serde_json::Value::Array(list).to_string(), buf, cap))
        })
    }

    /// Modo de projeção atual: 1 só PC, 2 duplicar, 4 estender, 8 só a segunda tela.
    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_topology() -> i64 {
        guard(-1, || Ok(crate::display::topology() as i64))
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_set_topology(mode: i64) -> i32 {
        guard(-1, || crate::display::set_topology(mode as u32).map(|_| 0))
    }

    /// Abre uma janela no monitor `index` (ordem de fhd_monitors).
    /// width/height <= 0: tela cheia sem borda; senão, janela com área útil
    /// width x height centralizada no monitor.
    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_open(index: i64, width: i64, height: i64, title: *const c_char) -> i64 {
        guard(-1, || {
            let title = unsafe { cstr(title) };
            let mons = crate::display::monitors();
            let m = mons
                .get(index as usize)
                .filter(|_| index >= 0)
                .ok_or_else(|| format!("monitor {index} not found ({} connected)", mons.len()))?;
            let fullscreen = width <= 0 || height <= 0;
            let (x, y, w, h) = if fullscreen {
                (m.x, m.y, m.w, m.h)
            } else {
                let (w, h) = (width as i32, height as i32);
                let (_, _, ow, oh) = Window::outer_rect(0, 0, w, h);
                (m.x + (m.w - ow) / 2, m.y + (m.h - oh) / 2, ow, oh)
            };
            let title = if title.is_empty() { "Fogu HDMI".to_string() } else { title };
            let window = Window::open(x, y, w, h, fullscreen, &title)?;
            let canvas = Canvas::new(window.w, window.h)?;
            Ok(add(Surface { canvas, window: Some(window), stored: Vec::new() }))
        })
    }

    /// Canvas fora da tela (testes, imagens): desenha e salva sem janela.
    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_canvas(width: i64, height: i64) -> i64 {
        guard(-1, || Ok(add(Surface { canvas: Canvas::new(width as i32, height as i32)?, window: None, stored: Vec::new() })))
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_close(h: i64) -> i32 {
        guard(-1, || {
            let s = SURFACES.lock().unwrap().remove(&h).ok_or_else(|| format!("invalid screen handle {h}"))?;
            let mut s = s.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(mut w) = s.window.take() {
                w.close();
            }
            Ok(0)
        })
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_width(h: i64) -> i64 {
        guard(-1, || with(h, |s| Ok(s.canvas.w as i64)))
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_height(h: i64) -> i64 {
        guard(-1, || with(h, |s| Ok(s.canvas.h as i64)))
    }

    /// 1 enquanto a janela está aberta (Esc fecha); canvas: sempre 1.
    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_is_open(h: i64) -> i32 {
        guard(0, || with(h, |s| Ok(s.window.as_ref().map(|w| w.is_open()).unwrap_or(true) as i32)))
    }

    /// Próxima tecla pressionada (código virtual do Windows) ou 0.
    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_key(h: i64) -> i64 {
        guard(-1, || with(h, |s| Ok(s.window.as_ref().map(|w| w.key()).unwrap_or(0))))
    }

    /// Todas as teclas pressionadas desde a última chamada, como JSON `[65, 32]`
    /// (pelo sink ou num buffer): um laço de quadros lê a fila inteira numa
    /// chamada, sem perder teclas quando várias chegam no mesmo quadro.
    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_keys(h: i64, buf: *mut u8, cap: i64) -> i64 {
        guard(-1, || {
            let keys: Vec<i64> = with(h, |s| {
                Ok(match s.window.as_ref() {
                    Some(w) => std::iter::from_fn(|| Some(w.key()).filter(|&k| k != 0)).collect(),
                    None => Vec::new(),
                })
            })?;
            Ok(write_out(&serde_json::to_string(&keys).unwrap_or_else(|_| "[]".into()), buf, cap))
        })
    }

    /// which: 0 = x, 1 = y, 2 = botões (1 esquerdo, 2 direito, 4 meio).
    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_mouse(h: i64, which: i64) -> i64 {
        guard(-1, || {
            with(h, |s| {
                let (x, y, b) = s.window.as_ref().map(|w| w.mouse()).unwrap_or((0, 0, 0));
                Ok(match which {
                    0 => x,
                    1 => y,
                    _ => b,
                } as i64)
            })
        })
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_present(h: i64) -> i32 {
        guard(-1, || {
            with(h, |s| {
                if let Some(w) = &s.window {
                    let px = s.canvas.pixels();
                    w.present(px);
                }
                Ok(0)
            })
        })
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_clear(h: i64, c: i64) -> i32 {
        draw(h, |s| s.canvas.pix().clear(Paint::from_i64(c)))
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_pixel(h: i64, x: i64, y: i64, c: i64) -> i32 {
        draw(h, |s| s.canvas.pix().pixel(x as i32, y as i32, Paint::from_i64(c)))
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_rect(h: i64, x: i64, y: i64, w: i64, hh: i64, c: i64, thick: i64) -> i32 {
        draw(h, |s| s.canvas.pix().rect(x as i32, y as i32, w as i32, hh as i32, Paint::from_i64(c), thick as i32))
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_round_rect(h: i64, x: i64, y: i64, w: i64, hh: i64, r: i64, c: i64) -> i32 {
        draw(h, |s| s.canvas.pix().round_rect(x as i32, y as i32, w as i32, hh as i32, r as i32, Paint::from_i64(c)))
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_line(h: i64, x0: i64, y0: i64, x1: i64, y1: i64, c: i64, thick: i64) -> i32 {
        draw(h, |s| s.canvas.pix().line(x0 as f32, y0 as f32, x1 as f32, y1 as f32, Paint::from_i64(c), thick as f32))
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_circle(h: i64, cx: i64, cy: i64, r: i64, c: i64, thick: i64) -> i32 {
        draw(h, |s| s.canvas.pix().circle(cx as f32, cy as f32, r as f32, Paint::from_i64(c), thick as f32))
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_triangle(h: i64, x0: i64, y0: i64, x1: i64, y1: i64, x2: i64, y2: i64, c: i64) -> i32 {
        draw(h, |s| {
            s.canvas
                .pix()
                .triangle([(x0 as f32, y0 as f32), (x1 as f32, y1 as f32), (x2 as f32, y2 as f32)], Paint::from_i64(c))
        })
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_gradient(h: i64, x: i64, y: i64, w: i64, hh: i64, c1: i64, c2: i64, vertical: i64) -> i32 {
        draw(h, |s| {
            s.canvas.pix().gradient(x as i32, y as i32, w as i32, hh as i32, Paint::from_i64(c1), Paint::from_i64(c2), vertical != 0)
        })
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_font(h: i64, face: *const c_char, bold: i64) -> i32 {
        let face = unsafe { cstr(face) };
        draw(h, |s| s.canvas.set_font(&face, bold != 0))
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_text(h: i64, x: i64, y: i64, text: *const c_char, c: i64, size: i64) -> i32 {
        let text = unsafe { cstr(text) };
        draw(h, |s| s.canvas.text(x as i32, y as i32, &text, Paint::from_i64(c), size as i32))
    }

    /// which: 0 = largura, 1 = altura.
    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_text_size(h: i64, text: *const c_char, size: i64, which: i64) -> i64 {
        let text = unsafe { cstr(text) };
        guard(-1, || {
            with(h, |s| {
                let (w, hh) = s.canvas.text_size(&text, size as i32);
                Ok(if which == 0 { w } else { hh } as i64)
            })
        })
    }

    /// Salva o canvas (o quadro sendo desenhado) como BMP.
    #[unsafe(no_mangle)]
    pub extern "C" fn fhd_save(h: i64, path: *const c_char) -> i32 {
        let path = unsafe { cstr(path) };
        guard(-1, || {
            with(h, |s| {
                let (w, hh) = (s.canvas.w, s.canvas.h);
                let data = encode_bmp(s.canvas.pixels(), w, hh);
                if let Some(dir) = std::path::Path::new(&path).parent() {
                    if !dir.as_os_str().is_empty() {
                        std::fs::create_dir_all(dir).map_err(|e| format!("{path}: {e}"))?;
                    }
                }
                std::fs::write(&path, data).map_err(|e| format!("{path}: {e}"))?;
                Ok(0)
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bmp_header() {
        let b = encode_bmp(&[0xFF0000, 0x00FF00, 0x0000FF, 0xFFFFFF], 2, 2);
        assert_eq!(&b[0..2], b"BM");
        assert_eq!(b.len(), 54 + 8 * 2);
        // primeira linha do arquivo = última da imagem: azul, branco (BGR)
        assert_eq!(&b[54..60], &[0xFF, 0, 0, 0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn random_in_range() {
        for _ in 0..1000 {
            let r = fhd_random(10);
            assert!((0..10).contains(&r));
        }
        assert_eq!(fhd_random(0), 0);
    }

    #[cfg(windows)]
    #[test]
    fn offscreen_canvas_draws_and_saves() {
        let h = fhd_canvas(64, 32);
        assert!(h > 0);
        assert_eq!(fhd_width(h), 64);
        assert_eq!(fhd_clear(h, 0x102030), 0);
        let t = std::ffi::CString::new("Hi").unwrap();
        assert!(fhd_text_size(h, t.as_ptr(), 20, 0) > 0);
        assert_eq!(fhd_text(h, 2, 2, t.as_ptr(), 0xFFFFFF, 20), 0);
        let path = std::env::temp_dir().join("foguhdmi_test.bmp");
        let p = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        assert_eq!(fhd_save(h, p.as_ptr()), 0);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 54 + 64 * 3 * 32);
        assert_eq!(fhd_close(h), 0);
        assert_eq!(fhd_clear(h, 0), -1);
    }

    #[cfg(windows)]
    #[test]
    fn lists_monitors() {
        let mons = display::monitors();
        assert!(!mons.is_empty());
        assert!(mons.iter().any(|m| m.primary));
    }
}

/// Registra (ou remove, com NULL) o callback (Fogu 0.7+, `ffi.callback`) que
/// recebe as saídas das funções chamadas com `buf = NULL, cap = 0` (ver `cb.rs`).
#[unsafe(no_mangle)]
pub extern "C" fn fhd_set_sink(sink: Option<cb::Sink>) -> i32 {
    cb::set_sink(sink);
    0
}
