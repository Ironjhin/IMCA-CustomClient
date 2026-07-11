//! GPU-accelerated H.265 decoder via FFmpeg subprocess (`-hwaccel auto`).
//!
//! Spawns an external `ffmpeg` and pipes raw HEVC into stdin, parses the
//! resulting `yuv4mpegpipe` (Y4M) stream from stdout. Per-platform hwaccel
//! probing tries d3d11va/dxva2/qsv/cuda on Windows, vaapi/qsv/cuda on Linux,
//! videotoolbox on macOS. Falls back to "auto" if none enumerated.

use std::collections::{HashMap, VecDeque};
use std::io::{BufWriter, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::OnceLock;
use std::thread::JoinHandle;

use tracing::{debug, error, info, warn};

use super::YuvFrame;

/// Spawn `Command` without flashing a console window on Windows.
/// Sets `CREATE_NO_WINDOW` (0x08000000); no-op on other platforms.
#[inline]
fn no_window(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

fn ffmpeg_binary_name() -> &'static str {
    if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    }
}

fn node_ffmpeg_platform_dir() -> &'static str {
    if cfg!(all(windows, target_arch = "x86_64")) {
        "win32-x64"
    } else if cfg!(all(windows, target_arch = "x86")) {
        "win32-ia32"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "linux-x64"
    } else if cfg!(all(target_os = "linux", target_arch = "x86")) {
        "linux-ia32"
    } else if cfg!(all(target_os = "linux", target_arch = "arm")) {
        "linux-arm"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "linux-arm64"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "darwin-x64"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "darwin-arm64"
    } else {
        ""
    }
}

fn verify_ffmpeg_candidate(candidate: PathBuf) -> Option<String> {
    if !candidate.is_file() {
        return None;
    }

    let status = no_window(&mut Command::new(&candidate))
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;

    if status.success() {
        Some(candidate.to_string_lossy().into_owned())
    } else {
        None
    }
}

fn push_candidate(candidates: &mut Vec<PathBuf>, path: PathBuf) {
    if !candidates.iter().any(|existing| existing == &path) {
        candidates.push(path);
    }
}

fn push_root_ffmpeg_candidates(
    candidates: &mut Vec<PathBuf>,
    root: &std::path::Path,
    binary_name: &str,
    platform_dir: &str,
) {
    push_candidate(candidates, root.join(binary_name));
    push_candidate(candidates, root.join("ffmpeg").join(binary_name));
    push_candidate(candidates, root.join("resources").join(binary_name));
    push_candidate(
        candidates,
        root.join("resources").join("ffmpeg").join(binary_name),
    );
    push_candidate(
        candidates,
        root.join("src-tauri").join("resources").join(binary_name),
    );
    push_candidate(
        candidates,
        root.join("src-tauri")
            .join("resources")
            .join("ffmpeg")
            .join(binary_name),
    );

    if !platform_dir.is_empty() {
        push_candidate(
            candidates,
            root.join("node_modules")
                .join("@ffmpeg-installer")
                .join(platform_dir)
                .join(binary_name),
        );
        push_candidate(
            candidates,
            root.join("UDP-MQTT Server")
                .join("node_modules")
                .join("@ffmpeg-installer")
                .join(platform_dir)
                .join(binary_name),
        );
    }
}

fn collect_ffmpeg_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let binary_name = ffmpeg_binary_name();
    let platform_dir = node_ffmpeg_platform_dir();

    if let Ok(explicit_path) = std::env::var("SHARK_FFMPEG_PATH") {
        let trimmed = explicit_path.trim();
        if !trimmed.is_empty() {
            push_candidate(&mut candidates, PathBuf::from(trimmed));
        }
    }

    if let Ok(current_dir) = std::env::current_dir() {
        for root in current_dir.ancestors().take(8) {
            push_root_ffmpeg_candidates(&mut candidates, root, binary_name, platform_dir);
        }
    }

    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(exe_dir) = current_exe.parent() {
            for root in exe_dir.ancestors().take(10) {
                push_root_ffmpeg_candidates(&mut candidates, root, binary_name, platform_dir);
            }
        }
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for root in manifest_dir.ancestors().take(8) {
        push_root_ffmpeg_candidates(&mut candidates, root, binary_name, platform_dir);
    }

    candidates
}

pub(super) fn describe_ffmpeg_candidates() -> String {
    collect_ffmpeg_candidates()
        .into_iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" | ")
}

fn find_ffmpeg_uncached() -> Option<String> {
    for candidate in collect_ffmpeg_candidates() {
        if let Some(path) = verify_ffmpeg_candidate(candidate) {
            return Some(path);
        }
    }

    let names = if cfg!(windows) {
        vec!["ffmpeg.exe", "ffmpeg"]
    } else {
        vec!["ffmpeg"]
    };
    for name in &names {
        if Command::new(name)
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
        {
            return Some(name.to_string());
        }
    }

    None
}

