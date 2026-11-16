//! Áudio: saídas de som (WASAPI), reprodução de arquivos (Media Foundation:
//! wav, mp3, m4a/aac, wma, flac…), tons e texto para fala (SAPI, incluindo as
//! vozes OneCore). Cada som toca numa thread própria, no dispositivo escolhido
//! (ex.: o áudio do monitor/TV no HDMI).

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{HGLOBAL, PROPERTYKEY};
use windows::Win32::Media::Audio::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::Media::Speech::*;
use windows::Win32::System::Com::StructuredStorage::{CreateStreamOnHGlobal, GetHGlobalFromStream, PROPVARIANT, PropVariantClear};
use windows::Win32::System::Com::*;
use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
use windows::Win32::System::Variant::{VT_I8, VT_LPWSTR, VT_UI4, VT_UI8};
use windows::core::{GUID, HSTRING, Interface, PCWSTR, PWSTR};

fn err(e: windows::core::Error, ctx: &str) -> String {
    format!("{ctx}: {} (0x{:08X})", e.message(), e.code().0 as u32)
}

fn co_init() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

unsafe fn take_pwstr(p: PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    let s = unsafe { p.to_string() }.unwrap_or_default();
    unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
    s
}

unsafe fn prop_string(v: &mut PROPVARIANT) -> String {
    unsafe {
        let s = if v.Anonymous.Anonymous.vt == VT_LPWSTR {
            v.Anonymous.Anonymous.Anonymous.pwszVal.to_string().unwrap_or_default()
        } else {
            String::new()
        };
        let _ = PropVariantClear(v);
        s
    }
}

unsafe fn prop_u64(v: &mut PROPVARIANT) -> Option<u64> {
    unsafe {
        let vt = v.Anonymous.Anonymous.vt;
        let r = if vt == VT_UI4 {
            Some(v.Anonymous.Anonymous.Anonymous.ulVal as u64)
        } else if vt == VT_UI8 {
            Some(v.Anonymous.Anonymous.Anonymous.uhVal)
        } else {
            None
        };
        let _ = PropVariantClear(v);
        r
    }
}

// ---------------- dispositivos ----------------

pub struct Device {
    pub id: String,
    pub name: String,
    /// Áudio de monitor/TV (HDMI ou DisplayPort).
    pub display: bool,
    pub default: bool,
}

fn enumerator() -> Result<IMMDeviceEnumerator, String> {
    co_init();
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.map_err(|e| err(e, "MMDeviceEnumerator"))
}

pub fn devices() -> Result<Vec<Device>, String> {
    let en = enumerator()?;
    unsafe {
        let default_id = en.GetDefaultAudioEndpoint(eRender, eConsole).and_then(|d| d.GetId()).map(|p| take_pwstr(p)).unwrap_or_default();
        let coll = en.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE).map_err(|e| err(e, "EnumAudioEndpoints"))?;
        let n = coll.GetCount().map_err(|e| err(e, "GetCount"))?;
        let mut out = Vec::new();
        for i in 0..n {
            let Ok(dev) = coll.Item(i) else { continue };
            let id = dev.GetId().map(|p| take_pwstr(p)).unwrap_or_default();
            let (mut name, mut display) = (String::new(), false);
            if let Ok(props) = dev.OpenPropertyStore(STGM_READ) {
                if let Ok(mut v) = props.GetValue(&PKEY_Device_FriendlyName as *const PROPERTYKEY) {
                    name = prop_string(&mut v);
                }
                if let Ok(mut v) = props.GetValue(&PKEY_AudioEndpoint_FormFactor as *const PROPERTYKEY) {
                    display = prop_u64(&mut v) == Some(DigitalAudioDisplayDevice.0 as u64);
                }
            }
            out.push(Device { default: id == default_id, id, name, display });
        }
        Ok(out)
    }
}

fn open_device(id: &str) -> Result<IMMDevice, String> {
    let en = enumerator()?;
    unsafe {
        if id.is_empty() {
            en.GetDefaultAudioEndpoint(eRender, eConsole).map_err(|e| err(e, "no default audio output"))
        } else {
            en.GetDevice(&HSTRING::from(id)).map_err(|e| err(e, &format!("audio device '{id}'")))
        }
    }
}

