#![windows_subsystem = "windows"]

use eframe::egui;
mod vcam;
mod modern;
mod modes;
mod watch;
mod web;
use std::{
    io::Read,
    net::Ipv4Addr,
    os::windows::process::CommandExt,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

const WIDTH: usize = 800;
const HEIGHT: usize = 450;

fn hidden(program: &str) -> Command {
    let mut command = Command::new(program);
    command.creation_flags(0x08000000);
    command
}
fn webcam_get(ip: Ipv4Addr, command: &str) -> Result<String, String> {
    let url = format!("http://{ip}:8080/gopro/webcam/{command}");
    let response = hidden("curl.exe")
        .args([
            "--noproxy",
            "*",
            "--silent",
            "--show-error",
            "--fail",
            "--max-time",
            "5",
            &url,
        ])
        .output()
        .map_err(|error| format!("Impossible de lancer curl : {error}"))?;
    if !response.status.success() {
        return Err(String::from_utf8_lossy(&response.stderr).trim().to_owned());
    }
    Ok(String::from_utf8_lossy(&response.stdout).into_owned())
}

fn camera(ip: Ipv4Addr, command: &str) -> Result<String, String> {
    let _ownership = if command.starts_with("start") || command == "stop" {
        Some(watch::lock()?)
    } else {
        None
    };
    if command.starts_with("start") {
        watch::claim(ip, command)?;
    }

    let body = webcam_get(ip, command)?;
    let error = body.split_once("\"error\"").and_then(|(_, value)| {
        let (_, value) = value.split_once(':')?;
        value
            .trim_start()
            .split(|c: char| !c.is_ascii_digit())
            .next()?
            .parse::<u32>()
            .ok()
    });
    if error == Some(0) {
        Ok(body)
    } else {
        Err(format!("Réponse de la caméra : {body}"))
    }
}

#[derive(Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
struct ImageSettings {
    brightness: f32,
    contrast: f32,
    saturation: f32,
    gamma: f32,
    sharpness: f32,
    zoom: f32,
    mirror: bool,
    flip: bool,
}
impl Default for ImageSettings {
    fn default() -> Self {
        Self {
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
            gamma: 1.0,
            sharpness: 0.0,
            zoom: 1.0,
            mirror: false,
            flip: false,
        }
    }
}

fn filters(_settings: ImageSettings, fps: u32, output: u32, native_fps: u32) -> String {
    let mut graph = vec!["eq@image=brightness=0:contrast=1:saturation=1:gamma=1".to_string()];
    if fps < native_fps {
        graph.push(format!("fps={fps}"));
    }
    if fps>native_fps&&output>1080 {graph.push(format!("framerate=fps={fps}:flags=0"));}
    graph.push("crop@zoom=w=iw:h=ih".into());
    let size = match output {
        240 => Some((426, 240)),
        360 => Some((640, 360)),
        480 => Some((854, 480)),
        720 => Some((1280, 720)),
        1080 => Some((1920, 1080)),
        1440 => Some((2560, 1440)),
        2160 => Some((3840, 2160)),
        _ => None,
    };
    if let Some((w, h)) = size {
        graph.push(format!("scale={w}:{h}:flags=bilinear"));
    }
    if fps>native_fps&&output<=1080 {graph.push(format!("framerate=fps={fps}:flags=0"));}
    graph.push("cas@sharp=strength=0:enable=0".into());
    graph.push("hflip@mirror=enable=0".into());
    graph.push("vflip@flip=enable=0".into());
    graph.join(",")
}

#[derive(Default)]
struct Shared {
    frame: Option<Vec<u8>>,
    status: String,
    finished: bool,
    fps: f32,
}

#[derive(Clone, PartialEq)]
struct Processing {
    filter: String,
    dimensions: (usize, usize),
}

#[derive(Clone)]
struct Session {
    shared: Arc<Mutex<Shared>>,
    child: Arc<Mutex<Option<Child>>>,
    processing: Arc<Mutex<Processing>>,
    image: Arc<Mutex<ImageSettings>>,
    cancel: Arc<AtomicBool>,
}

impl Session {
    fn update_image(&self, image: ImageSettings) {
        *self.image.lock().unwrap() = image;
    }

    fn update_processing(&self, filter: String, dimensions: (usize, usize)) {
        let next = Processing { filter, dimensions };
        let mut current = self.processing.lock().unwrap();
        if *current == next {
            return;
        }
        *current = next;
        drop(current);
        self.shared.lock().unwrap().fps = 0.0;
        if let Some(child) = self.child.lock().unwrap().as_mut() {
            let _ = child.kill();
        }
    }

    fn stop(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(child) = self.child.lock().unwrap().as_mut() {
            let _ = child.kill();
        }
    }
    fn run(
        source: Option<(Ipv4Addr, u32, u32)>,
        filter: String,
        gpu: Option<u32>,
        target: Option<vcam::Target>,
    ) -> Self {
        let dimensions = target
            .as_ref()
            .map_or((WIDTH, HEIGHT), |target| target.dimensions);
        let session = Self {
            shared: Arc::new(Mutex::new(Shared {
                status: "Connexion à la GoPro…".into(),
                ..Default::default()
            })),
            child: Arc::new(Mutex::new(None)),
            image: Arc::new(Mutex::new(ImageSettings::default())),
            processing: Arc::new(Mutex::new(Processing {
                filter: filter.clone(),
                dimensions,
            })),
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let worker = session.clone();
        thread::spawn(move || {
            let result = worker.receive(source, &filter, gpu, target);
            let mut shared = worker.shared.lock().unwrap();
            shared.status = if worker.cancel.load(Ordering::Relaxed) {
                "Aperçu arrêté".into()
            } else {
                result.err().unwrap_or_else(|| "Flux terminé".into())
            };
            shared.finished = true;
        });
        session
    }
    fn receive(
        &self,
        source: Option<(Ipv4Addr, u32, u32)>,
        _filter: &str,
        gpu: Option<u32>,
        target: Option<vcam::Target>,
    ) -> Result<(), String> {
        let delivery = target.clone().map(vcam::Delivery::start);
        let modern_delivery=target.clone().filter(|_|modern::ready()).map(vcam::Delivery::start_modern);
        let port = target.as_ref().map_or(8554, |t| 8554 + t.slot);
        let mut camera_started = false;
        let mut outcome = Ok(());
        while !self.cancel.load(Ordering::Relaxed) {
            let config = self.processing.lock().unwrap().clone();
            let (width, height) = config.dimensions;
            let binding = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
            let control_port = binding.local_addr().map_err(|e| e.to_string())?.port();
            drop(binding);
            let mut command = hidden("ffmpeg.exe");
            command.args([
                "-hide_banner",
                "-nostdin",
                "-loglevel",
                "error",
                "-probesize",
                "262144",
                "-analyzeduration",
                "500000",
                "-flags",
                "low_delay",
            ]);
            if gpu.is_none() && source.is_some() {
                command.args(["-hwaccel", "auto"]);
            }
            if let Some(adapter) = gpu.filter(|id| *id != u32::MAX && source.is_some()) {
                command.args([
                    "-hwaccel",
                    "d3d11va",
                    "-hwaccel_device",
                    &adapter.to_string(),
                ]);
            }
            if let Some((ip, _, _)) = source {
                command.args(["-i",&format!("udp://0.0.0.0:{port}?timeout=7000000&fifo_size=512&overrun_nonfatal=1&sources={ip}")]);
            } else {
                command.args([
                    "-re",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=1920x1080:rate=30000/1001",
                ]);
            }
            let graph = format!(
                r"zmq=b='tcp\://127.0.0.1\:{control_port}',{},setsar=1",
                if config.filter.contains(&format!("scale={width}:{height}:")){config.filter.clone()}else{format!("{},scale={width}:{height}:flags=bilinear",config.filter)}
            );
            command.args([
                "-an",
                "-filter_threads",
                "4",
                "-vf",
                &graph,
                "-pix_fmt",
                "rgba",
                "-threads",
                "1",
                "-flush_packets",
                "1",
                "-f",
                "rawvideo",
                "pipe:1",
            ]);
            let mut child = command
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| e.to_string())?;
            let mut pixels_out = child.stdout.take().unwrap();
            let mut stderr = child.stderr.take().unwrap();
            {
                let mut decoder = self.child.lock().unwrap();
                *decoder = Some(child);
                if *self.processing.lock().unwrap() != config {
                    if let Some(child) = decoder.as_mut() {
                        let _ = child.kill();
                    }
                }
            }
            let logs = thread::spawn(move || {
                let mut log = String::new();
                let _ = stderr.read_to_string(&mut log);
                log
            });
            if !camera_started {
                if let Some((ip, res, lens)) = source {
                    match camera(
                        ip,
                        &format!("start?res={res}&fov={lens}&protocol=TS&port={port}"),
                    ) {
                        Ok(_) => camera_started = true,
                        Err(error) => {
                            outcome = Err(error);
                            self.stop();
                        }
                    }
                }
            }
            let control_done = Arc::new(AtomicBool::new(false));
            let control_stop = control_done.clone();
            let image = self.image.clone();
            let control = thread::spawn(move || live_controls(control_port, image, control_stop));
            let mut clock = Instant::now();
            let mut count = 0;
            let mut preview_clock=Instant::now()-Duration::from_secs(1);
            let mut pixels = vec![0; width * height * 4];
            while !self.cancel.load(Ordering::Relaxed) && pixels_out.read_exact(&mut pixels).is_ok()
            {
                if let Some(delivery) = &delivery {delivery.frame(&pixels,width,height);}
                if let Some(delivery)=&modern_delivery{delivery.frame(&pixels,width,height);}
                count += 1;
                let update_fps=clock.elapsed().as_secs_f32()>=1.0;
                if preview_clock.elapsed()<Duration::from_millis(16)&&!update_fps{continue;}
                let mut shared = self.shared.lock().unwrap();
                // The GUI only consumes the latest image and never queues old previews.
                let preview = shared
                    .frame
                    .get_or_insert_with(|| vec![0; WIDTH * HEIGHT * 4]);
                if preview_clock.elapsed()>=Duration::from_millis(16) {
                for y in 0..HEIGHT {
                    for x in 0..WIDTH {
                        let from = ((y * height / HEIGHT) * width + x * width / WIDTH) * 4;
                        let to = (y * WIDTH + x) * 4;
                        preview[to..to + 4].copy_from_slice(&pixels[from..from + 4]);
                    }
                }
                preview_clock=Instant::now();
                }
                shared.status = if source.is_some() {
                    "En direct"
                } else {
                    "Signal de test"
                }
                .into();
                if update_fps {
                    shared.fps = count as f32 / clock.elapsed().as_secs_f32();
                    count = 0;
                    clock = Instant::now();
                }
            }
            control_done.store(true, Ordering::Relaxed);
            if let Some(mut child) = self.child.lock().unwrap().take() {
                let _ = child.kill();
                let _ = child.wait();
            }
            let _ = control.join();
            let error = logs.join().unwrap_or_default();
            if *self.processing.lock().unwrap() == config {
                if !self.cancel.load(Ordering::Relaxed) && outcome.is_ok() {
                    outcome = Err(if error.is_empty() {
                        "Le flux vidéo s’est interrompu".into()
                    } else {
                        error
                    });
                }
                break;
            }
        }
        if camera_started {
            if let Some((ip, _, _)) = source {
                let _ = camera(ip, "stop");
            }
        }
        outcome
    }
}

fn live_controls(port: u16, image: Arc<Mutex<ImageSettings>>, stop: Arc<AtomicBool>) {
    use zeromq::{Socket, SocketRecv, SocketSend};
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    runtime.block_on(async move {
        let mut previous = None::<ImageSettings>;
        let mut socket = zeromq::ReqSocket::new();
        loop {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            if matches!(
                tokio::time::timeout(
                    Duration::from_millis(500),
                    socket.connect(&format!("tcp://127.0.0.1:{port}"))
                )
                .await,
                Ok(Ok(_))
            ) {
                break;
            }
            socket = zeromq::ReqSocket::new();
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        while !stop.load(Ordering::Relaxed) {
            let next = *image.lock().unwrap();
            if previous != Some(next) {
                let mut commands = Vec::<(&str, &str, String)>::new();
                let changes = [
                    (
                        "brightness",
                        next.brightness,
                        previous.map(|p| p.brightness),
                    ),
                    ("contrast", next.contrast, previous.map(|p| p.contrast)),
                    (
                        "saturation",
                        next.saturation,
                        previous.map(|p| p.saturation),
                    ),
                    ("gamma", next.gamma, previous.map(|p| p.gamma)),
                ];
                for (name, value, old) in changes {
                    if old != Some(value) {
                        commands.push(("eq@image", name, format!("{value:.3}")));
                    }
                }
                if previous.is_none_or(|p| p.zoom != next.zoom) {
                    commands.push(("crop@zoom", "w", format!("trunc(iw/{:.3}/2)*2", next.zoom)));
                    commands.push(("crop@zoom", "h", format!("trunc(ih/{:.3}/2)*2", next.zoom)));
                }
                if previous.is_none_or(|p| p.sharpness != next.sharpness) {
                    commands.push(("cas@sharp", "strength", format!("{:.3}", next.sharpness)));
                    commands.push((
                        "cas@sharp",
                        "enable",
                        u8::from(next.sharpness > 0.0).to_string(),
                    ));
                }
                if previous.is_none_or(|p| p.mirror != next.mirror) {
                    commands.push(("hflip@mirror", "enable", u8::from(next.mirror).to_string()));
                }
                if previous.is_none_or(|p| p.flip != next.flip) {
                    commands.push(("vflip@flip", "enable", u8::from(next.flip).to_string()));
                }
                let mut succeeded = true;
                for (target, option, value) in commands {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    let request = format!("{target} {option} {value}");
                    let result = tokio::time::timeout(Duration::from_millis(300), async {
                        socket.send(request.into()).await?;
                        socket.recv().await
                    })
                    .await;
                    match result {
                        Ok(Ok(response)) => {
                            let text = String::try_from(response).unwrap_or_default();
                            if !text.starts_with("0 ") {
                                #[cfg(test)]
                                eprintln!("FFmpeg control reply: {text}");
                                succeeded = false;
                            }
                        }
                        _ => {
                            socket = zeromq::ReqSocket::new();
                            if !matches!(
                                tokio::time::timeout(
                                    Duration::from_millis(300),
                                    socket.connect(&format!("tcp://127.0.0.1:{port}"))
                                )
                                .await,
                                Ok(Ok(_))
                            ) {
                                return;
                            }
                            succeeded = false;
                            break;
                        }
                    }
                }
                if succeeded {
                    previous = Some(next);
                }
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    });
}

const LANGUAGES: [&str; 6] = [
    "Français",
    "English",
    "Español",
    "日本語",
    "Русский",
    "Deutsch",
];

fn height(res: u32) -> u32 {
    match res {
        7 => 720,
        12 => 1080,
        _ => res,
    }
}
fn resolution_label(res: u32, lang: usize, modes: &[Mode]) -> String {
    let res = height(res);
    let name = match res {
        360 => "360p",
        480 => "480p",
        720 => "720p",
        1080 => "1080p HD",
        1440 => "1440p 2K",
        _ => "2160p 4K",
    };
    if modes.iter().any(|m| m.3 == res) {
        return name.into();
    }
    let max = modes.iter().map(|m| m.3).max().unwrap_or(1080);
    let suffix = if res > max {
        generated(lang)
    } else if !modes.is_empty() || res < 720 {
        tr(
            lang,
            [
                "redimensionné",
                "resized",
                "redimensionado",
                "リサイズ",
                "масштабировано",
                "skaliert",
            ],
        )
    } else {
        return name.into();
    };
    format!("{name} · {suffix}")
}
fn generated(lang: usize) -> &'static str {
    tr(
        lang,
        [
            "généré",
            "generated",
            "generado",
            "生成",
            "сгенерировано",
            "generiert",
        ],
    )
}
fn fps_label(fps: u32, lang: usize, native: u32) -> String {
    if fps > native {
        format!("{fps} FPS · {}", generated(lang))
    } else {
        format!("{fps} FPS")
    }
}
fn tr(lang: usize, labels: [&'static str; 6]) -> &'static str {
    labels[lang]
}

fn discover() -> Vec<Ipv4Addr> {
    let script = r#"Get-CimInstance Win32_NetworkAdapter | Where-Object {
        $_.PNPDeviceID -like 'USB\VID_2672*' -and $_.InterfaceIndex
    } | ForEach-Object {
        Get-NetIPAddress -InterfaceIndex $_.InterfaceIndex -AddressFamily IPv4 -ErrorAction SilentlyContinue
    } | Select-Object -ExpandProperty IPAddress"#;
    let Ok(output) = hidden("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
    else {
        return Vec::new();
    };
    let mut cameras = Vec::new();
    for address in String::from_utf8_lossy(&output.stdout).lines() {
        let Ok(ip) = address.trim().parse::<Ipv4Addr>() else {
            continue;
        };
        let [a, b, c, _] = ip.octets();
        if a != 172 || !(16..=31).contains(&b) {
            continue;
        }
        let candidate = Ipv4Addr::new(a, b, c, 51);
        if !cameras.contains(&candidate) && camera(candidate, "status").is_ok() {
            cameras.push(candidate);
        }
    }
    cameras
}

type Mode = (u32, u32, u32, u32, String);

fn preferred_mode(modes: &[Mode], output: u32, lens: u32) -> Option<&Mode> {
    let target = height(output);
    let wanted = if modes.iter().any(|m| m.3 == target) {
        target
    } else {
        modes.iter().map(|m| m.3).max()?
    };
    modes
        .iter()
        .find(|m| m.3 == wanted && m.1 == lens)
        .or_else(|| modes.iter().find(|m| m.3 == wanted))
}
fn gpu_adapters() -> Vec<String> {
    (0..8)
        .map_while(|id| {
            let out = hidden("ffmpeg.exe")
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "verbose",
                    "-init_hw_device",
                    &format!("d3d11va=probe:{id}"),
                    "-f",
                    "lavfi",
                    "-i",
                    "color=s=32x32",
                    "-frames:v",
                    "1",
                    "-f",
                    "null",
                    "NUL",
                ])
                .output()
                .ok()?;
            if !out.status.success() {
                return None;
            }
            let log = String::from_utf8_lossy(&out.stderr);
            let entry = log.lines().find(|line| line.contains("Using device"))?;
            Some(entry.split_once(" (")?.1.trim_end_matches(").").to_owned())
        })
        .collect()
}

fn probe_modes(ip: Ipv4Addr, cancel: &AtomicBool) -> Vec<Mode> {
    let mut modes = Vec::new();
    let Ok(status) = camera(ip, "status") else {
        return modes;
    };
    let status: String = status.chars().filter(|c| !c.is_whitespace()).collect();
    if !status.contains("\"status\":0") && !status.contains("\"status\":1") {
        return modes;
    }
    const ENDPOINT: &str = "udp://0.0.0.0:8554?timeout=2500000&fifo_size=512&overrun_nonfatal=1";
    for res in [12, 7, 4] {
        for lens in [0, 4, 2, 3] {
            if cancel.load(Ordering::Relaxed) {
                return modes;
            }
            let Ok(mut probe) = hidden("ffmpeg.exe")
                .args([
                    "-hide_banner",
                    "-nostdin",
                    "-loglevel",
                    "info",
                    "-probesize",
                    "128k",
                    "-analyzeduration",
                    "250000",
                    "-i",
                    &format!("{ENDPOINT}&sources={ip}"),
                    "-map",
                    "0:v:0",
                    "-frames:v",
                    "1",
                    "-vf",
                    "scale=16:16,format=gray",
                    "-f",
                    "rawvideo",
                    "pipe:1",
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
            else {
                return modes;
            };
            let result = if camera(ip, &format!("start?res={res}&fov={lens}&protocol=TS")).is_ok() {
                let deadline = Instant::now() + Duration::from_secs(4);
                while matches!(probe.try_wait(), Ok(None))
                    && Instant::now() < deadline
                    && !cancel.load(Ordering::Relaxed)
                {
                    thread::sleep(Duration::from_millis(20));
                }
                if matches!(probe.try_wait(), Ok(None)) {
                    let _ = probe.kill();
                }
                let out = probe.wait_with_output().ok();
                let _ = camera(ip, "stop");
                out
            } else {
                let _ = probe.kill();
                let _ = probe.wait();
                None
            };
            let Some(output) = result else { continue };
            if !output.status.success() || output.stdout.len() != 256 {
                continue;
            }
            let log = String::from_utf8_lossy(&output.stderr);
            let metadata = log
                .lines()
                .find(|line| line.contains("Video:") && line.contains(" fps"));
            if let Some(metadata) = metadata {
                let dimensions = metadata
                    .split(|c: char| !(c.is_ascii_digit() || c == 'x'))
                    .find_map(|item| {
                        let (w, h) = item.split_once('x')?;
                        let (w, h): (u32, u32) = (w.parse().ok()?, h.parse().ok()?);
                        (w >= 320 && h >= 240).then_some((w, h))
                    });
                let rate = metadata
                    .split(" fps")
                    .next()
                    .and_then(|prefix| prefix.split_whitespace().last());
                if let (Some((w, h)), Some(rate)) = (dimensions, rate) {
                    modes.push((res, lens, w, h, rate.to_owned()));
                }
            }
        }
    }
    modes
}

fn usage() -> String {
    let script = r#"$cpu=(Get-CimInstance Win32_PerfFormattedData_PerfOS_Processor -Filter "Name='_Total'").PercentProcessorTime
    "CPU $cpu %"
    Get-CimInstance Win32_PerfFormattedData_GPUPerformanceCounters_GPUEngine -ErrorAction SilentlyContinue |
      Where-Object Name -match 'phys_\d+' | Group-Object { [regex]::Match($_.Name,'luid_0x[0-9a-f]+_0x[0-9a-f]+_phys_\d+').Value } |
      Sort-Object Name | ForEach-Object -Begin { $index=0 } -Process { $gpu=($_.Group | Group-Object { $_.Name -replace '^pid_\d+_', '' } | ForEach-Object { ($_.Group | Measure-Object UtilizationPercentage -Sum).Sum } | Measure-Object -Maximum).Maximum
      "GPU $index $([Math]::Min(100,$gpu)) %"; $index++ }"#;
    hidden("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .ok()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("   ·   ")
        })
        .unwrap_or_default()
}

struct Studio {
    saved: String,
    web: Option<web::Interface>,
    virtual_cameras: Option<vcam::Cameras>,
    virtual_error: String,
    session_ip: Option<Ipv4Addr>,
    lang: usize,
    confirmed: std::collections::HashMap<Ipv4Addr, Vec<Mode>>,
    probing: Option<std::sync::mpsc::Receiver<(Ipv4Addr, Vec<Mode>)>>,
    rescan_modes: bool,
    mode_retries: std::collections::BTreeSet<Ipv4Addr>,
    attempted: Option<String>,
    cancel_probe: Arc<AtomicBool>,
    next_search: Instant,
    refresh: bool,
    closing: bool,
    tray: Option<tray_icon::TrayIcon>,
    quit_requested: Arc<AtomicBool>,
    background_on_launch: Option<Instant>,
    demo: bool,
    actual_fps: f32,
    adapters: Vec<String>,
    adapter_scan: std::sync::mpsc::Receiver<Vec<String>>,
    gpu: Option<u32>,
    metrics: std::sync::mpsc::Receiver<String>,
    usage: String,
    cameras: Vec<Ipv4Addr>,
    discovery: Option<std::sync::mpsc::Receiver<Vec<Ipv4Addr>>>,
    ip: String,
    resolution: u32,
    lens: u32,
    fps: u32,
    image: ImageSettings,
    session: Option<Session>,
    texture: Option<egui::TextureHandle>,
    status: String,
    camera_info: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
    capabilities: String,
}

impl Default for Studio {
    fn default() -> Self {
        Self {
            lang: 0,
            saved: String::new(),
            web: None,
            virtual_cameras: None,
            virtual_error: String::new(),
            session_ip: None,
            confirmed: Default::default(),
            probing: None,
            rescan_modes: false,
            mode_retries: Default::default(),
            attempted: None,
            cancel_probe: Arc::new(AtomicBool::new(false)),
            next_search: Instant::now() + Duration::from_secs(15),
            refresh: false,
            closing: false,
            tray: None,
            quit_requested: Arc::new(AtomicBool::new(false)),
            background_on_launch: std::env::args()
                .any(|arg| arg == "--background")
                .then(|| Instant::now() + Duration::from_millis(500)),
            demo: std::env::args().any(|arg| arg == "--demo"),
            actual_fps: 0.0,
            adapters: Vec::new(),
            adapter_scan: {
                let (send, receive) = std::sync::mpsc::channel();
                thread::spawn(move || {
                    let _ = send.send(gpu_adapters());
                });
                receive
            },
            gpu: None,
            metrics: {
                let (sender, receiver) = std::sync::mpsc::channel();
                thread::spawn(move || loop {
                    if sender.send(usage()).is_err() {
                        break;
                    }
                    thread::sleep(Duration::from_secs(3));
                });
                receiver
            },
            usage: String::new(),
            cameras: Vec::new(),
            discovery: {
                let (sender, receiver) = std::sync::mpsc::channel();
                thread::spawn(move || {
                    let _ = sender.send(discover());
                });
                Some(receiver)
            },
            ip: String::new(),
            resolution: 12,
            lens: 0,
            fps: 30,
            image: ImageSettings::default(),
            session: None,
            texture: None,
            status: "Branche et allume ta GoPro.".into(),
            camera_info: None,
            capabilities: String::new(),
        }
    }
}
impl Drop for Studio {
    fn drop(&mut self) {
        self.cancel_probe.store(true, Ordering::Relaxed);
        if let Some(session) = &self.session {
            session.stop();
        }
    }
}

fn translate<'a>(lang: usize, text: &'a str) -> &'a str {
    match text {
        "Caméra" => tr(
            lang,
            ["Caméra", "Camera", "Cámara", "カメラ", "Камера", "Kamera"],
        ),
        "Connexion USB" => tr(
            lang,
            [
                "Connexion USB",
                "USB connection",
                "Conexión USB",
                "USB 接続",
                "Подключение USB",
                "USB-Verbindung",
            ],
        ),
        "Adresse de la caméra" => tr(
            lang,
            [
                "Adresse de la caméra",
                "Camera address",
                "Dirección de la cámara",
                "カメラのアドレス",
                "Адрес камеры",
                "Kameraadresse",
            ],
        ),
        "Lire les capacités caméra" => tr(
            lang,
            [
                "Lire les capacités caméra",
                "Read camera capabilities",
                "Consultar capacidades",
                "カメラ機能を取得",
                "Проверить возможности",
                "Kamerafunktionen abfragen",
            ],
        ),
        "Réponse de la caméra" => tr(
            lang,
            [
                "Réponse de la caméra",
                "Camera response",
                "Respuesta de la cámara",
                "カメラの応答",
                "Ответ камеры",
                "Kameraantwort",
            ],
        ),
        "Champ de vision" => tr(
            lang,
            [
                "Champ de vision",
                "Field of view",
                "Campo de visión",
                "画角",
                "Угол обзора",
                "Sichtfeld",
            ],
        ),
        "Large" => tr(lang, ["Large", "Wide", "Amplio", "広角", "Широкий", "Weit"]),
        "Linéaire" => tr(
            lang,
            [
                "Linéaire",
                "Linear",
                "Lineal",
                "リニア",
                "Линейный",
                "Linear",
            ],
        ),
        "Étroit" => tr(
            lang,
            ["Étroit", "Narrow", "Estrecho", "狭角", "Узкий", "Schmal"],
        ),
        "Vidéo" => tr(lang, ["Vidéo", "Video", "Vídeo", "映像", "Видео", "Video"]),
        "Réglages de l’image" => tr(
            lang,
            [
                "Réglages de l’image",
                "Image filters",
                "Filtros de imagen",
                "映像フィルター",
                "Фильтры изображения",
                "Bildfilter",
            ],
        ),
        "Luminosité" => tr(
            lang,
            [
                "Luminosité",
                "Brightness",
                "Brillo",
                "明るさ",
                "Яркость",
                "Helligkeit",
            ],
        ),
        "Contraste" => tr(
            lang,
            [
                "Contraste",
                "Contrast",
                "Contraste",
                "コントラスト",
                "Контраст",
                "Kontrast",
            ],
        ),
        "Saturation" => tr(
            lang,
            [
                "Saturation",
                "Saturation",
                "Saturación",
                "彩度",
                "Насыщенность",
                "Sättigung",
            ],
        ),
        "Zoom numérique" => tr(
            lang,
            [
                "Zoom numérique",
                "Digital zoom",
                "Zoom digital",
                "デジタルズーム",
                "Цифровой зум",
                "Digitalzoom",
            ],
        ),
        "Miroir" => tr(
            lang,
            [
                "Miroir",
                "Mirror",
                "Espejo",
                "左右反転",
                "Отражение",
                "Spiegeln",
            ],
        ),
        "Réinitialiser l’image" => tr(
            lang,
            [
                "Réinitialiser l’image",
                "Reset image",
                "Restablecer imagen",
                "映像をリセット",
                "Сбросить фильтры",
                "Bild zurücksetzen",
            ],
        ),
        "Arrêter" => tr(
            lang,
            [
                "Arrêter",
                "Stop",
                "Detener",
                "停止",
                "Остановить",
                "Stoppen",
            ],
        ),
        "Démarrer" => tr(
            lang,
            [
                "Démarrer",
                "Start",
                "Iniciar",
                "開始",
                "Запустить",
                "Starten",
            ],
        ),
        "Aperçu" => tr(
            lang,
            [
                "Aperçu",
                "Preview",
                "Vista previa",
                "プレビュー",
                "Предпросмотр",
                "Vorschau",
            ],
        ),
        "Aucun flux vidéo" => tr(
            lang,
            [
                "Aucun flux vidéo",
                "No video feed",
                "Sin señal de vídeo",
                "映像信号なし",
                "Нет видеосигнала",
                "Kein Videosignal",
            ],
        ),
        "Branche et allume ta GoPro." => tr(
            lang,
            [
                "Branche et allume ta GoPro.",
                "Connect and turn on your GoPro.",
                "Conecta y enciende tu GoPro.",
                "GoPro を接続して電源を入れてください。",
                "Подключите и включите GoPro.",
                "GoPro verbinden und einschalten.",
            ],
        ),
        "Connexion à la GoPro…" => tr(
            lang,
            [
                "Connexion à la GoPro…",
                "Connecting to GoPro…",
                "Conectando con la GoPro…",
                "GoPro に接続中…",
                "Подключение к GoPro…",
                "Verbindung zur GoPro…",
            ],
        ),
        "En direct" => tr(
            lang,
            [
                "En direct",
                "Live",
                "En directo",
                "ライブ",
                "В эфире",
                "Live",
            ],
        ),
        "Aperçu arrêté" => tr(
            lang,
            [
                "Aperçu arrêté",
                "Preview stopped",
                "Vista previa detenida",
                "プレビュー停止",
                "Предпросмотр остановлен",
                "Vorschau gestoppt",
            ],
        ),
        "Flux terminé" => tr(
            lang,
            [
                "Flux terminé",
                "Stream ended",
                "Transmisión finalizada",
                "配信終了",
                "Поток завершён",
                "Stream beendet",
            ],
        ),
        "Adresse IPv4 invalide." => tr(
            lang,
            [
                "Adresse IPv4 invalide.",
                "Invalid IPv4 address.",
                "Dirección IPv4 no válida.",
                "IPv4 アドレスが無効です。",
                "Неверный адрес IPv4.",
                "Ungültige IPv4-Adresse.",
            ],
        ),
        "Lecture en cours…" => tr(
            lang,
            [
                "Lecture en cours…",
                "Reading…",
                "Consultando…",
                "読み込み中…",
                "Чтение…",
                "Wird abgefragt…",
            ],
        ),
        _ => text,
    }
}

fn status_color(status: &str) -> egui::Color32 {
    let (red, green, blue) = match status {
        "En direct" | "Signal de test" => (28, 238, 31),
        "Connexion à la GoPro…" | "Vérification des images caméra…" => (245, 199, 0),
        "Branche et allume ta GoPro." | "Aperçu arrêté" | "Flux terminé" => (183, 231, 252),
        _ => (239, 62, 46),
    };
    egui::Color32::from_rgb(red, green, blue)
}

type Preferences = (usize, String, u32, u32, u32, Option<u32>, ImageSettings);

fn config_path() -> Option<std::path::PathBuf> {
    Some(
        std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA")?)
            .join("GoPro USB")
            .join("settings.json"),
    )
}

impl Studio {
    fn load() -> Self {
        let mut app = Self::default();
        if let Some(text) = config_path().and_then(|p| std::fs::read_to_string(p).ok()) {
            if let Ok((lang, ip, res, lens, fps, gpu, image)) =
                serde_json::from_str::<Preferences>(&text)
            {
                app.lang = lang.min(LANGUAGES.len() - 1);
                app.ip = ip;
                if [360, 480, 7, 12, 1440, 2160].contains(&res) {
                    app.resolution = res;
                }
                if [0, 2, 3, 4].contains(&lens) {
                    app.lens = lens;
                }
                if [15, 24, 30, 50, 60, 75, 100, 120].contains(&fps) {
                    app.fps = fps;
                }
                app.gpu = gpu;
                app.image = image;
                if let Some(marker) = config_path().map(|p| p.with_file_name("orientation-v2")) {
                    if !marker.exists() {
                        app.image.flip = false;
                        let _ =
                            std::fs::write(marker, "DirectShow top-down RGBA conversion corrected");
                    }
                }
                app.saved = text;
            }
        }
        app
    }

    fn save(&mut self) {
        let Ok(text) = serde_json::to_string(&(
            self.lang,
            &self.ip,
            self.resolution,
            self.lens,
            self.fps,
            self.gpu,
            self.image,
        )) else {
            return;
        };
        if text == self.saved {
            return;
        }
        if let Some(path) = config_path() {
            let temp = path.with_extension("tmp");
            if std::fs::create_dir_all(path.parent().unwrap())
                .and_then(|_| std::fs::write(&temp, &text))
                .and_then(|_| std::fs::rename(temp, path))
                .is_ok()
            {
                self.saved = text;
            }
        }
    }
}

impl eframe::App for Studio {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(session) = &self.session {
            let mut state = session.shared.lock().unwrap();
            self.status.clone_from(&state.status);
            self.actual_fps = state.fps;
            if let Some(pixels) = state.frame.take() {
                if let Some(web) = &self.web {
                    web.frame(pixels);
                } else {
                    let image = egui::ColorImage::from_rgba_unmultiplied([WIDTH, HEIGHT], &pixels);
                    if let Some(texture) = &mut self.texture {
                        texture.set(image, egui::TextureOptions::LINEAR);
                    } else {
                        self.texture =
                            Some(ctx.load_texture("GoPro", image, egui::TextureOptions::LINEAR));
                    }
                }
            }
            let finished = state.finished;
            if finished&&state.fps==0.0&&!self.closing&&!session.cancel.load(Ordering::Relaxed){
                if let Some(ip)=self.session_ip {
                    if self.mode_retries.insert(ip){
                        self.confirmed.remove(&ip);
                        self.rescan_modes=true;
                        self.refresh=true;
                    }
                }
            }
            let reconnect = finished
                && state.fps > 0.0
                && !self.demo
                && !self.closing
                && !session.cancel.load(Ordering::Relaxed);
            drop(state);
            if finished {
                self.session = None;
                self.session_ip = None;
                self.texture = None;
                self.actual_fps = 0.0;
                if let Some(web) = &self.web {
                    web.clear();
                }
                if reconnect {
                    self.refresh = true;
                }
            } else {
                ctx.request_repaint_after(Duration::from_millis(7));
            }
        }
        if let Some(receiver) = &self.camera_info {
            match receiver.try_recv() {
                Ok(result) => {
                    self.capabilities =
                        result.unwrap_or_else(|e| format!("Caméra injoignable : {e}"));
                    self.camera_info = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(100))
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.camera_info = None,
            }
        }
        if let Some(receiver) = &self.discovery {
            match receiver.try_recv() {
                Ok(cameras) => {
                    if cameras.is_empty() && self.session.is_none() && !self.demo {
                        self.status = "Branche et allume ta GoPro.".into();
                    }
                    self.confirmed.retain(|ip, _| cameras.contains(ip));
                    if let Some(manager) = &mut self.virtual_cameras {
                        manager.sync(&cameras);
                    }
                    if self.session_ip.is_some_and(|ip| !cameras.contains(&ip)) {
                        if let Some(session) = &self.session {
                            session.stop();
                        }
                    }
                    self.cameras = cameras;
                    if let Some(first) = self.cameras.first() {
                        if !self.cameras.iter().any(|ip| ip.to_string() == self.ip) {
                            self.ip = first.to_string();
                        }
                    }
                    self.discovery = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(100))
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.discovery = None,
            }
        }
        if let Ok(sample) = self.metrics.try_recv() {
            self.usage = sample;
        }
        if let Ok(adapters) = self.adapter_scan.try_recv() {
            self.adapters = adapters;
        }
        ctx.request_repaint_after(Duration::from_millis(250));
        if self
            .background_on_launch
            .is_some_and(|at| Instant::now() >= at)
            && self.tray.is_some()
        {
            self.background_on_launch = None;
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
        if ctx.input(|i| i.viewport().close_requested())
            && self.tray.is_some()
            && !self.closing
            && !self.quit_requested.load(Ordering::Relaxed)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
        if (self.quit_requested.load(Ordering::Relaxed)
            || (self.tray.is_none() && ctx.input(|i| i.viewport().close_requested())))
            && !self.closing
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.closing = true;
            self.cancel_probe.store(true, Ordering::Relaxed);
            if let Some(session) = &self.session {
                session.stop();
            }
        }
        if self.closing
            && self.session.is_none()
            && self.probing.is_none()
            && self
                .virtual_cameras
                .as_ref()
                .is_none_or(|m| m.background.is_empty())
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if !self.closing
            && !self.demo
            && self.discovery.is_none()
            && self.probing.is_none()
            && (!self.refresh || self.session.is_none())
            && (self.refresh || Instant::now() >= self.next_search)
        {
            if self.refresh {
                self.confirmed.clear();
                self.attempted = None;
            }
            let manual = self
                .refresh
                .then(|| self.ip.trim().parse::<Ipv4Addr>().ok())
                .flatten();
            self.refresh = false;
            self.next_search = Instant::now() + Duration::from_secs(15);
            let (send, receive) = std::sync::mpsc::channel();
            self.discovery = Some(receive);

            thread::spawn(move || {
                let mut cameras = discover();
                if let Some(ip) = manual.filter(|ip| !cameras.contains(ip)) {
                    if camera(ip, "status").is_ok() {
                        cameras.push(ip);
                    }
                }
                let _ = send.send(cameras);
            });
        }
        if let Some(receiver) = &self.probing {
            match receiver.try_recv() {
                Ok((ip, modes)) => {
                    if modes.is_empty() {
                        self.status = "Aucune image confirmée sur cette caméra.".into();
                    }
                    self.confirmed.insert(ip, modes);
                    self.probing = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.probing = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        let ip = self.ip.trim().parse::<Ipv4Addr>().ok();
        if let Some(ip) = ip {
            if self.cameras.contains(&ip) && !self.confirmed.contains_key(&ip) {
                if let Some(session) = &self.session {
                    session.stop();
                }
            }
            if self.cameras.contains(&ip)
                && !self.confirmed.contains_key(&ip)
                && self.probing.is_none()
                && self.session.is_none()
                && self.discovery.is_none()
                && !self.refresh
                && !self.demo
                && !self.closing
            {
                let (send, receive) = std::sync::mpsc::channel();
                self.probing = Some(receive);
                self.status = "Vérification des images caméra…".into();
                self.cancel_probe.store(false, Ordering::Relaxed);
                let cancel = self.cancel_probe.clone();
                let force=std::mem::take(&mut self.rescan_modes);
                thread::spawn(move || {
                    let _ = send.send((ip, modes::confirmed(ip, &cancel,force)));
                });
            }
        }
        if let Some(web) = self.web.take() {
            web.apply(self);
            self.web = Some(web);
        }
        let ip = self.ip.trim().parse::<Ipv4Addr>().ok();
        let modes = ip
            .and_then(|ip| self.confirmed.get(&ip))
            .cloned()
            .unwrap_or_default();
        let native_fps = preferred_mode(&modes, self.resolution, self.lens)
            .and_then(|m| m.4.parse::<f32>().ok())
            .map(|fps| fps.round() as u32)
            .unwrap_or(30);
        if let Some(manager) = &mut self.virtual_cameras {
            manager.update_background(
                ip,
                self.closing,
                self.probing.as_ref().map(|_| ip).flatten(),
                self.session_ip,
            );
        }
        let background_stopping = self
            .virtual_cameras
            .as_ref()
            .is_some_and(|m| ip.is_some_and(|ip| m.background.contains_key(&ip)));
        let mode = preferred_mode(&modes, self.resolution, self.lens);
        if !self.closing
            && !self.refresh
            && self.probing.is_none()
            && !background_stopping
            && !ctx.input(|i| i.pointer.any_down())
        {
            let source = ip.zip(mode).map(|(ip, m)| (ip, m.0, m.1));
            if self.demo || source.is_some() {
                if let Some(m) = mode.filter(|_| !self.demo) {
                    self.lens = m.1;
                }
                let source = if self.demo { None } else { source };
                let filter = filters(
                    self.image,
                    self.fps,
                    if mode.is_some_and(|m| m.3 == height(self.resolution)) {
                        0
                    } else {
                        height(self.resolution)
                    },
                    native_fps,
                );
                let key = format!("{source:?}|{:?}", self.gpu);
                let dimensions = match height(self.resolution) {
                    360 => (640, 360),
                    480 => (854, 480),
                    720 => (1280, 720),
                    1440 => (2560, 1440),
                    2160 => (3840, 2160),
                    _ => (1920, 1080),
                };
                if self.attempted.as_ref() == Some(&key) {
                    if let Some(session) = &self.session {
                        session.update_processing(filter.clone(), dimensions);
                        session.update_image(self.image);
                    }
                }
                if self.attempted.as_ref() != Some(&key) {
                    if let Some(session) = &self.session {
                        session.stop();
                    } else {
                        self.attempted = Some(key);
                        self.texture = None;
                        self.session_ip = source.map(|(ip, _, _)| ip);
                        let mut target = self
                            .session_ip
                            .and_then(|ip| self.virtual_cameras.as_ref()?.target(ip));
                        if let Some(target) = &mut target {
                            target.dimensions = match height(self.resolution) {
                                360 => (640, 360),
                                480 => (854, 480),
                                720 => (1280, 720),
                                1440 => (2560, 1440),
                                2160 => (3840, 2160),
                                _ => (1920, 1080),
                            };
                        }
                        self.session = Some(Session::run(source, filter, self.gpu, target));
                        self.session.as_ref().unwrap().update_image(self.image);
                    }
                }
            } else if let Some(session) = &self.session {
                session.stop();
            }
        }
        self.save();
        if let Some(web) = &self.web {
            web.publish(self, &modes, mode, native_fps);
        } else {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.label(&self.status);
            });
        }
    }
}
#[link(name = "user32")]
unsafe extern "system" {
    fn FindWindowW(class: *const u16, title: *const u16) -> isize;
    fn ShowWindow(hwnd: isize, command: i32) -> i32;
    fn SetForegroundWindow(hwnd: isize) -> i32;
}

fn open_window() {
    let title: Vec<u16> = "GoPro USB\0".encode_utf16().collect();
    unsafe {
        let hwnd = FindWindowW(std::ptr::null(), title.as_ptr());
        if hwnd != 0 {
            ShowWindow(hwnd, 9);
            SetForegroundWindow(hwnd);
        }
    }
}

fn create_tray(ctx: &egui::Context, quit: Arc<AtomicBool>) -> Option<tray_icon::TrayIcon> {
    use tray_icon::{
        menu::{Menu, MenuEvent, MenuItem},
        Icon, MouseButton, TrayIconBuilder, TrayIconEvent,
    };
    let pixels = eframe::icon_data::from_png_bytes(include_bytes!("../assets/camera.png")).ok()?;
    let icon = Icon::from_rgba(pixels.rgba.clone(), pixels.width, pixels.height).ok()?;
    let menu = Menu::new();
    let open = MenuItem::new("Ouvrir GoPro USB", true, None);
    let exit = MenuItem::new("Quitter", true, None);
    menu.append_items(&[&open, &exit]).ok()?;
    let open_id = open.id().clone();
    let exit_id = exit.id().clone();
    let tray = TrayIconBuilder::new()
        .with_tooltip("GoPro USB")
        .with_icon(icon)
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()
        .ok()?;
    let ctx = ctx.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if event.id == open_id {
            open_window();
        }
        if event.id == exit_id {
            quit.store(true, Ordering::Relaxed);
            open_window();
            ctx.request_repaint();
        }
    }));
    TrayIconEvent::set_event_handler(Some(|event| {
        if matches!(
            event,
            TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            }
        ) {
            open_window();
        }
    }));
    Some(tray)
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateMutexW(attributes: *const std::ffi::c_void, owner: i32, name: *const u16) -> isize;
    fn GetLastError() -> u32;
    fn CloseHandle(handle: isize) -> i32;
}

