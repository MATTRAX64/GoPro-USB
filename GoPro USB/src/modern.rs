//! Windows 11 Camera Frame Server publication in addition to legacy DirectShow.
use super::*;
use libloading::Library;
use std::{ffi::c_void,hash::{Hash,Hasher},path::PathBuf};
const BINARY:&[u8]=include_bytes!(concat!(env!("OUT_DIR"),"/modern/GoProMF.dll"));
fn version()->String {
    let mut hash=std::hash::DefaultHasher::new();BINARY.hash(&mut hash);format!("{:016x}",hash.finish())
}
fn folder()->Result<PathBuf,String> {
    Ok(PathBuf::from(std::env::var_os("ProgramW6432").or_else(||std::env::var_os("ProgramFiles")).ok_or("Program Files indisponible")?).join("GoPro USB"))
}
fn installed_version()->Option<String>{
    let directory=folder().ok()?;
    let version=std::fs::read_to_string(directory.join("camera-version.txt")).ok()?;
    if version.len()!=16||!version.bytes().all(|c|c.is_ascii_hexdigit()){return None;}
    directory.join(format!("GoProMF-{version}.dll")).is_file().then_some(version)
}
pub fn ready()->bool {installed_version().is_some()}
pub fn auto_install(){
    let Some(path)=config_path().map(|p|p.with_file_name("modern-install-requested")) else{return;};
    if !ready() && !path.exists(){
        let _=std::fs::write(path,b"requested");
        let _=request_install();
    }
}
pub fn request_install()->Result<(),String>{
    #[link(name="shell32")]
    unsafe extern "system" {
        fn ShellExecuteW(window:isize,verb:*const u16,file:*const u16,args:*const u16,dir:*const u16,show:i32)->isize;
    }
    let exe=std::env::current_exe().map_err(|e|e.to_string())?;
    let wide=|value:&str|value.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let result=unsafe{ShellExecuteW(0,wide("runas").as_ptr(),wide(exe.to_str().ok_or("Chemin invalide")?).as_ptr(),
        wide("--install-modern-camera").as_ptr(),std::ptr::null(),0)};
    if result<=32{Err(format!("Installation caméra Windows annulée ou refusée ({result})."))}else{Ok(())}
}
pub fn helper()->Option<i32>{
    let arguments:Vec<_>=std::env::args().collect();
    if arguments.get(1).map(String::as_str)==Some("--modern-camera-fixture"){
        let result=Publisher::create(56,"GoPro USB - MF verification");
        if let Ok(publisher)=result {
            let pixels:Vec<u8>=(0..1280*720).flat_map(|_|[45,145,220,255]).collect();
            let until=Instant::now()+Duration::from_secs(40);
            while Instant::now()<until {
                publisher.frame(&pixels,1280,720);
                thread::sleep(Duration::from_millis(33));
            }
            return Some(0);
        }
        return Some(1);
    }
    if arguments.get(1).map(String::as_str)==Some("--check-modern-camera"){
        let result=(||{
            let path=folder()?.join(format!("GoProMF-{}.dll",installed_version().ok_or("Composant caméra indisponible")?));
            let library=unsafe{Library::new(path)}.map_err(|e|e.to_string())?;
            let check=unsafe{library.get::<unsafe extern "C" fn(*const u16,i32,*mut i32)->i32>(b"GoProMFCheck")}.map_err(|e|e.to_string())?;
            let name:Vec<u16>=arguments.get(2).map(String::as_str).unwrap_or("GoPro USB 1").encode_utf16().chain(Some(0)).collect();
            let format=arguments.get(3).and_then(|v|v.parse::<i32>().ok()).unwrap_or(0);
            let mut mean=0;
            let code=unsafe{check(name.as_ptr(),format,&mut mean)};
            let report=serde_json::json!({"format":format,"hresult":format!("0x{:08X}",code as u32),"pixel_mean":mean});
            if let Some(path)=config_path(){let _=std::fs::write(path.with_file_name("modern-check.json"),report.to_string());}
            if code<0{Err(format!("0x{:08X}",code as u32))}else{Ok(())}
        })();
        return Some(if result.is_ok(){0}else{1});
    }
    if !std::env::args().any(|arg|arg=="--install-modern-camera"){return None;}
    let result=(||{
        let dir=folder()?;std::fs::create_dir_all(&dir).map_err(|e|e.to_string())?;
        let path=dir.join(format!("GoProMF-{}.dll",version()));
        if std::fs::read(&path).ok().as_deref()!=Some(BINARY){
            std::fs::write(&path,BINARY).map_err(|e|e.to_string())?;
        }
        let library=unsafe{Library::new(&path)}.map_err(|e|e.to_string())?;
        let install=unsafe{library.get::<unsafe extern "C" fn()->i32>(b"GoProMFInstall")}.map_err(|e|e.to_string())?;
        let code=unsafe{install()};
        if code<0{return Err(format!("Installation caméra Windows : 0x{:08X}",code as u32));}
        std::fs::write(dir.join("camera-version.txt"),version()).map_err(|e|e.to_string())?;
        Ok::<_,String>(())
    })();
    if let Err(message)=&result {
        if let Some(base)=config_path().and_then(|p|p.parent().map(|p|p.to_path_buf())){
            let _=std::fs::write(base.join("modern-install-error.txt"),message);
        }
    }
    Some(if result.is_ok(){0}else{1})
}
pub struct Publisher {
    pointer:*mut c_void,
    write:unsafe extern "C" fn(*mut c_void,*const u8,i32,i32)->i32,
    destroy:unsafe extern "C" fn(*mut c_void),
    _library:Library,
}
impl Publisher {
    pub fn create(slot:u32,name:&str)->Result<Self,String>{
        if !ready(){return Err("Caméra Windows : installation administrateur requise.".into());}
        let path=folder()?.join(format!("GoProMF-{}.dll",installed_version().ok_or("Composant caméra indisponible")?));
        let library=unsafe{Library::new(path)}.map_err(|e|e.to_string())?;
        unsafe {
            let create=library.get::<unsafe extern "C" fn(i32,*const u16,*mut *mut c_void)->i32>(b"GoProMFCreate").map_err(|e|e.to_string())?;
            let mut pointer=std::ptr::null_mut();
            let name:Vec<u16>=name.encode_utf16().chain(Some(0)).collect();
            let result=create(slot as i32,name.as_ptr(),&mut pointer);
            if result<0{return Err(format!("Caméra Windows : 0x{:08X}",result as u32));}
            Ok(Self{pointer,write:*library.get(b"GoProMFWrite").map_err(|e|e.to_string())?,
                destroy:*library.get(b"GoProMFDestroy").map_err(|e|e.to_string())?,_library:library})
        }
    }
    pub fn frame(&self,pixels:&[u8],width:usize,height:usize){
        unsafe{(self.write)(self.pointer,pixels.as_ptr(),width as i32,height as i32);}
    }
}
impl Drop for Publisher {fn drop(&mut self){unsafe{(self.destroy)(self.pointer);}}}
