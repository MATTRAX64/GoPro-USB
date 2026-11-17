use super::*;
use std::{borrow::Cow, sync::mpsc};

#[derive(serde::Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
enum CommandMessage {
    Refresh,
    Retry,
    InstallCamera,
    RecheckModes,
    Camera { value: Ipv4Addr },
    Resolution { value: u32 },
    Fps { value: u32 },
    Lens { value: u32 },
    Gpu { value: Option<u32> },
    Language { value: usize },
    Image { value: ImageSettings },
}

#[derive(Default)]
struct Content {
    state: Vec<u8>,
    frame: Vec<u8>,
    sequence: u64,
}

pub(super) struct Interface {
    _view: wry::WebView,
    content: Arc<Mutex<Content>>,
    commands: mpsc::Receiver<CommandMessage>,
}

impl Interface {
    pub fn new(cc: &eframe::CreationContext<'_>) -> wry::Result<Self> {
        let content = Arc::new(Mutex::new(Content::default()));
        let resources = content.clone();
        let (send, commands) = mpsc::channel();
        let context = cc.egui_ctx.clone();
        let local = std::env::var_os("LOCALAPPDATA").ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "LOCALAPPDATA is unavailable")
        })?;
        let profile = std::path::PathBuf::from(local)
            .join("GoPro USB")
            .join("WebView2");
        std::fs::create_dir_all(&profile)?;
        let mut web_context = wry::WebContext::new(Some(profile));
        let view = wry::WebViewBuilder::new_with_web_context(&mut web_context)
            .with_navigation_handler(|url| {
                url.starts_with("http://gopro.local/") || url == "gopro://local/"
            })
            .with_custom_protocol("gopro".into(), move |_, request| {
                use wry::http::Response;
                let mut response = Response::builder().header("Cache-Control", "no-store");
                let path = request.uri().path();
                let (kind, body): (&str, Cow<'static, [u8]>) = match path {
                    "/" => (
                        "text/html; charset=utf-8",
                        Cow::Borrowed(include_bytes!("../ui/index.html")),
                    ),
                    "/style.css" => (
                        "text/css; charset=utf-8",
                        Cow::Borrowed(include_bytes!("../ui/style.css")),
                    ),
                    "/interface.js" => (
                        "text/javascript; charset=utf-8",
                        Cow::Borrowed(include_bytes!("../ui/interface.js")),
                    ),
                    "/camera.png" => (
                        "image/png",
                        Cow::Borrowed(include_bytes!("../assets/camera.png")),
                    ),
                    "/state" => (
                        "application/json",
                        Cow::Owned(resources.lock().unwrap().state.clone()),
                    ),
                    "/frame" => {
                        let data = resources.lock().unwrap();
                        let current = request.uri().query().and_then(|s| s.parse().ok());
                        if data.frame.is_empty() || current == Some(data.sequence) {
                            return response.status(204).body(Cow::Borrowed(&[][..])).unwrap();
                        }
                        response = response.header("X-GoPro-Sequence", data.sequence.to_string());
                        ("application/octet-stream", Cow::Owned(data.frame.clone()))
                    }
                    _ => return response.status(404).body(Cow::Borrowed(&[][..])).unwrap(),
                };
                response.header("Content-Type", kind).body(body).unwrap()
            })
            .with_ipc_handler(move |message| {
                if message.body().len() <= 2048 {
                    if let Ok(command) = serde_json::from_str(message.body()) {
                        let _ = send.send(command);
                        context.request_repaint();
                    }
                }
            })
            .with_url("gopro://local/")
            .build(cc)?;
        Ok(Self {
            _view: view,
            content,
            commands,
        })
    }

    pub fn apply(&self, app: &mut Studio) {
        for msg in self.commands.try_iter() {
            match msg {
                CommandMessage::Refresh if app.discovery.is_none() && app.probing.is_none() => {
                    app.refresh = true;
                    if let Some(session) = &app.session {
                        session.stop();
                    }
                }
                CommandMessage::RecheckModes if app.probing.is_none() => {
                    app.rescan_modes=true;
                    app.refresh=true;
                    if let Some(session)=&app.session{session.stop();}
                }
                CommandMessage::Retry => app.attempted = None,
                CommandMessage::InstallCamera => {
                    if let Err(error)=modern::request_install(){app.virtual_error=error;}
                }
                CommandMessage::Camera { value } => app.ip = value.to_string(),
                CommandMessage::Resolution { value }
                    if [360, 480, 7, 12, 1440, 2160].contains(&value) =>
                {
                    app.resolution = value
                }
                CommandMessage::Fps { value } if [15, 24, 30, 50, 60, 75, 100, 120].contains(&value) => {
                    app.fps = value
                }
                CommandMessage::Lens { value } if [0, 2, 3, 4].contains(&value) => app.lens = value,
                CommandMessage::Gpu { value }
                    if value.is_none_or(|n| n == u32::MAX || (n as usize) < app.adapters.len()) =>
                {
                    app.gpu = value
                }
                CommandMessage::Language { value } if value < LANGUAGES.len() => app.lang = value,
                CommandMessage::Image { mut value } => {
                    value.brightness = value.brightness.clamp(-0.5, 0.5);
                    value.contrast = value.contrast.clamp(0.5, 2.0);
                    value.saturation = value.saturation.clamp(0.0, 2.0);
                    value.gamma = value.gamma.clamp(0.5, 2.0);
                    value.sharpness = value.sharpness.clamp(0.0, 1.0);
                    value.zoom = value.zoom.clamp(1.0, 3.0);
                    app.image = value;
                }
                _ => {}
            }
        }
    }

    pub fn frame(&self, frame: Vec<u8>) {
        let mut data = self.content.lock().unwrap();
        data.sequence += 1;
        data.frame = frame;
    }

    pub fn clear(&self) {
        self.content.lock().unwrap().frame.clear();
    }

    pub fn publish(&self, app: &Studio, modes: &[Mode], source: Option<&Mode>, native_fps: u32) {
        use serde_json::json;
        let lang = app.lang;
        let labels = [
            "Caméra",
            "Vidéo",
            "Champ de vision",
            "Réglages de l’image",
            "Luminosité",
            "Contraste",
            "Saturation",
            "Netteté",
            "Zoom numérique",
            "Miroir",
            "Réinitialiser l’image",
            "Aperçu",
            "Aucun flux vidéo",
            "Adresse de la caméra",
        ];
        let translations: std::collections::BTreeMap<_, _> = labels
            .into_iter()
            .map(|label| (label, translate(lang, label)))
            .collect();
        let lenses: Vec<_> = [
            (0, "Large"),
            (4, "Linéaire"),
            (2, "Étroit"),
            (3, "SuperView"),
        ]
        .into_iter()
        .filter(|(id, _)| {
            source.is_some_and(|source| modes.iter().any(|m| m.0 == source.0 && m.1 == *id))
        })
        .map(|(id, name)| (id, translate(lang, name)))
        .collect();
        let resolutions: Vec<_> = [360, 480, 7, 12, 1440, 2160]
            .into_iter()
            .map(|res| (res, resolution_label(res, lang, modes)))
            .collect();
        let cadence: Vec<_> = [15, 24, 30, 50, 60, 75, 100, 120]
            .into_iter()
            .map(|fps| (fps, fps_label(fps, lang, native_fps)))
            .collect();
        let status = status_color(&app.status);
        let state = json!({
            "labels":translations, "lang":lang, "languages":LANGUAGES, "image":app.image,
            "ip":app.ip, "cameras":app.cameras, "resolution":app.resolution, "resolutions":resolutions,
            "fps":app.fps, "cadence":cadence, "lens":app.lens, "lenses":lenses, "gpu":app.gpu,
            "adapters":app.adapters, "usage":app.usage, "fps_actual":app.actual_fps,
            "working":app.discovery.is_some() || app.probing.is_some(),
            "modern_installed":modern::ready(),
            "virtual_cameras":app.virtual_cameras.as_ref().map(|m| m.names()).unwrap_or_default(),
            "virtual_errors":app.virtual_cameras.as_ref().map(|m| m.errors()).unwrap_or_else(|| vec![app.virtual_error.clone()]),
            "retry":app.session.is_none() && app.attempted.is_some(),
            "status":translate(lang,&app.status), "status_color":format!("#{:02x}{:02x}{:02x}",status.r(),status.g(),status.b()),
            "source":source.map(|m| format!("USB · {} × {} · {} FPS",m.2,m.3,m.4))
        });
        self.content.lock().unwrap().state = serde_json::to_vec(&state).unwrap_or_default();
    }
}