/// Volume do Windows para a saída (0.0..1.0) e se está sem som.
pub fn endpoint_volume(id: &str) -> Result<(f32, bool), String> {
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    unsafe {
        let dev = open_device(id)?;
        let v: IAudioEndpointVolume = dev.Activate(CLSCTX_ALL, None).map_err(|e| err(e, "IAudioEndpointVolume"))?;
        let level = v.GetMasterVolumeLevelScalar().map_err(|e| err(e, "GetMasterVolumeLevelScalar"))?;
        let mute = v.GetMute().map_err(|e| err(e, "GetMute"))?.as_bool();
        Ok((level, mute))
    }
}

/// Muda o volume do Windows da saída (level < 0: não muda) e o mudo (None: não muda).
pub fn set_endpoint_volume(id: &str, level: f32, mute: Option<bool>) -> Result<(), String> {
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    unsafe {
        let dev = open_device(id)?;
        let v: IAudioEndpointVolume = dev.Activate(CLSCTX_ALL, None).map_err(|e| err(e, "IAudioEndpointVolume"))?;
        if level >= 0.0 {
            v.SetMasterVolumeLevelScalar(level.clamp(0.0, 1.0), std::ptr::null()).map_err(|e| err(e, "SetMasterVolumeLevelScalar"))?;
        }
        if let Some(m) = mute {
            v.SetMute(m, std::ptr::null()).map_err(|e| err(e, "SetMute"))?;
        }
        Ok(())
    }
}

/// Pico do sinal que está saindo agora pela saída (0.0..1.0), depois do
/// volume do Windows: é o mesmo medidor que aparece nas configurações de som.
pub fn endpoint_peak(id: &str) -> Result<f32, String> {
    use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
    unsafe {
        let dev = open_device(id)?;
        let m: IAudioMeterInformation = dev.Activate(CLSCTX_ALL, None).map_err(|e| err(e, "IAudioMeterInformation"))?;
        m.GetPeakValue().map_err(|e| err(e, "GetPeakValue"))
    }
}

// ---------------- fontes de áudio ----------------

pub enum Spec {
    /// Amostras f32 intercaladas.
    Pcm { data: Vec<f32>, channels: u32, rate: u32 },
    File(String),
}

enum Source {
    Mem { data: Vec<f32>, channels: u32, rate: u32, pos: usize },
    Mf { reader: IMFSourceReader, channels: u32, rate: u32, pending: Vec<f32>, at: usize, duration: Option<u64> },
}

const MF_FIRST_AUDIO: u32 = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;
const MF_MEDIASOURCE: u32 = MF_SOURCE_READER_MEDIASOURCE.0 as u32;

