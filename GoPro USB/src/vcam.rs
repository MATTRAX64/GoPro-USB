use super::*;
use libloading::Library;
use std::{
    collections::BTreeMap,
    ffi::c_void,
    hash::{Hash, Hasher},
    path::PathBuf,
};

const COMPONENT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/x64/GoProCapture.dll"));
const COMPONENT32: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/x86/GoProCapture.dll"));
const LICENSE: &str = include_str!("../third_party/UnityCapture/Source/UnityCaptureFilter.cpp");
const MAX_CAMERAS: usize = 60;
type Register = unsafe extern "C" fn(i32, *const u16, i32) -> i32;
type Create = unsafe extern "C" fn(i32) -> *mut c_void;
type SendFrame = unsafe extern "C" fn(*mut c_void, *const u8, i32, i32) -> i32;
type Destroy = unsafe extern "C" fn(*mut c_void);

fn install_binary(component: &[u8], architecture: &str) -> Result<PathBuf, String> {
    let base = config_path()
        .ok_or("AppData indisponible")?
        .parent()
        .unwrap()
        .join("Camera");
    std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    let mut hash = std::hash::DefaultHasher::new();
    component.hash(&mut hash);
    let path = base.join(format!(
        "GoProCapture-{architecture}-{:016x}.dll",
        hash.finish()
    ));
    if std::fs::read(&path).ok().as_deref() != Some(component) {
        let temp = path.with_extension("tmp");
        std::fs::write(&temp, component)
            .and_then(|_| std::fs::rename(&temp, &path))
            .map_err(|e| e.to_string())?;
    }
    let notice = LICENSE
        .split("*/")
        .next()
        .unwrap_or(LICENSE)
        .trim_start_matches("/*");
    std::fs::write(base.join("LICENSE-UnityCapture.txt"), notice).map_err(|e| e.to_string())?;
    Ok(path)
}

fn install() -> Result<PathBuf, String> {
    install_binary(COMPONENT32, "x86")?;
    install_binary(COMPONENT, "x64")
}

/// Called in a short-lived helper process; avoids changing COM registration on the UI thread.
pub fn helper() -> Option<i32> {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).map(String::as_str) != Some("--camera-register") {
        return None;
    }
    let result = (|| {
        let slot: i32 = args
            .get(2)
            .ok_or("Slot manquant")?
            .parse()
            .map_err(|_| "Slot invalide")?;
        if !(0..MAX_CAMERAS as i32).contains(&slot) {
            return Err("Slot invalide".into());
        }
        let name = args.get(3).ok_or("Nom manquant")?;
        let active = i32::from(args.get(4).map(String::as_str) == Some("1"));
        let path = install()?;
        let library = unsafe { Library::new(path) }.map_err(|e| e.to_string())?;
        let register =
            unsafe { library.get::<Register>(b"GoProRegister") }.map_err(|e| e.to_string())?;
        let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let error = unsafe { register(slot, wide.as_ptr(), active) };
        if error != 0 {
            return Err(format!("Enregistrement DirectShow : {error}"));
        }

        let x86 = install_binary(COMPONENT32, "x86")?;
        let system = std::env::var_os("SystemRoot").ok_or("SystemRoot indisponible")?;
        let registrar = PathBuf::from(system).join("SysWOW64").join("regsvr32.exe");
        let result = hidden(registrar.to_str().ok_or("Chemin invalide")?)
            .args([
                "/s",
                "/n",
                &format!("/i:{slot}|{name}|{active}"),
                x86.to_str().ok_or("Chemin invalide")?,
            ])
            .status()
            .map_err(|e| e.to_string())?;
        if !result.success() {
            return Err("Enregistrement caméra 32 bits impossible".into());
        }
        Ok::<_, String>(())
    })();
    Some(if result.is_ok() { 0 } else { 1 })
}

pub struct Driver {
    _library: Library,
    create: Create,
    send: SendFrame,
    destroy: Destroy,
}

