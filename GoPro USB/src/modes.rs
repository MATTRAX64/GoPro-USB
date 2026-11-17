//! Persist confirmed capabilities, never camera pictures.
use super::*;
use std::{collections::BTreeMap,path::Path};
#[derive(Clone,serde::Serialize,serde::Deserialize)]
struct Entry {model:String,firmware:String,modes:Vec<Mode>}
#[derive(Default,serde::Serialize,serde::Deserialize)]
struct Cache {schema:u32,cameras:BTreeMap<String,Entry>}
fn read(path:&Path)->Cache {
    std::fs::read(path).ok().and_then(|bytes|serde_json::from_slice::<Cache>(&bytes).ok())
        .filter(|cache|cache.schema==1).unwrap_or_else(||Cache{schema:1,..Default::default()})
}
fn valid(modes:&[Mode])->bool {
    !modes.is_empty()&&modes.iter().all(|m|[4,7,12].contains(&m.0)&&[0,2,3,4].contains(&m.1)&&
        (4..=3840).contains(&m.2)&&(4..=2160).contains(&m.3)&&
        m.4.parse::<f32>().is_ok_and(|fps|fps.is_finite()&&fps>0.0&&fps<=240.0))
}
fn lookup(cache:&Cache,serial:&str,model:&str,firmware:&str)->Option<Vec<Mode>> {
    let entry=cache.cameras.get(serial)?;
    (entry.model==model&&entry.firmware==firmware&&valid(&entry.modes)).then(||entry.modes.clone())
}
pub fn confirmed(ip:Ipv4Addr,cancel:&AtomicBool,force:bool)->Vec<Mode>{
    let info=hidden("curl.exe").args(["--noproxy","*","-sf","--max-time","2",
        &format!("http://{ip}:8080/gopro/camera/info")]).output().ok()
        .and_then(|output|serde_json::from_slice::<serde_json::Value>(&output.stdout).ok());
    let identity=info.as_ref().and_then(|info|{
        let serial=info["serial_number"].as_str()?.trim();
        let model=info["model_name"].as_str()?.trim();
        let firmware=info["firmware_version"].as_str()?.trim();
        (!serial.is_empty()&&!model.is_empty()&&!firmware.is_empty()).then(||(serial.to_owned(),model.to_owned(),firmware.to_owned()))
    });
    let path=config_path().map(|p|p.with_file_name("confirmed-modes.json"));
    if !force {
        if let (Some(path),Some((serial,model,firmware)))=(&path,&identity){
            if let Some(modes)=lookup(&read(path),serial,model,firmware){return modes;}
        }
    }
    let modes=probe_modes(ip,cancel);
    if !cancel.load(Ordering::Relaxed)&&valid(&modes) {
        if let (Some(path),Some((serial,model,firmware)))=(path,identity){
            let mut cache=read(&path);
            cache.cameras.insert(serial,Entry{model,firmware,modes:modes.clone()});
            if let Ok(bytes)=serde_json::to_vec_pretty(&cache){
                let temp=path.with_extension("tmp");
                let _=std::fs::create_dir_all(path.parent().unwrap())
                    .and_then(|_|std::fs::write(&temp,bytes)).and_then(|_|std::fs::rename(temp,path));
            }
        }
    }
    modes
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cache_roundtrip_matches_camera_and_firmware(){
        let mut cache=Cache{schema:1,..Default::default()};
        cache.cameras.insert("serial-a".into(),Entry{model:"HERO13".into(),firmware:"v1".into(),
            modes:vec![(7,4,1280,720,"29.97".into())]});
        let restored:Cache=serde_json::from_slice(&serde_json::to_vec(&cache).unwrap()).unwrap();
        assert!(lookup(&restored,"serial-a","HERO13","v1").is_some());
        assert!(lookup(&restored,"serial-b","HERO13","v1").is_none());
        assert!(lookup(&restored,"serial-a","HERO13","v2").is_none());
        assert!(!valid(&[]));
        assert!(!valid(&[(7,4,1280,720,"NaN".into())]));
    }
}