/// Find FFmpeg binary in PATH, env override, or bundled project resources.
pub(super) fn find_ffmpeg() -> Option<String> {
    static FFMPEG_PATH_CACHE: OnceLock<Option<String>> = OnceLock::new();
    FFMPEG_PATH_CACHE.get_or_init(find_ffmpeg_uncached).clone()
}

/// GPU-accelerated H.265 decoder using FFmpeg subprocess.
///
/// Per-platform hwaccel probing order (first working one wins):
///   - Windows: d3d11va → dxva2 → qsv → cuda
///   - Linux:   vaapi  → qsv   → cuda
///   - macOS:   videotoolbox
///   - Fallback: "auto" (lets FFmpeg pick, may go to software)
///
/// Outputs **YUV4MPEG2 (Y4M)** raw YUV420P stream so the frontend WebGL
/// renderer can upload planes directly — no JPEG re-encode on the CPU.
pub(super) struct GpuDecoder {
    child: Child,
    stdin: BufWriter<std::process::ChildStdin>,
    frame_rx: Receiver<YuvFrame>,
    reader_thread: Option<JoinHandle<()>>,
    pending_frames: VecDeque<YuvFrame>,
    frame_count: u64,
    active_hwaccel: String,
}


unsafe impl Send for GpuDecoder {}

/// Preferred hwaccel list per platform. The first successfully-spawned
/// and still-alive FFmpeg process wins.
fn preferred_hwaccels() -> &'static [&'static str] {
    if cfg!(target_os = "windows") {
        // D3D11VA is the modern default on Windows; DXVA2 covers older GPUs.
        // qsv / cuda require vendor-specific drivers but give lower CPU.
        &["d3d11va", "dxva2", "qsv", "cuda", "auto"]
    } else if cfg!(target_os = "macos") {
        &["videotoolbox", "auto"]
    } else {
        // Linux: qsv first (Intel best: decode + mjpeg_qsv encode all on GPU),
        // vaapi second (Intel/AMD: decode + mjpeg_vaapi encode on GPU),
        // cuda third (NVIDIA: GPU decode, software mjpeg encode).
        &["qsv", "vaapi", "cuda", "auto"]
    }
}