impl Driver {
    pub fn load() -> Result<Arc<Self>, String> {
        let library = unsafe { Library::new(install()?) }.map_err(|e| e.to_string())?;
        unsafe {
            Ok(Arc::new(Self {
                create: *library
                    .get::<Create>(b"GoProSenderCreate")
                    .map_err(|e| e.to_string())?,
                send: *library
                    .get::<SendFrame>(b"GoProSend")
                    .map_err(|e| e.to_string())?,
                destroy: *library
                    .get::<Destroy>(b"GoProSenderDestroy")
                    .map_err(|e| e.to_string())?,
                _library: library,
            }))
        }
    }

    pub(crate) fn register(slot: u32, name: &str, active: bool) -> Result<(), String> {
        let status = hidden(
            std::env::current_exe()
                .map_err(|e| e.to_string())?
                .to_str()
                .ok_or("Chemin invalide")?,
        )
        .args([
            "--camera-register",
            &slot.to_string(),
            name,
            if active { "1" } else { "0" },
        ])
        .status()
        .map_err(|e| e.to_string())?;
        if status.success() {
            Ok(())
        } else {
            Err("Impossible d’enregistrer la caméra virtuelle Windows.".into())
        }
    }
}

pub struct Sender {
    driver: Arc<Driver>,
    pointer: *mut c_void,
    target: Target,
}
unsafe impl std::marker::Send for Sender {}

#[derive(Clone)]
pub struct Target {
    pub driver: Arc<Driver>,
    pub slot: u32,
    pub name: String,
    pub live: Arc<AtomicBool>,
    pub enabled: Arc<AtomicBool>,
    pub error: Arc<Mutex<String>>,
    pub dimensions: (usize, usize),
}