struct Instance(isize);
impl Drop for Instance {
    fn drop(&mut self) {
        if self.0 != 0 {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

fn main() -> eframe::Result {
    if let Some(code)=modern::helper(){std::process::exit(code);}
    if let Some(code) = watch::helper() {
        std::process::exit(code);
    }
    if let Some(code) = vcam::helper() {
        std::process::exit(code);
    }
    let background = std::env::args().any(|arg| arg == "--background");
    let name: Vec<u16> = "Local\\GoPro USB.Application\0".encode_utf16().collect();
    let (instance, exists) = unsafe {
        let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        (Instance(handle), handle != 0 && GetLastError() == 183)
    };
    let _instance = instance;
    if exists {
        if !background {
            open_window();
        }
        return Ok(());
    }
    let _ = watch::launch();
    modern::auto_install();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!("../assets/camera.png"))
                    .expect("invalid camera icon"),
            )
            .with_active(!background)
            .with_inner_size([1060.0, 740.0])
            .with_min_inner_size([700.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "GoPro USB",
        options,
        Box::new(|cc| {
            let mut style = (*cc.egui_ctx.style()).clone();
            style.spacing.item_spacing = egui::vec2(10.0, 11.0);
            style.spacing.button_padding = egui::vec2(10.0, 6.0);
            style.visuals = egui::Visuals::dark();
            style.visuals.panel_fill = egui::Color32::from_rgb(18, 33, 42);
            style.visuals.window_fill = egui::Color32::from_rgb(19, 40, 70);
            style.visuals.selection.bg_fill = egui::Color32::from_rgb(13, 102, 254);
            for widget in [
                &mut style.visuals.widgets.inactive,
                &mut style.visuals.widgets.hovered,
                &mut style.visuals.widgets.active,
            ] {
                widget.corner_radius = egui::CornerRadius::same(7);
            }
            cc.egui_ctx.set_style(style);
            if let Ok(data) = std::fs::read(r"C:\Windows\Fonts\msgothic.ttc") {
                let mut fonts = egui::FontDefinitions::default();
                fonts
                    .font_data
                    .insert("Japanese".into(), egui::FontData::from_owned(data).into());
                for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                    fonts
                        .families
                        .entry(family)
                        .or_default()
                        .push("Japanese".into());
                }
                cc.egui_ctx.set_fonts(fonts);
            }
            let mut studio = Studio::load();
            match vcam::Cameras::load() {
                Ok(manager) => studio.virtual_cameras = Some(manager),
                Err(error) => studio.virtual_error = error,
            }
            studio.tray = create_tray(&cc.egui_ctx, studio.quit_requested.clone());
            match web::Interface::new(cc) {
                Ok(web) => studio.web = Some(web),
                Err(error) => studio.status = format!("WebView2 : {error}"),
            }
            Ok(Box::new(studio))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "Requires the connected GoPro; the desktop app must be stopped first"]
    fn measure_camera_modes() {
        let ip: Ipv4Addr = std::env::var("GOPRO_CHECK_IP")
            .expect("GOPRO_CHECK_IP")
            .parse()
            .unwrap();
        let settings = ImageSettings::default();
        let session = Session::run(
            Some((ip, 12, 4)),
            filters(settings, 30, 1080, 30),
            Some(2),
            None,
        );
        let mut results = Vec::new();
        for (h, w) in [
            (360, 640),
            (480, 854),
            (720, 1280),
            (1080, 1920),
            (1440, 2560),
            (2160, 3840),
        ] {
            if std::env::var_os("GOPRO_CHECK_FAST").is_some() && ![720,1080].contains(&h){continue;}
            for fps in [15, 24, 30, 50, 60, 75, 100, 120] {
                if std::env::var_os("GOPRO_CHECK_FAST").is_some() && ![30,100,120].contains(&fps){continue;}
                session.update_processing(filters(settings, fps, h, 30), (w as usize, h as usize));
                let mut samples = Vec::new();
                let deadline = Instant::now() + Duration::from_millis(3400);
                while Instant::now() < deadline {
                    let mut state = session.shared.lock().unwrap();
                    if state.finished {
                        panic!("Capture ended: {}", state.status);
                    }
                    if state.fps > 0.0 {
                        samples.push(state.fps);
                        state.fps = 0.0;
                    }
                    drop(state);
                    thread::sleep(Duration::from_millis(20));
                }
                let measured = samples.last().copied().unwrap_or(0.0);
                println!("{w}x{h} target={fps} measured={measured:.1}");
                results.push(serde_json::json!({"width":w,"height":h,"target_fps":fps,"measured_fps":measured}));
            }
        }
        session.stop();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !session.shared.lock().unwrap().finished && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let path = config_path()
            .unwrap()
            .with_file_name("performance-last.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&results).unwrap()).unwrap();
        assert!(session.shared.lock().unwrap().finished);
    }

    #[test]
    fn lenses_are_filtered_by_actual_source_resolution() {
        let modes = [
            (12, 0, 1920, 1080, "29.97".into()),
            (7, 4, 1280, 720, "29.97".into()),
        ];
        assert_eq!(preferred_mode(&modes, 7, 4).unwrap().1, 4);
        assert_eq!(preferred_mode(&modes, 12, 4).unwrap().1, 0);
        assert_eq!(preferred_mode(&modes, 1440, 4).unwrap().3, 1080);
        assert!(preferred_mode(&[], 12, 0).is_none());
    }

    #[test]
    fn filter_changes_keep_the_decoder_running() {
        let session = Session::run(
            None,
            filters(ImageSettings::default(), 30, 360, 30),
            Some(u32::MAX),
            None,
        );
        let wait = |session: &Session| {
            let deadline = Instant::now() + Duration::from_secs(8);
            loop {
                let mut shared = session.shared.lock().unwrap();
                if shared.frame.take().is_some() {
                    return true;
                }
                if shared.finished || Instant::now() >= deadline {
                    return false;
                }
                drop(shared);
                thread::sleep(Duration::from_millis(10));
            }
        };
        assert!(wait(&session));
        let decoder = session.child.lock().unwrap().as_ref().unwrap().id();
        let initial = {
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                let mut state = session.shared.lock().unwrap();
                if let Some(frame) = state.frame.take() {
                    let avg = frame
                        .chunks_exact(4)
                        .map(|p| p[0] as u64 + p[1] as u64 + p[2] as u64)
                        .sum::<u64>() as f64
                        / (WIDTH * HEIGHT * 3) as f64;
                    break avg;
                }
                assert!(Instant::now() < deadline);
                drop(state);
                thread::sleep(Duration::from_millis(10));
            }
        };
        let mut image = ImageSettings::default();
        image.brightness = 0.3;
        session.update_image(image);
        let deadline = Instant::now() + Duration::from_secs(4);
        let mut changed = false;
        while Instant::now() < deadline {
            let mut state = session.shared.lock().unwrap();
            if let Some(frame) = state.frame.take() {
                let avg = frame
                    .chunks_exact(4)
                    .map(|p| p[0] as u64 + p[1] as u64 + p[2] as u64)
                    .sum::<u64>() as f64
                    / (WIDTH * HEIGHT * 3) as f64;
                if avg > initial + 20.0 {
                    changed = true;
                    break;
                }
            }
            drop(state);
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            changed,
            "Live filter command did not modify the actual image"
        );
        assert_eq!(
            session.child.lock().unwrap().as_ref().unwrap().id(),
            decoder
        );
        session.stop();
        let deadline = Instant::now() + Duration::from_secs(4);
        while !session.shared.lock().unwrap().finished && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(session.shared.lock().unwrap().finished);
    }

    #[test]
    fn decodes_test_signal_and_stops_cleanly() {
        let mut image = ImageSettings::default();
        image.mirror = true;
        image.flip = true;
        let session = Session::run(None, filters(image, 60, 360, 30), None, None);
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut samples = 0;
        while Instant::now() < deadline {
            {
                let mut state = session.shared.lock().unwrap();
                if state
                    .frame
                    .take()
                    .is_some_and(|pixels| pixels.len() == WIDTH * HEIGHT * 4)
                {
                    samples += 1;
                }
                if samples >= 2 || state.finished {
                    break;
                }
            }
            thread::sleep(Duration::from_millis(20));
        }
        session.stop();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !session.shared.lock().unwrap().finished && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        let state = session.shared.lock().unwrap();
        assert!(samples >= 2, "No filtered preview: {}", state.status);
        assert!(state.finished, "Preview worker still running");
    }
}