/// Probe whether a specific hwaccel is enumerated by the FFmpeg binary.
/// Runs `ffmpeg -hide_banner -hwaccels` for the given binary and caches
/// the result per-binary path so switching `SHARK_FFMPEG_PATH` at runtime
/// (or across reconnects) does not return a stale listing.
fn ffmpeg_lists_hwaccel(ffmpeg: &str, name: &str) -> bool {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    static HWACCEL_LIST_CACHE: OnceLock<std::sync::Mutex<HashMap<u64, Vec<String>>>> =
        OnceLock::new();

    let mut hasher = DefaultHasher::new();
    ffmpeg.hash(&mut hasher);
    let key = hasher.finish();

    let cache = HWACCEL_LIST_CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut map = cache.lock().unwrap();

    let listing = map.entry(key).or_insert_with(|| {
        let output = no_window(&mut Command::new(ffmpeg))
            .args(["-hide_banner", "-hwaccels"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output();

        match output {
            Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(ToOwned::to_owned)
                .collect(),
            _ => Vec::new(),
        }
    });
    listing.iter().any(|line| line == name)
}

impl GpuDecoder {
    /// Try to create a GPU decoder. Returns Err if FFmpeg is not available,
    /// or if every probed hwaccel failed to start.
    pub(super) fn try_new() -> Result<Self, String> {
        let ffmpeg = find_ffmpeg().ok_or_else(|| "FFmpeg not found in PATH".to_string())?;

        let mut last_err = String::from("no hwaccel attempted");
        for hwaccel in preferred_hwaccels() {
            // "auto" is always worth trying even when not enumerated.
            if *hwaccel != "auto" && !ffmpeg_lists_hwaccel(&ffmpeg, hwaccel) {
                continue;
            }
            match Self::spawn_with_hwaccel(&ffmpeg, hwaccel) {
                Ok(decoder) => return Ok(decoder),
                Err(e) => {
                    debug!(%hwaccel, error = %e, "hwaccel attempt failed");
                    last_err = format!("{} (last hwaccel: {})", e, hwaccel);
                }
            }
        }
        Err(last_err)
    }

    fn spawn_with_hwaccel(ffmpeg: &str, hwaccel: &str) -> Result<Self, String> {
        // Optional downscale cap to control IPC bandwidth.
        //   Raw YUV420P at 1080p = 1920*1080*1.5 ≈ 3.1 MB/frame → 90 MB/s at 30 FPS.
        //   Capping at 720p cuts this by ~55% while remaining visually acceptable for
        //   a HUD overlay. User can override via env var SHARK_VIDEO_MAX_HEIGHT:
        //     - 0 or "none": disable scaling (full source resolution)
        //     - N (positive integer): cap height to N, width auto (even-aligned)
        let max_height: u32 = std::env::var("SHARK_VIDEO_MAX_HEIGHT")
            .ok()
            .and_then(|v| {
                let trimmed = v.trim().to_ascii_lowercase();
                if trimmed == "none" || trimmed == "0" {
                    Some(0)
                } else {
                    trimmed.parse::<u32>().ok()
                }
            })
            .unwrap_or(720);
        // Build: only downscale if source > max_height; never upscale.
        // Both dimensions MUST be even for yuv420p (chroma is subsampled 2x2).
        // `-2` in scale= means "auto, multiple of 2", but combined with
        // force_original_aspect_ratio it can still produce odd widths in edge
        // cases. We therefore wrap the output width in `trunc(.../2)*2` and
        // always follow up with a `pad` that rounds up to an even size.
        let scale_clause: Option<String> = if max_height > 0 {
            Some(format!(
                "scale=w='trunc(min(iw,iw*{h}/ih)/2)*2':h='trunc(min(ih,{h})/2)*2'",
                h = max_height
            ))
        } else {
            // No downscale — still enforce even dimensions just in case.
            Some("scale=w='trunc(iw/2)*2':h='trunc(ih/2)*2'".into())
        };

        // VAAPI needs an explicit render device on Linux.
        // VAAPI frames live in GPU memory — use scale_vaapi for scaling in-GPU,
        // then hwdownload + format=yuv420p to produce software YUV for Y4M output.
        let mut extra_hw_args: Vec<String> = Vec::new();
        let mut filter_chain: Vec<String> = Vec::new();
        if hwaccel == "vaapi" && cfg!(target_os = "linux") {
            let device = std::env::var("SHARK_VAAPI_DEVICE")
                .unwrap_or_else(|_| "/dev/dri/renderD128".to_string());
            extra_hw_args.push("-vaapi_device".to_string());
            extra_hw_args.push(device);

            // Optional in-GPU downscale (opt-in via SHARK_VAAPI_SCALE_HEIGHT).
            // VAAPI decodes to GPU surfaces; scale_vaapi resizes on the GPU for
            // free, then hwdownload pulls the (smaller) NV12 frame to system
            // memory. This cuts the CPU YUV→RGB→JPEG workload roughly in
            // proportion to the pixel reduction (1080p→720p ≈ −55%).
            //
            // Default OFF: the bare path (auto-download + -pix_fmt yuv420p) is
            // the proven-stable one on Intel iHD. Set e.g. =720 or =540 to test.
            let scale_h = std::env::var("SHARK_VAAPI_SCALE_HEIGHT")
                .ok()
                .and_then(|v| v.trim().parse::<u32>().ok())
                .filter(|&h| h >= 120 && h <= 2160 && h % 2 == 0)
                .unwrap_or(0);
            if scale_h > 0 {
                // Keep decoded frames on the GPU as VAAPI surfaces; otherwise
                // `-hwaccel vaapi` auto-downloads them to nv12 in system memory
                // and scale_vaapi (which needs hw surfaces) fails with
                // "Impossible to convert ... src: nv12 dst: nv12".
                extra_hw_args.push("-hwaccel_output_format".to_string());
                extra_hw_args.push("vaapi".to_string());
                // w=-2 keeps aspect ratio and forces an even width (NV12 needs it).
                // scale_vaapi resizes on-GPU, then hwdownload pulls nv12 back.
                filter_chain.push(format!("scale_vaapi=w=-2:h={}", scale_h));
                filter_chain.push("hwdownload".into());
                filter_chain.push("format=nv12".into());
            }
            // else: no filters — FFmpeg auto-downloads hw frames and the output
            // -pix_fmt yuv420p handles NV12→YUV420P conversion.
        } else if hwaccel == "qsv" {
            // Intel QSV: initialize QSV device, then download to system memory for Y4M output.
            extra_hw_args.push("-init_hw_device".to_string());
            extra_hw_args.push("qsv=hw".to_string());
            filter_chain.push("hwdownload".into());
            filter_chain.push("format=nv12".into());
            if let Some(scale) = scale_clause {
                filter_chain.push(scale);
            }
            filter_chain.push("format=yuv420p".into());
        } else if hwaccel == "cuda" {
            // NVIDIA NVDEC: download to system memory.
            filter_chain.push("hwdownload".into());
            filter_chain.push("format=nv12".into());
            if let Some(scale) = scale_clause {
                filter_chain.push(scale);
            }
            filter_chain.push("format=yuv420p".into());
        } else {
            // d3d11va / dxva2 / videotoolbox / auto: FFmpeg auto-downloads by default.
            if let Some(scale) = scale_clause {
                filter_chain.push(scale);
            }
            filter_chain.push("format=yuv420p".into());
        }
        let filter_expr = filter_chain.join(",");

        let mut cmd = Command::new(ffmpeg);
        no_window(&mut cmd);
        // NOTE: `-fflags nobuffer` breaks VAAPI HEVC decode on some Intel iHD
        // drivers — the decoder initializes (emits the Y4M header) but produces
        // zero frames ("Could not find ref with POC"). `-flags low_delay` plus
        // a tiny probesize keeps latency low without that side effect.
        if hwaccel == "vaapi" {
            cmd.args([
                "-flags",
                "low_delay",
                "-probesize",
                "32",
                "-analyzeduration",
                "0",
                "-use_wallclock_as_timestamps",
                "1",
            ]);
        } else {
            cmd.args([
                "-fflags",
                "nobuffer",
                "-flags",
                "low_delay",
                "-probesize",
                "32",
                "-analyzeduration",
                "0",
                "-use_wallclock_as_timestamps",
                "1",
            ]);
        }
        for arg in &extra_hw_args {
            cmd.arg(arg);
        }
        cmd.args([
            "-hwaccel", hwaccel, "-f", "hevc", "-i", "pipe:0", "-vsync", "0",
        ]);
        if !filter_expr.is_empty() {
            cmd.args(["-vf", &filter_expr]);
        }
        cmd.args([
            "-f",
            "yuv4mpegpipe",
            "-pix_fmt",
            "yuv420p",
            "-strict",
            "-1", // Allow odd dims (Y4M requires even for 4:2:0 but we never send odd)
            "-an",
            "-v",
            "error",
            "pipe:1",
        ]);

        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn FFmpeg: {}", e))?;

        let stdin = BufWriter::new(child.stdin.take().unwrap());
        let stdout = child.stdout.take().unwrap();
        // Pump FFmpeg stderr to the log. Classify lines: routine packet-loss /
        // reference-loss noise (expected on a lossy UDP link, self-heals at the
        // next IDR) goes to DEBUG so it does not flood; genuinely fatal problems
        // (spawn/filter/format failures) stay at WARN.
        if let Some(stderr) = child.stderr.take() {
            std::thread::Builder::new()
                .name("ffmpeg-stderr".into())
                .spawn(move || {
                    use std::io::BufRead;
                    let reader = std::io::BufReader::new(stderr);
                    for line in reader.lines().map_while(Result::ok) {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        // UDP packet loss artifacts — common and self-healing.
                        let is_loss_noise = trimmed.contains("Could not find ref")
                            || trimmed.contains("Error constructing the frame RPS")
                            || trimmed.contains("decode_slice_header error")
                            || trimmed.contains("Could not find reference")
                            || trimmed.contains("concealing")
                            || trimmed.contains("error while decoding MB")
                            || trimmed.contains("missing picture");
                        if is_loss_noise {
                            debug!(target: "ffmpeg", "{}", trimmed);
                        } else {
                            warn!(target: "ffmpeg", "{}", trimmed);
                        }
                    }
                })
                .ok();
        }
        let (frame_tx, frame_rx) = mpsc::channel();
        let reader_thread = Some(spawn_ffmpeg_y4m_reader(stdout, frame_tx));

        // Short settle delay so immediate spawn failures surface via try_wait.
        std::thread::sleep(std::time::Duration::from_millis(40));
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!("FFmpeg exited immediately (status: {})", status));
        }

        info!(
            path = %ffmpeg,
            %hwaccel,
            "FFmpeg started (output=Y4M/YUV420P)"
        );

        Ok(Self {
            child,
            stdin,
            frame_rx,
            reader_thread,
            pending_frames: VecDeque::with_capacity(8),
            frame_count: 0,
            active_hwaccel: hwaccel.to_string(),
        })
    }

    /// Which hwaccel is this instance actually using?
    #[allow(dead_code)]
    pub(super) fn active_hwaccel(&self) -> &str {
        &self.active_hwaccel
    }

    /// Feed H.265 data and try to read a decoded YUV frame.
    ///
    /// FFmpeg's `-f hevc` demuxer is one frame latent: it only emits frame N
    /// after it sees the start code of frame N+1. We therefore collect frames
    /// opportunistically with a short blocking wait so the pipeline does not
    /// stall waiting for a frame that FFmpeg has not flushed yet.
    ///
    /// Latency strategy: if FFmpeg has produced multiple frames since the last
    /// call, we DROP all but the newest. Showing stale frames from a backlog
    /// would only increase glass-to-glass latency.
    pub(super) fn decode_to_yuv(&mut self, h265_data: &[u8]) -> Result<Option<YuvFrame>, String> {
        self.collect_ready_frames();

        // Write H.265 NAL data to FFmpeg stdin
        self.stdin
            .write_all(h265_data)
            .map_err(|e| format!("FFmpeg stdin write failed: {}", e))?;
        self.stdin
            .flush()
            .map_err(|e| format!("FFmpeg stdin flush failed: {}", e))?;

        self.collect_ready_frames();
        // Drain all but newest.
        if self.pending_frames.len() > 1 {
            let drop_count = self.pending_frames.len() - 1;
            for _ in 0..drop_count {
                let _ = self.pending_frames.pop_front();
            }
        }
        Ok(self.pending_frames.pop_front())
    }

    fn collect_ready_frames(&mut self) {
        loop {
            match self.frame_rx.try_recv() {
                Ok(frame) => {
                    self.frame_count += 1;
                    self.pending_frames.push_back(frame);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
        }
    }

    /// Check if the FFmpeg process is still running.
    pub(super) fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for GpuDecoder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader_thread) = self.reader_thread.take() {
            let _ = reader_thread.join();
        }
    }
}