impl Source {
    fn open(spec: Spec) -> Result<Source, String> {
        match spec {
            Spec::Pcm { data, channels, rate } => Ok(Source::Mem { data, channels, rate, pos: 0 }),
            Spec::File(path) => unsafe {
                if !std::path::Path::new(&path).exists() {
                    return Err(format!("{path}: file not found"));
                }
                let full = std::fs::canonicalize(&path).map(|p| p.to_string_lossy().trim_start_matches(r"\\?\").to_string()).unwrap_or(path.clone());
                MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(|e| err(e, "MFStartup"))?;
                let reader = MFCreateSourceReaderFromURL(&HSTRING::from(full.as_str()), None).map_err(|e| err(e, &path))?;
                let mt = MFCreateMediaType().map_err(|e| err(e, "MFCreateMediaType"))?;
                mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).map_err(|e| err(e, "SetGUID"))?;
                mt.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_Float).map_err(|e| err(e, "SetGUID"))?;
                reader.SetCurrentMediaType(MF_FIRST_AUDIO, None, &mt).map_err(|e| err(e, &format!("{path}: no decodable audio")))?;
                let cur = reader.GetCurrentMediaType(MF_FIRST_AUDIO).map_err(|e| err(e, "GetCurrentMediaType"))?;
                let channels = cur.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS).map_err(|e| err(e, "channels"))?;
                let rate = cur.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND).map_err(|e| err(e, "rate"))?;
                let duration = reader.GetPresentationAttribute(MF_MEDIASOURCE, &MF_PD_DURATION).ok().and_then(|mut v| prop_u64(&mut v));
                Ok(Source::Mf { reader, channels, rate, pending: Vec::new(), at: 0, duration })
            },
        }
    }

    fn format(&self) -> (u32, u32) {
        match self {
            Source::Mem { channels, rate, .. } | Source::Mf { channels, rate, .. } => (*channels, *rate),
        }
    }

    /// Duração em ms, se conhecida.
    fn duration_ms(&self) -> i64 {
        match self {
            Source::Mem { data, channels, rate, .. } => (data.len() as u64 / *channels as u64 * 1000 / *rate as u64) as i64,
            Source::Mf { duration, .. } => duration.map(|d| (d / 10_000) as i64).unwrap_or(-1),
        }
    }

    /// Preenche `out` (amostras intercaladas); devolve quantas amostras escreveu (0 = fim).
    fn fill(&mut self, out: &mut [f32]) -> Result<usize, String> {
        match self {
            Source::Mem { data, pos, .. } => {
                let n = out.len().min(data.len() - *pos);
                out[..n].copy_from_slice(&data[*pos..*pos + n]);
                *pos += n;
                Ok(n)
            }
            Source::Mf { reader, pending, at, .. } => {
                let mut written = 0;
                while written < out.len() {
                    if *at >= pending.len() {
                        pending.clear();
                        *at = 0;
                        let mut flags = 0u32;
                        let mut sample: Option<IMFSample> = None;
                        unsafe {
                            reader
                                .ReadSample(MF_FIRST_AUDIO, 0, None, Some(&mut flags), None, Some(&mut sample))
                                .map_err(|e| err(e, "ReadSample"))?;
                        }
                        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                            break;
                        }
                        let Some(sample) = sample else { continue };
                        unsafe {
                            let buf = sample.ConvertToContiguousBuffer().map_err(|e| err(e, "buffer"))?;
                            let mut p = std::ptr::null_mut();
                            let mut len = 0u32;
                            buf.Lock(&mut p, None, Some(&mut len)).map_err(|e| err(e, "Lock"))?;
                            let f = std::slice::from_raw_parts(p as *const f32, len as usize / 4);
                            pending.extend_from_slice(f);
                            let _ = buf.Unlock();
                        }
                        continue;
                    }
                    let n = (out.len() - written).min(pending.len() - *at);
                    out[written..written + n].copy_from_slice(&pending[*at..*at + n]);
                    written += n;
                    *at += n;
                }
                Ok(written)
            }
        }
    }

    fn rewind(&mut self) {
        match self {
            Source::Mem { pos, .. } => *pos = 0,
            Source::Mf { reader, pending, at, .. } => unsafe {
                pending.clear();
                *at = 0;
                let mut v = PROPVARIANT::default();
                (*v.Anonymous.Anonymous).vt = VT_I8;
                (*v.Anonymous.Anonymous).Anonymous.hVal = 0;
                let _ = reader.SetCurrentPosition(&GUID::zeroed(), &v);
            },
        }
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        if let Source::Mf { .. } = self {
            unsafe {
                let _ = MFShutdown();
            }
        }
    }
}

// ---------------- reprodução ----------------

pub struct State {
    pub playing: AtomicBool,
    stop: AtomicBool,
    volume: AtomicU32, // f32 em bits
    played: AtomicU64, // frames já tocados
    pub rate: AtomicU32,
    pub channels: AtomicU32,
    pub duration_ms: AtomicU64,
    pub error: Mutex<String>,
}

pub struct Sound {
    pub state: Arc<State>,
    thread: Option<JoinHandle<()>>,
}

impl Sound {
    pub fn playing(&self) -> bool {
        self.state.playing.load(Ordering::Relaxed)
    }

    pub fn position_ms(&self) -> i64 {
        let rate = self.state.rate.load(Ordering::Relaxed).max(1) as u64;
        (self.state.played.load(Ordering::Relaxed) * 1000 / rate) as i64
    }