impl Target {
    pub fn sender(&self) -> Result<Sender, String> {
        let pointer = unsafe { (self.driver.create)(self.slot as i32) };
        if pointer.is_null() {
            return Err("Création du flux virtuel impossible".into());
        }
        Ok(Sender {
            driver: self.driver.clone(),
            pointer,
            target: self.clone(),
        })
    }
}
impl Sender {
    pub fn frame(&mut self, pixels: &[u8], width: usize, height: usize) -> Result<(), String> {
        if pixels.len() != width * height * 4 {
            return Err("Taille d’image virtuelle invalide".into());
        }
        if !self.target.enabled.load(Ordering::Relaxed) {
            return Ok(());
        }
        if !self.target.live.load(Ordering::Relaxed) {
            Driver::register(self.target.slot, &self.target.name, true)?;
            if !self.target.enabled.load(Ordering::Relaxed) {
                Driver::register(self.target.slot, &self.target.name, false)?;
                return Ok(());
            }
            self.target.error.lock().unwrap().clear();
            self.target.live.store(true, Ordering::Relaxed);
        }
        let result = unsafe {
            (self.driver.send)(self.pointer, pixels.as_ptr(), width as i32, height as i32)
        };
        if result < 0 {
            return Err("Transmission vers la caméra virtuelle impossible".into());
        }
        Ok(())
    }
}
impl Drop for Sender {
    fn drop(&mut self) {
        unsafe {
            (self.driver.destroy)(self.pointer);
        }
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Identity {
    model: String,
    slot: u32,
}

pub struct Cameras {
    driver: Arc<Driver>,
    identities: BTreeMap<String, Identity>,
    targets: BTreeMap<Ipv4Addr, Target>,
    pub background: BTreeMap<Ipv4Addr, Session>,
    retry_after: BTreeMap<Ipv4Addr, Instant>,
    pub error: String,
}
impl Cameras {
    pub fn load() -> Result<Self, String> {
        let driver = Driver::load()?;
        let path = config_path().unwrap().with_file_name("cameras.json");
        let identities: BTreeMap<String, Identity> = std::fs::read(path)
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default();
        // Remove device monikers left behind if a previous process was interrupted.
        for id in identities.values() {
            if id.slot < MAX_CAMERAS as u32 {
                Driver::register(id.slot, "", false)?;
            }
        }
        Ok(Self {
            driver,
            identities,
            targets: BTreeMap::new(),
            background: BTreeMap::new(),
            retry_after: BTreeMap::new(),
            error: String::new(),
        })
    }

    pub fn sync(&mut self, connected: &[Ipv4Addr]) {
        let removed: Vec<_> = self
            .targets
            .keys()
            .filter(|ip| !connected.contains(ip))
            .copied()
            .collect();
        for ip in removed {
            if let Some(session) = self.background.remove(&ip) {
                session.stop();
            }
            if let Some(target) = self.targets.remove(&ip) {
                target.enabled.store(false, Ordering::Relaxed);
                target.live.store(false, Ordering::Relaxed);
                if let Err(error) = Driver::register(target.slot, &target.name, false) {
                    self.error = error;
                }
            }
        }
        for ip in connected {
            if self.targets.contains_key(ip) {
                continue;
            }
            // Camera identity is fetched over the USB network link and persisted locally.
            let response = hidden("curl.exe")
                .args([
                    "--noproxy",
                    "*",
                    "-sf",
                    "--max-time",
                    "2",
                    &format!("http://{ip}:8080/gopro/camera/info"),
                ])
                .output();
            let info = response
                .ok()
                .and_then(|o| serde_json::from_slice::<serde_json::Value>(&o.stdout).ok());
            let model = info
                .as_ref()
                .and_then(|v| v["model_name"].as_str())
                .unwrap_or("GoPro")
                .to_owned();
            let identity = info
                .as_ref()
                .and_then(|v| v["serial_number"].as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("")
                .to_owned();
            let key = if identity.is_empty() {
                format!("usb:{ip}")
            } else {
                identity
            };
            let existing = self
                .identities
                .get(&key)
                .map(|id| id.slot)
                .filter(|slot| *slot < MAX_CAMERAS as u32);
            let slot = existing.or_else(|| {
                (0..MAX_CAMERAS as u32)
                    .find(|slot| !self.identities.values().any(|id| id.slot == *slot))
            });
            let Some(slot) = slot else {
                self.error = "Limite de 60 caméras virtuelles atteinte".into();
                continue;
            };
            self.identities.insert(
                key,
                Identity {
                    model: model.clone(),
                    slot,
                },
            );
            let name = format!("GoPro USB {} · {}", slot + 1, model);
            self.targets.insert(
                *ip,
                Target {
                    driver: self.driver.clone(),
                    slot,
                    name,
                    live: Arc::new(AtomicBool::new(false)),
                    enabled: Arc::new(AtomicBool::new(true)),
                    error: Arc::new(Mutex::new(String::new())),
                    dimensions: (1920, 1080),
                },
            );
        }
        if let Ok(data) = serde_json::to_vec(&self.identities) {
            let path = config_path().unwrap().with_file_name("cameras.json");
            let temp = path.with_extension("tmp");
            if let Err(error) =
                std::fs::write(&temp, data).and_then(|_| std::fs::rename(temp, path))
            {
                self.error = error.to_string();
            }
        }
    }

    pub fn target(&self, ip: Ipv4Addr) -> Option<Target> {
        self.targets.get(&ip).cloned()
    }
    pub fn errors(&self) -> Vec<String> {
        let mut errors: Vec<_> = self
            .targets
            .values()
            .filter_map(|t| {
                let error = t.error.lock().unwrap();
                (!error.is_empty()).then(|| format!("{} : {}", t.name, error))
            })
            .collect();
        if !self.error.is_empty() {
            errors.push(self.error.clone());
        }
        errors
    }
    pub fn names(&self) -> Vec<(String, String, bool)> {
        self.targets
            .iter()
            .map(|(ip, t)| {
                (
                    ip.to_string(),
                    t.name.clone(),
                    t.live.load(Ordering::Relaxed),
                )
            })
            .collect()
    }

    pub fn update_background(
        &mut self,
        selected: Option<Ipv4Addr>,
        closing: bool,
        scanning: Option<Ipv4Addr>,
        active: Option<Ipv4Addr>,
    ) {
        for (ip, session) in &self.background {
            if Some(*ip) == selected || closing || Some(*ip) == scanning {
                session.stop();
            }
        }
        self.background.retain(|ip, session| {
            let state = session.shared.lock().unwrap();
            if state.finished && !session.cancel.load(Ordering::Relaxed) {
                if let Some(target) = self.targets.get(ip) {
                    *target.error.lock().unwrap() = state.status.clone();
                }
                self.retry_after
                    .insert(*ip, Instant::now() + Duration::from_secs(15));
            }
            !state.finished
        });
        if closing {
            return;
        }
        for (ip, target) in &self.targets {
            if Some(*ip) == selected
                || Some(*ip) == scanning
                || Some(*ip) == active
                || self.background.contains_key(ip)
                || self
                    .retry_after
                    .get(ip)
                    .is_some_and(|at| Instant::now() < *at)
            {
                continue;
            }
            self.background.insert(
                *ip,
                Session::run(
                    Some((*ip, 12, 0)),
                    filters(ImageSettings::default(), 30, 0, 30),
                    None,
                    Some(target.clone()),
                ),
            );
        }
    }
}
impl Drop for Cameras {
    fn drop(&mut self) {
        for session in self.background.values() {
            session.stop();
        }
        for target in self.targets.values() {
            target.enabled.store(false, Ordering::Relaxed);
            let _ = Driver::register(target.slot, &target.name, false);
        }
    }
}


// A slow recording client must never block the decoder or build a queue of old images.
struct OutputFrame { pixels:Vec<u8>,width:usize,height:usize }
#[derive(Default)]
struct OutputQueue { pending:Option<OutputFrame>,spare:Vec<u8>,stopped:bool }
pub struct Delivery {
    state:Arc<(Mutex<OutputQueue>,std::sync::Condvar)>,
    worker:Option<std::thread::JoinHandle<()>>,
    modern:bool,
}
impl Delivery {
    pub fn start(target:Target)->Self {Self::spawn(target,false)}
    pub fn start_modern(target:Target)->Self {Self::spawn(target,true)}
    fn spawn(target:Target,modern:bool)->Self {
        let state=Arc::new((Mutex::new(OutputQueue::default()),std::sync::Condvar::new()));
        let input=state.clone();
        let worker=thread::spawn(move||{
            let mut sender=if modern {None}else{match target.sender(){
                Ok(sender)=>Some(sender),
                Err(error)=>{*target.error.lock().unwrap()=error;return;}
            }};
            let mut publisher=None;
            let mut retry=Instant::now();
            loop {
                let (lock,wake)=&*input;
                let mut queue=lock.lock().unwrap();
                while queue.pending.is_none()&&!queue.stopped {queue=wake.wait(queue).unwrap();}
                if queue.stopped {break;}
                let frame=queue.pending.take().unwrap();
                drop(queue);
                if let Some(sender)=&mut sender {
                    if let Err(error)=sender.frame(&frame.pixels,frame.width,frame.height){*target.error.lock().unwrap()=error;}
                } else {
                    if publisher.is_none()&&Instant::now()>=retry {
                        retry=Instant::now()+Duration::from_secs(15);
                        match modern::Publisher::create(target.slot,&target.name) {
                            Ok(value)=>{publisher=Some(value);target.error.lock().unwrap().clear();}
                            Err(error)=>{*target.error.lock().unwrap()=error;}
                        }
                    }
                    if let Some(publisher)=&publisher{publisher.frame(&frame.pixels,frame.width,frame.height);}
                }
                let mut queue=lock.lock().unwrap();
                queue.spare=frame.pixels;
            }
        });
        Self{state,worker:Some(worker),modern}
    }
    pub fn frame(&self,pixels:&[u8],width:usize,height:usize){
        let (lock,wake)=&*self.state;
        let Ok(mut queue)=lock.try_lock()else{return;};
        if queue.stopped{return;}
        let mut frame=queue.pending.take().unwrap_or_else(||OutputFrame{
            pixels:std::mem::take(&mut queue.spare),width,height
        });
        frame.width=width;frame.height=height;frame.pixels.resize(pixels.len(),0);
        // Capture components consume the opposite row order from the on-screen preview.
        // Do this only while exporting; explicit X/Y image controls keep their meaning.
        let row=width*4;
        for (destination,source) in frame.pixels.chunks_exact_mut(row).zip(pixels.chunks_exact(row).rev()){
            destination.copy_from_slice(source);
        }
        queue.pending=Some(frame);wake.notify_one();
    }
}
impl Drop for Delivery {
    fn drop(&mut self){
        let (lock,wake)=&*self.state;
        {let mut queue=lock.lock().unwrap();queue.stopped=true;queue.pending=None;wake.notify_all();}
        if let Some(worker)=self.worker.take(){if !self.modern{let _=worker.join();}}
    }
}


#[cfg(test)]
mod delivery_tests {
    use super::*;
    #[test]
    fn exported_rows_match_the_preview() {
        let driver=Driver::load().unwrap();
        let register=unsafe{*driver._library.get::<Register>(b"GoProRegister").unwrap()};
        let name:Vec<u16>="GoPro USB - Orientation verification".encode_utf16().chain(Some(0)).collect();
        assert_eq!(unsafe{register(54,name.as_ptr(),1)},0);
        let target=Target{driver,slot:54,name:"GoPro USB - Orientation verification".into(),
            live:Arc::new(AtomicBool::new(true)),enabled:Arc::new(AtomicBool::new(true)),
            error:Arc::new(Mutex::new(String::new())),dimensions:(64,64)};
        let delivery=Delivery::start(target.clone());
        let mut pixels=vec![0u8;64*64*4];
        for y in 0..64 {for x in 0..64 {
            let p=&mut pixels[(y*64+x)*4..][..4];
            p.copy_from_slice(if y<32 {&[240,20,30,255]}else{&[25,40,230,255]});
        }}
        let deadline=Instant::now()+Duration::from_secs(10);
        while !target.live.load(Ordering::Relaxed) {
            delivery.frame(&pixels,64,64);
            assert!(Instant::now()<deadline,"{}",target.error.lock().unwrap());
            thread::sleep(Duration::from_millis(10));
        }
        let child=hidden("ffmpeg.exe").args(["-hide_banner","-loglevel","error","-f","dshow",
            "-video_size","64x64","-i","video=GoPro USB - Orientation verification",
            "-frames:v","15","-pix_fmt","rgba","-f","rawvideo","pipe:1"])
            .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let pid=child.id();
        let (send,receive)=std::sync::mpsc::channel();
        thread::spawn(move||{let _=send.send(child.wait_with_output());});
        let result=loop {
            delivery.frame(&pixels,64,64);
            if let Ok(output)=receive.try_recv(){break output.unwrap();}
            if Instant::now()>deadline {
                let _=hidden("taskkill.exe").args(["/PID",&pid.to_string(),"/F"]).status();
                panic!("DirectShow client timed out");
            }
            thread::sleep(Duration::from_millis(10));
        };
        target.enabled.store(false,Ordering::Relaxed);
        drop(delivery);
        assert_eq!(unsafe{register(54,[0u16].as_ptr(),0)},0);
        assert!(result.status.success(),"{}",String::from_utf8_lossy(&result.stderr));
        let frame=&result.stdout[result.stdout.len()-pixels.len()..];
        assert_eq!(&frame[(10*64+10)*4..][..3],&[240,20,30]);
        assert_eq!(&frame[(50*64+10)*4..][..3],&[25,40,230]);
    }
}