// ==================== Y4M Stream Parser ====================
//
// YUV4MPEG2 format (FFmpeg `-f yuv4mpegpipe`):
//   Stream header: "YUV4MPEG2 W<w> H<h> F<n>:<d> Ip A<a>:<b> C420<variant>\n"
//   Per frame:     "FRAME[ <param1> <param2> ...]\n<Y bytes><U bytes><V bytes>"
//
// The stream header is sent once. Each FRAME line ends with '\n' and is
// immediately followed by a fixed-length payload derived from W/H.

struct Y4mReaderState {
    width: u32,
    height: u32,
    frame_size: usize, // Y + U + V planar bytes per frame
    header_parsed: bool,
}

impl Y4mReaderState {
    fn new() -> Self {
        Self {
            width: 0,
            height: 0,
            frame_size: 0,
            header_parsed: false,
        }
    }
}

/// Find the index just past the next '\n' in `buf`, or None if missing.
fn find_line_end(buf: &[u8]) -> Option<usize> {
    buf.iter().position(|&b| b == b'\n').map(|i| i + 1)
}

/// Parse a YUV4MPEG2 stream header. Returns Err if the signature is wrong.
fn parse_y4m_header(line: &str) -> Result<(u32, u32), String> {
    if !line.starts_with("YUV4MPEG2 ") {
        return Err(format!(
            "not a Y4M stream: {:?}",
            line.chars().take(40).collect::<String>()
        ));
    }

    let mut w: Option<u32> = None;
    let mut h: Option<u32> = None;
    for token in line.split_ascii_whitespace().skip(1) {
        match token.as_bytes().first() {
            Some(b'W') => w = token[1..].parse::<u32>().ok(),
            Some(b'H') => h = token[1..].parse::<u32>().ok(),
            _ => {}
        }
    }

    match (w, h) {
        (Some(w), Some(h)) if w > 0 && h > 0 => Ok((w, h)),
        _ => Err(format!("invalid Y4M header: {}", line.trim())),
    }
}