    pub fn set_volume(&self, v: f32) {
        self.state.volume.store(v.clamp(0.0, 4.0).to_bits(), Ordering::Relaxed);
    }

    pub fn stop(&mut self) {
        self.state.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Sound {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Começa a tocar `spec` no dispositivo `device_id` ("" = saída padrão).
/// Volta assim que o áudio começou (ou com o erro de abertura).
pub fn play(device_id: &str, spec: Spec, volume: f32, looped: bool) -> Result<Sound, String> {
    let state = Arc::new(State {
        playing: AtomicBool::new(true),
        stop: AtomicBool::new(false),
        volume: AtomicU32::new(volume.clamp(0.0, 4.0).to_bits()),
        played: AtomicU64::new(0),
        rate: AtomicU32::new(0),
        channels: AtomicU32::new(0),
        duration_ms: AtomicU64::new(u64::MAX),
        error: Mutex::new(String::new()),
    });
    let (tx, rx) = mpsc::channel::<Result<(), String>>();
    let st = state.clone();
    let device_id = device_id.to_string();
    let thread = std::thread::spawn(move || {
        co_init();
        let result = run(&device_id, spec, looped, &st, &tx);
        if let Err(e) = result {
            *st.error.lock().unwrap() = e.clone();
            let _ = tx.send(Err(e));
        }
        st.playing.store(false, Ordering::Relaxed);
    });
    match rx.recv() {
        Ok(Ok(())) => Ok(Sound { state, thread: Some(thread) }),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e)
        }
        Err(_) => {
            let _ = thread.join();
            Err(state.error.lock().unwrap().clone())
        }
    }
}

fn run(device_id: &str, spec: Spec, looped: bool, st: &State, ready: &mpsc::Sender<Result<(), String>>) -> Result<(), String> {
    let mut src = Source::open(spec)?;
    let (channels, rate) = src.format();
    st.rate.store(rate, Ordering::Relaxed);
    st.channels.store(channels, Ordering::Relaxed);
    let d = src.duration_ms();
    st.duration_ms.store(if d < 0 { u64::MAX } else { d as u64 }, Ordering::Relaxed);
    unsafe {
        let dev = open_device(device_id)?;
        let client: IAudioClient = dev.Activate(CLSCTX_ALL, None).map_err(|e| err(e, "IAudioClient"))?;
        let wfx = WAVEFORMATEX {
            wFormatTag: 3, // WAVE_FORMAT_IEEE_FLOAT
            nChannels: channels as u16,
            nSamplesPerSec: rate,
            nAvgBytesPerSec: rate * channels * 4,
            nBlockAlign: (channels * 4) as u16,
            wBitsPerSample: 32,
            cbSize: 0,
        };
        client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                2_000_000, // 200 ms
                0,
                &wfx,
                None,
            )
            .map_err(|e| err(e, "IAudioClient::Initialize"))?;
        let frames = client.GetBufferSize().map_err(|e| err(e, "GetBufferSize"))?;
        let render: IAudioRenderClient = client.GetService().map_err(|e| err(e, "IAudioRenderClient"))?;
        let _ = ready.send(Ok(()));

        let ch = channels as usize;
        let mut written: u64 = 0;
        let mut ended = false;
        let mut started = false;
        let mut tmp: Vec<f32> = vec![0.0; frames as usize * ch];
        loop {
            if st.stop.load(Ordering::Relaxed) {
                break;
            }
            let padding = client.GetCurrentPadding().map_err(|e| err(e, "audio device lost"))?;
            st.played.store(written.saturating_sub(padding as u64), Ordering::Relaxed);
            let avail = (frames - padding) as usize;
            if !ended && avail > 0 {
                let want = avail * ch;
                let mut n = src.fill(&mut tmp[..want])?;
                if n < want && looped {
                    src.rewind();
                    n += src.fill(&mut tmp[n..want])?;
                }
                let nf = n / ch;
                if nf > 0 {
                    let vol = f32::from_bits(st.volume.load(Ordering::Relaxed));
                    let p = render.GetBuffer(nf as u32).map_err(|e| err(e, "GetBuffer"))? as *mut f32;
                    let dst = std::slice::from_raw_parts_mut(p, nf * ch);
                    for (d, s) in dst.iter_mut().zip(&tmp[..nf * ch]) {
                        *d = s * vol;
                    }
                    render.ReleaseBuffer(nf as u32, 0).map_err(|e| err(e, "ReleaseBuffer"))?;
                    written += nf as u64;
                }
                if nf * ch < want && !looped {
                    ended = true;
                }
                if !started {
                    client.Start().map_err(|e| err(e, "Start"))?;
                    started = true;
                }
            }
            if ended && padding == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        st.played.store(written, Ordering::Relaxed);
        let _ = client.Stop();
    }
    Ok(())
}

/// Tom senoidal (com rampa de 5 ms nas pontas para não estalar).
pub fn tone(freq: f32, ms: u32) -> Spec {
    let rate = 48_000u32;
    let n = (rate as u64 * ms as u64 / 1000) as usize;
    let ramp = (rate as usize / 200).min(n / 2).max(1);
    let data = (0..n)
        .map(|i| {
            let env = (i.min(n - 1 - i) as f32 / ramp as f32).min(1.0);
            (2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin() * 0.5 * env
        })
        .collect();
    Spec::Pcm { data, channels: 1, rate }
}

// ---------------- texto para fala (SAPI) ----------------

pub struct Voice {
    pub id: String,
    pub name: String,
    pub language: String,
}

const VOICE_CATEGORIES: [&str; 2] = [
    r"HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Speech_OneCore\Voices",
    r"HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Speech\Voices",
];

/// SPDFID_WaveFormatEx
const SPDFID_WAVEFORMATEX: GUID = GUID::from_u128(0xc31adbae_527f_4ff5_a230_f62bb61ff70c);

fn lcid_name(hex: &str) -> String {
    let first = hex.split(';').next().unwrap_or("").trim();
    match u32::from_str_radix(first, 16).unwrap_or(0) {
        0x409 => "en-US".into(),
        0x809 => "en-GB".into(),
        0x416 => "pt-BR".into(),
        0x816 => "pt-PT".into(),
        0xC0A => "es-ES".into(),
        0x80A => "es-MX".into(),
        0x40C => "fr-FR".into(),
        0x407 => "de-DE".into(),
        0x410 => "it-IT".into(),
        0x411 => "ja-JP".into(),
        0x804 => "zh-CN".into(),
        0 => String::new(),
        n => format!("lcid-{n:x}"),
    }
}

pub fn voices() -> Vec<Voice> {
    co_init();
    let mut out: Vec<Voice> = Vec::new();
    for cat_id in VOICE_CATEGORIES {
        unsafe {
            let Ok(cat) = CoCreateInstance::<_, ISpObjectTokenCategory>(&SpObjectTokenCategory, None, CLSCTX_ALL) else { continue };
            if cat.SetId(&HSTRING::from(cat_id), false).is_err() {
                continue;
            }
            let Ok(en) = cat.EnumTokens(PCWSTR::null(), PCWSTR::null()) else { continue };
            let mut n = 0u32;
            if en.GetCount(&mut n).is_err() {
                continue;
            }
            for i in 0..n {
                let Ok(tok) = en.Item(i) else { continue };
                let id = tok.GetId().map(|p| take_pwstr(p)).unwrap_or_default();
                let name = tok.GetStringValue(PCWSTR::null()).map(|p| take_pwstr(p)).unwrap_or_default();
                let language = tok
                    .OpenKey(&HSTRING::from("Attributes"))
                    .and_then(|k| k.GetStringValue(&HSTRING::from("Language")))
                    .map(|p| lcid_name(&take_pwstr(p)))
                    .unwrap_or_default();
                // a mesma voz pode estar nas duas categorias: fica a primeira (OneCore)
                let short = name.split(" - ").next().unwrap_or(&name).replace(" Desktop", "");
                if out.iter().any(|v| v.name.split(" - ").next().unwrap_or(&v.name).replace(" Desktop", "") == short) {
                    continue;
                }
                out.push(Voice { id, name, language });
            }
        }
    }
    out
}

/// Acha uma voz pelo id do token, por parte do nome ("Helia") ou pelo idioma ("pt-PT", "pt").
pub fn find_voice(q: &str) -> Result<Option<String>, String> {
    if q.is_empty() {
        return Ok(None);
    }
    if q.starts_with("HKEY_") {
        return Ok(Some(q.to_string()));
    }
    let list = voices();
    let ql = q.to_lowercase();
    if let Some(v) = list.iter().find(|v| v.name.to_lowercase().contains(&ql)) {
        return Ok(Some(v.id.clone()));
    }
    if let Some(v) = list.iter().find(|v| v.language.to_lowercase() == ql || v.language.to_lowercase().starts_with(&format!("{ql}-"))) {
        return Ok(Some(v.id.clone()));
    }
    let names: Vec<String> = list.iter().map(|v| format!("{} [{}]", v.name, v.language)).collect();
    Err(format!("voice '{q}' not found; installed: {}", names.join(", ")))
}

pub const TTS_RATE: u32 = 22_050;

/// Sintetiza `text` e devolve PCM 16 bits mono a 22050 Hz.
/// rate: -10 (lento) a 10 (rápido). xml: o texto usa marcação SAPI/SSML.
pub fn synth(text: &str, voice: &str, rate: i32, xml: bool) -> Result<Vec<i16>, String> {
    co_init();
    let voice_id = find_voice(voice)?;
    unsafe {
        let sp: ISpVoice = CoCreateInstance(&SpVoice, None, CLSCTX_ALL).map_err(|e| err(e, "SpVoice"))?;
        if let Some(id) = voice_id {
            let tok: ISpObjectToken = CoCreateInstance(&SpObjectToken, None, CLSCTX_ALL).map_err(|e| err(e, "SpObjectToken"))?;
            tok.SetId(PCWSTR::null(), &HSTRING::from(id.as_str()), false).map_err(|e| err(e, &format!("voice token {id}")))?;
            sp.SetVoice(&tok).map_err(|e| err(e, "SetVoice"))?;
        }
        sp.SetRate(rate.clamp(-10, 10)).map_err(|e| err(e, "SetRate"))?;
        let stream = CreateStreamOnHGlobal(HGLOBAL::default(), true).map_err(|e| err(e, "CreateStreamOnHGlobal"))?;
        let spstream: ISpStream = CoCreateInstance(&SpStream, None, CLSCTX_ALL).map_err(|e| err(e, "SpStream"))?;
        let wfx = WAVEFORMATEX {
            wFormatTag: 1, // PCM
            nChannels: 1,
            nSamplesPerSec: TTS_RATE,
            nAvgBytesPerSec: TTS_RATE * 2,
            nBlockAlign: 2,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        spstream.SetBaseStream(&stream, &SPDFID_WAVEFORMATEX, &wfx).map_err(|e| err(e, "SetBaseStream"))?;
        sp.SetOutput(&spstream.cast::<windows::core::IUnknown>().map_err(|e| err(e, "cast"))?, true).map_err(|e| err(e, "SetOutput"))?;
        let flags = if xml { SPF_IS_XML.0 } else { SPF_DEFAULT.0 | 16 /* SPF_IS_NOT_XML */ } as u32;
        sp.Speak(&HSTRING::from(text), flags, None).map_err(|e| err(e, "Speak"))?;
        let mut size = 0u64;
        stream.Seek(0, STREAM_SEEK_CUR, Some(&mut size)).map_err(|e| err(e, "Seek"))?;
        let hg = GetHGlobalFromStream(&stream).map_err(|e| err(e, "GetHGlobalFromStream"))?;
        let p = GlobalLock(hg) as *const i16;
        if p.is_null() {
            return Err("GlobalLock failed".into());
        }
        let samples = std::slice::from_raw_parts(p, size as usize / 2).to_vec();
        let _ = GlobalUnlock(hg);
        Ok(samples)
    }
}

/// Grava PCM 16 bits mono em .mp3 ou .m4a (AAC) com os codificadores do
/// Windows (Media Foundation). A taxa é convertida para 44100 Hz.
pub fn encode_compressed(samples: &[i16], rate: u32, path: &str) -> Result<(), String> {
    let lower = path.to_lowercase();
    let (subtype, bytes_per_sec) = if lower.ends_with(".mp3") {
        (MFAudioFormat_MP3, 16_000u32) // 128 kbit/s
    } else if lower.ends_with(".m4a") || lower.ends_with(".aac") || lower.ends_with(".mp4") {
        (MFAudioFormat_AAC, 16_000u32) // 128 kbit/s
    } else {
        return Err(format!("{path}: use .wav, .mp3 or .m4a"));
    };
    const OUT_RATE: u32 = 44_100;
    // reamostragem linear para 44100 Hz
    let n_out = (samples.len() as u64 * OUT_RATE as u64 / rate.max(1) as u64) as usize;
    let pcm: Vec<i16> = (0..n_out)
        .map(|i| {
            let p = i as f64 * rate as f64 / OUT_RATE as f64;
            let j = p as usize;
            let a = samples.get(j).copied().unwrap_or(0) as f64;
            let b = samples.get(j + 1).copied().unwrap_or(a as i16) as f64;
            (a + (b - a) * (p - j as f64)) as i16
        })
        .collect();
    co_init();
    unsafe {
        MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(|e| err(e, "MFStartup"))?;
        let result = (|| -> Result<(), String> {
            let _ = std::fs::remove_file(path);
            let writer = MFCreateSinkWriterFromURL(&HSTRING::from(path), None, None).map_err(|e| err(e, path))?;
            let out = MFCreateMediaType().map_err(|e| err(e, "MFCreateMediaType"))?;
            out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).map_err(|e| err(e, "type"))?;
            out.SetGUID(&MF_MT_SUBTYPE, &subtype).map_err(|e| err(e, "type"))?;
            out.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, OUT_RATE).map_err(|e| err(e, "type"))?;
            out.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 1).map_err(|e| err(e, "type"))?;
            if subtype == MFAudioFormat_AAC {
                // o codificador MP3 recusa este atributo na saída
                out.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16).map_err(|e| err(e, "type"))?;
            }
            out.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, bytes_per_sec).map_err(|e| err(e, "type"))?;
            let stream = writer.AddStream(&out).map_err(|e| err(e, "no encoder for this format"))?;
            let inp = MFCreateMediaType().map_err(|e| err(e, "MFCreateMediaType"))?;
            inp.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).map_err(|e| err(e, "type"))?;
            inp.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM).map_err(|e| err(e, "type"))?;
            inp.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, OUT_RATE).map_err(|e| err(e, "type"))?;
            inp.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 1).map_err(|e| err(e, "type"))?;
            inp.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16).map_err(|e| err(e, "type"))?;
            inp.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 2).map_err(|e| err(e, "type"))?;
            inp.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, OUT_RATE * 2).map_err(|e| err(e, "type"))?;
            writer.SetInputMediaType(stream, &inp, None).map_err(|e| err(e, "SetInputMediaType"))?;
            writer.BeginWriting().map_err(|e| err(e, "BeginWriting"))?;
            let chunk = OUT_RATE as usize / 10; // 100 ms por amostra
            let mut t: i64 = 0;
            for part in pcm.chunks(chunk) {
                let bytes = (part.len() * 2) as u32;
                let buf = MFCreateMemoryBuffer(bytes).map_err(|e| err(e, "MFCreateMemoryBuffer"))?;
                let mut p = std::ptr::null_mut();
                buf.Lock(&mut p, None, None).map_err(|e| err(e, "Lock"))?;
                std::ptr::copy_nonoverlapping(part.as_ptr() as *const u8, p, bytes as usize);
                let _ = buf.Unlock();
                buf.SetCurrentLength(bytes).map_err(|e| err(e, "SetCurrentLength"))?;
                let sample = MFCreateSample().map_err(|e| err(e, "MFCreateSample"))?;
                sample.AddBuffer(&buf).map_err(|e| err(e, "AddBuffer"))?;
                let dur = part.len() as i64 * 10_000_000 / OUT_RATE as i64;
                sample.SetSampleTime(t).map_err(|e| err(e, "SetSampleTime"))?;
                sample.SetSampleDuration(dur).map_err(|e| err(e, "SetSampleDuration"))?;
                writer.WriteSample(stream, &sample).map_err(|e| err(e, "WriteSample"))?;
                t += dur;
            }
            writer.Finalize().map_err(|e| err(e, "Finalize"))?;
            Ok(())
        })();
        let _ = MFShutdown();
        result
    }
}