fn spawn_ffmpeg_y4m_reader(mut stdout: ChildStdout, frame_tx: Sender<YuvFrame>) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut read_buf = vec![0u8; 65536];
        let mut buf: Vec<u8> = Vec::with_capacity(1024 * 1024);
        let mut state = Y4mReaderState::new();

        loop {
            match stdout.read(&mut read_buf) {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&read_buf[..n]);

                    // Parse stream header (runs once).
                    if !state.header_parsed {
                        let header_end = match find_line_end(&buf) {
                            Some(idx) => idx,
                            None => continue, // wait for more bytes
                        };
                        let header_str = match std::str::from_utf8(&buf[..header_end - 1]) {
                            Ok(s) => s,
                            Err(_) => {
                                error!("Y4M stream header is not valid UTF-8");
                                return;
                            }
                        };
                        match parse_y4m_header(header_str) {
                            Ok((w, h)) => {
                                state.width = w;
                                state.height = h;
                                state.frame_size = YuvFrame::expected_size(w, h);
                                state.header_parsed = true;
                                info!(
                                    width = w,
                                    height = h,
                                    frame_size = state.frame_size,
                                    "Y4M stream header parsed"
                                );
                            }
                            Err(e) => {
                                error!(error = %e, "Y4M header parse failed");
                                return;
                            }
                        }
                        buf.drain(..header_end);
                    }

                    // Extract as many complete frames as possible.
                    while let Some(frame_hdr_end) = find_line_end(&buf) {
                        // Expect the line to start with "FRAME".
                        if !buf.starts_with(b"FRAME") {
                            // Resync — scan forward to the next "FRAME" token.
                            if let Some(pos) = buf.windows(5).position(|w| w == b"FRAME") {
                                buf.drain(..pos);
                                continue;
                            }
                            // No FRAME token anywhere — drop everything but keep last 4 bytes
                            // in case a split "FRAM|E" straddles the chunk boundary.
                            let keep = buf.len().saturating_sub(4);
                            buf.drain(..keep);
                            break;
                        }

                        let total_needed = frame_hdr_end + state.frame_size;
                        if buf.len() < total_needed {
                            break; // wait for more payload bytes
                        }

                        let frame_data = buf[frame_hdr_end..total_needed].to_vec();
                        buf.drain(..total_needed);

                        let yuv = YuvFrame {
                            width: state.width,
                            height: state.height,
                            data: frame_data,
                        };
                        if frame_tx.send(yuv).is_err() {
                            return;
                        }
                    }
                }
                Err(_) => break,
            }
        }
    })
}

// ==================== MJPEG GPU Decoder ====================
//
// FFmpeg decodes H.265 → scales → encodes to MJPEG in one pipeline.
// Rust only forwards the pre-encoded JPEG bytes — zero CPU encoding.

/// Pre-encoded JPEG frame from FFmpeg's mjpeg encoder.
pub struct JpegFrame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// GPU decoder that outputs pre-encoded MJPEG frames (zero Rust-side encoding).
pub struct MjpegGpuDecoder {
    child: Child,
    stdin: BufWriter<std::process::ChildStdin>,
    frame_rx: Receiver<JpegFrame>,
    reader_thread: Option<JoinHandle<()>>,
    pending_frames: VecDeque<JpegFrame>,
    frame_count: u64,
    active_hwaccel: String,
}

unsafe impl Send for MjpegGpuDecoder {}

impl MjpegGpuDecoder {
    pub(super) fn try_new() -> Result<Self, String> {
        let scale_h: u32 = std::env::var("SHARK_VAAPI_SCALE_HEIGHT")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .filter(|&h| h >= 120 && h <= 2160 && h % 2 == 0)
            .unwrap_or(1080);
        Self::try_new_with_scale(scale_h)
    }

    pub(super) fn try_new_with_scale(scale_height: u32) -> Result<Self, String> {
        let ffmpeg = find_ffmpeg().ok_or_else(|| "FFmpeg not found in PATH".to_string())?;

        let mut last_err = String::from("no hwaccel attempted");
        for hwaccel in preferred_hwaccels() {
            if *hwaccel != "auto" && !ffmpeg_lists_hwaccel(&ffmpeg, hwaccel) {
                continue;
            }
            match Self::spawn_with_hwaccel(&ffmpeg, hwaccel, scale_height) {
                Ok(decoder) => return Ok(decoder),
                Err(e) => {
                    debug!(%hwaccel, error = %e, "mjpeg hwaccel attempt failed");
                    last_err = format!("{} (last hwaccel: {})", e, hwaccel);
                }
            }
        }
        Err(last_err)
    }