/// Lê um arquivo de áudio inteiro (qualquer formato que o Windows decodifique)
/// e devolve (amostras f32 intercaladas, canais, taxa).
pub fn decode_file(path: &str) -> Result<(Vec<f32>, u32, u32), String> {
    co_init();
    let mut src = Source::open(Spec::File(path.to_string()))?;
    let (ch, rate) = src.format();
    let mut out = Vec::new();
    let mut buf = vec![0f32; 48_000];
    loop {
        let n = src.fill(&mut buf)?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
    }
    Ok((out, ch, rate))
}

/// WAV 16 bits PCM.
pub fn encode_wav(samples: &[i16], channels: u16, rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut b = Vec::with_capacity(44 + data_len as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&channels.to_le_bytes());
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * channels as u32 * 2).to_le_bytes());
    b.extend_from_slice(&(channels * 2).to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        b.extend_from_slice(&s.to_le_bytes());
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header() {
        let w = encode_wav(&[0, 1000, -1000], 1, 22050);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(&w[8..16], b"WAVEfmt ");
        assert_eq!(w.len(), 44 + 6);
    }

    #[test]
    fn tone_has_expected_length() {
        match tone(440.0, 100) {
            Spec::Pcm { data, channels, rate } => {
                assert_eq!((channels, rate), (1, 48_000));
                assert_eq!(data.len(), 4800);
                assert!(data.iter().all(|s| s.abs() <= 0.5));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn lists_devices_and_voices() {
        let devs = devices().unwrap();
        assert!(!devs.is_empty());
        assert!(!voices().is_empty());
    }

    #[test]
    fn synthesizes_speech() {
        let pcm = synth("test", "", 0, false).unwrap();
        assert!(pcm.len() > TTS_RATE as usize / 10, "too short: {}", pcm.len());
    }

    /// mp3 e m4a: grava com o codificador do Windows e decodifica de volta.
    #[test]
    fn compressed_roundtrip() {
        // 1 s de 440 Hz a 22050 Hz
        let pcm: Vec<i16> = (0..22_050).map(|i| ((i as f32 * 440.0 * 2.0 * std::f32::consts::PI / 22_050.0).sin() * 12_000.0) as i16).collect();
        for ext in ["mp3", "m4a"] {
            let path = std::env::temp_dir().join(format!("foguhdmi_test.{ext}"));
            let p = path.to_str().unwrap();
            encode_compressed(&pcm, 22_050, p).unwrap_or_else(|e| panic!("{ext}: {e}"));
            assert!(std::fs::metadata(p).unwrap().len() > 1000, "{ext} too small");
            let (data, ch, rate) = decode_file(p).unwrap();
            let secs = data.len() as f32 / ch as f32 / rate as f32;
            assert!((0.9..1.2).contains(&secs), "{ext}: {secs} s");
            let peak = data.iter().fold(0f32, |m, s| m.max(s.abs()));
            assert!(peak > 0.2, "{ext}: silent ({peak})");
        }
        assert!(encode_compressed(&pcm, 22_050, "x.ogg").is_err());
    }

    /// Toca de verdade (volume 0, na saída padrão) e confere o tempo.
    #[test]
    fn plays_to_the_end() {
        let t0 = std::time::Instant::now();
        let s = play("", tone(440.0, 300), 0.0, false).unwrap();
        while s.playing() {
            std::thread::sleep(Duration::from_millis(10));
        }
        let el = t0.elapsed().as_millis();
        assert!((250..1500).contains(&el), "took {el} ms");
        assert!(s.position_ms() >= 290, "position {}", s.position_ms());
        assert!(s.state.error.lock().unwrap().is_empty());
    }
}