    fn spawn_with_hwaccel(ffmpeg: &str, hwaccel: &str, scale_h: u32) -> Result<Self, String> {

        let jpeg_quality: u32 = std::env::var("SHARK_JPEG_QUALITY")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .filter(|&q| q >= 1 && q <= 100)
            .unwrap_or(50);

        // Select MJPEG encoder based on hwaccel type:
        //   vaapi → mjpeg_vaapi (Intel/AMD on Linux, GPU encode)
        //   qsv   → mjpeg_qsv  (Intel QSV, GPU encode)
        //   cuda  → mjpeg      (NVIDIA: GPU decode only, software encode)
        //   other → mjpeg      (software encode)
        let mjpeg_codec = match hwaccel {
            "vaapi" if cfg!(target_os = "linux") => "mjpeg_vaapi",
            "qsv" => "mjpeg_qsv",
            _ => "mjpeg",
        };

        let mut extra_hw_args: Vec<String> = Vec::new();
        let mut filter_chain: Vec<String> = Vec::new();

        // Per-hwaccel device init + filter chain.
        // When using a hardware MJPEG encoder, keep frames in GPU surfaces.
        // When falling back to software mjpeg, download to system memory.
        let use_hw_encoder = mjpeg_codec != "mjpeg";

        match hwaccel {
            "vaapi" if cfg!(target_os = "linux") => {
                let device = std::env::var("SHARK_VAAPI_DEVICE")
                    .unwrap_or_else(|_| "/dev/dri/renderD128".to_string());
                extra_hw_args.push("-vaapi_device".to_string());
                extra_hw_args.push(device);
                if use_hw_encoder {
                    // Keep as VAAPI surfaces for mjpeg_vaapi encoder.
                    extra_hw_args.push("-hwaccel_output_format".to_string());
                    extra_hw_args.push("vaapi".to_string());
                    if scale_h > 0 {
                        filter_chain.push(format!("scale_vaapi=w=-2:h={}", scale_h));
                    }
                } else {
                    // Software fallback: download to NV12.
                    filter_chain.push("hwdownload".into());
                    filter_chain.push("format=nv12".into());
                    if scale_h > 0 {
                        filter_chain.push(format!("scale=w=-2:h={}", scale_h));
                    }
                    filter_chain.push("format=yuvj420p".into());
                }
            }
            "qsv" => {
                if use_hw_encoder {
                    // Keep as QSV surfaces for mjpeg_qsv encoder.
                    extra_hw_args.push("-init_hw_device".to_string());
                    extra_hw_args.push("qsv=hw".to_string());
                    extra_hw_args.push("-hwaccel_output_format".to_string());
                    extra_hw_args.push("qsv".to_string());
                    extra_hw_args.push("-filter_hw_device".to_string());
                    extra_hw_args.push("hw".to_string());
                    if scale_h > 0 {
                        filter_chain.push(format!("scale_qsv=w=-2:h={}", scale_h));
                    }
                } else {
                    filter_chain.push("hwdownload".into());
                    filter_chain.push("format=nv12".into());
                    if scale_h > 0 {
                        filter_chain.push(format!("scale=w=-2:h={}", scale_h));
                    }
                    filter_chain.push("format=yuvj420p".into());
                }
            }
            "cuda" => {
                // NVIDIA: CUDA decode → download → software encode.
                filter_chain.push("hwdownload".into());
                filter_chain.push("format=nv12".into());
                if scale_h > 0 {
                    filter_chain.push(format!("scale=w=-2:h={}", scale_h));
                }
                filter_chain.push("format=yuvj420p".into());
            }
            _ => {
                // d3d11va / dxva2 / videotoolbox / auto: download + software encode.
                filter_chain.push("hwdownload".into());
                filter_chain.push("format=nv12".into());
                if scale_h > 0 {
                    filter_chain.push(format!("scale=w=-2:h={}", scale_h));
                }
                filter_chain.push("format=yuvj420p".into());
            }
        }

        let filter_expr = filter_chain.join(",");

        let mut cmd = Command::new(ffmpeg);
        no_window(&mut cmd);

        // Low-latency flags. VAAPI needs special handling (-fflags nobuffer
        // breaks some Intel iHD drivers); others use nobuffer.
        if hwaccel == "vaapi" {
            cmd.args(["-flags", "low_delay", "-probesize", "32", "-analyzeduration", "0"]);
        } else {
            cmd.args(["-fflags", "nobuffer", "-flags", "low_delay", "-probesize", "32", "-analyzeduration", "0"]);
        }

        for arg in &extra_hw_args {
            cmd.arg(arg);
        }
        cmd.args(["-hwaccel", hwaccel, "-f", "hevc", "-i", "pipe:0", "-vsync", "0"]);
        if !filter_expr.is_empty() {
            cmd.args(["-vf", &filter_expr]);
        }
        cmd.args([
            "-c:v", mjpeg_codec,
            "-qmin", "1", "-q:v", &jpeg_quality.to_string(),
            "-f", "rawvideo",
            "-pix_fmt", "yuvj420p",
            "-an", "-v", "error",
            "pipe:1",
        ]);

        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn FFmpeg mjpeg: {}", e))?;

        let stdin = BufWriter::new(child.stdin.take().unwrap());
        let stdout = child.stdout.take().unwrap();

        if let Some(stderr) = child.stderr.take() {
            std::thread::Builder::new()
                .name("ffmpeg-mjpeg-stderr".into())
                .spawn(move || {
                    use std::io::BufRead;
                    let reader = std::io::BufReader::new(stderr);
                    for line in reader.lines().map_while(Result::ok) {
                        let trimmed = line.trim();
                        if trimmed.is_empty() { continue; }
                        let is_loss_noise = trimmed.contains("Could not find ref")
                            || trimmed.contains("Error constructing the frame RPS")
                            || trimmed.contains("decode_slice_header error")
                            || trimmed.contains("Could not find reference")
                            || trimmed.contains("concealing")
                            || trimmed.contains("error while decoding MB")
                            || trimmed.contains("missing picture");
                        if is_loss_noise {
                            debug!(target: "ffmpeg", "{}", trimmed);
                        } else {
                            warn!(target: "ffmpeg", "{}", trimmed);
                        }
                    }
                })
                .ok();
        }

        let (frame_tx, frame_rx) = mpsc::channel();
        let reader_thread = Some(spawn_mjpeg_reader(stdout, frame_tx));

        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Ok(Some(status)) = child.try_wait() {
            // Hardware encoder failed — return error so probe loop tries next hwaccel.
            if use_hw_encoder {
                return Err(format!("{} exited immediately ({}), try next", mjpeg_codec, status));
            }
            return Err(format!("FFmpeg mjpeg exited immediately (status: {})", status));
        }

        info!(path = %ffmpeg, %hwaccel, codec = %mjpeg_codec, "FFmpeg MJPEG encoder started");

        Ok(Self {
            child,
            stdin,
            frame_rx,
            reader_thread,
            pending_frames: VecDeque::with_capacity(8),
            frame_count: 0,
            active_hwaccel: hwaccel.to_string(),
        })
    }

    pub(super) fn active_hwaccel(&self) -> &str {
        &self.active_hwaccel
    }

    pub(super) fn decode_to_jpeg(&mut self, h265_data: &[u8]) -> Result<Option<JpegFrame>, String> {
        self.collect_ready_frames();
        self.stdin.write_all(h265_data)
            .map_err(|e| format!("FFmpeg stdin write failed: {}", e))?;
        self.stdin.flush()
            .map_err(|e| format!("FFmpeg stdin flush failed: {}", e))?;
        self.collect_ready_frames();
        // Drop all but newest.
        if self.pending_frames.len() > 1 {
            let drop_count = self.pending_frames.len() - 1;
            for _ in 0..drop_count {
                let _ = self.pending_frames.pop_front();
            }
        }
        Ok(self.pending_frames.pop_front())
    }

    fn collect_ready_frames(&mut self) {
        loop {
            match self.frame_rx.try_recv() {
                Ok(frame) => {
                    self.frame_count += 1;
                    self.pending_frames.push_back(frame);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
        }
    }

    pub(super) fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for MjpegGpuDecoder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader_thread) = self.reader_thread.take() {
            let _ = reader_thread.join();
        }
    }
}

/// Read raw JPEG frames from FFmpeg's rawvideo MJPEG output.
///
/// FFmpeg's `-f rawvideo` with mjpeg codec outputs raw JPEG frames back-to-back.
/// We split by scanning for JPEG SOI (FF D8) and EOI (FF D9) markers.
fn spawn_mjpeg_reader(mut stdout: ChildStdout, frame_tx: Sender<JpegFrame>) -> JoinHandle<()> {
    std::thread::spawn(move || {
        use std::io::Read;
        let mut read_buf = vec![0u8; 65536];
        let mut buf: Vec<u8> = Vec::with_capacity(256 * 1024);
        let mut width: u32 = 0;
        let mut height: u32 = 0;

        loop {
            match stdout.read(&mut read_buf) {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&read_buf[..n]);

                    // Extract complete JPEG frames by scanning for SOI/EOI markers.
                    loop {
                        // Find SOI marker (FF D8).
                        let soi = match find_jpeg_soi(&buf) {
                            Some(pos) => pos,
                            None => break,
                        };
                        // Search for EOI marker (FF D8) after SOI.
                        let eoi = match find_jpeg_eoi(&buf, soi + 2) {
                            Some(pos) => pos,
                            None => break, // incomplete frame
                        };

                        let jpeg_data = buf[soi..eoi + 2].to_vec();
                        buf.drain(..eoi + 2);

                        // Parse dimensions from SOF if not yet known.
                        if width == 0 {
                            if let Some((w, h)) = parse_jpeg_dimensions(&jpeg_data) {
                                width = w;
                                height = h;
                                info!(width = w, height = h, "MJPEG frame dimensions detected");
                            }
                        }

                        if width > 0 && height > 0 {
                            let frame = JpegFrame {
                                data: jpeg_data,
                                width,
                                height,
                            };
                            if frame_tx.send(frame).is_err() {
                                return;
                            }
                        }
                    }
                }
                Err(_) => break,
            }
        }
    })
}

fn find_jpeg_soi(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == [0xFF, 0xD8])
}

fn find_jpeg_eoi(buf: &[u8], start: usize) -> Option<usize> {
    if start + 2 > buf.len() {
        return None;
    }
    buf[start..].windows(2).position(|w| w == [0xFF, 0xD9]).map(|p| p + start)
}

/// Parse JPEG dimensions from SOF0/SOF2 marker.
fn parse_jpeg_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    let mut i = 0;
    while i + 4 < data.len() {
        if data[i] == 0xFF && (data[i + 1] == 0xC0 || data[i + 1] == 0xC2) {
            // SOF marker: skip marker(2) + length(2) + precision(1), then height(2) + width(2)
            if i + 9 < data.len() {
                let h = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
                let w = u16::from_be_bytes([data[i + 7], data[i + 8]]) as u32;
                if w > 0 && h > 0 {
                    return Some((w, h));
                }
            }
        }
        i += 1;
    }
    None
}
