"use strict";
const $ = id => document.getElementById(id);
const send = message => window.ipc.postMessage(JSON.stringify(message));
$("recheck-modes").onclick=()=>send({command:"recheck_modes"});
$("install-camera").onclick=()=>send({command:"install_camera"});
const defaults = { brightness:0, contrast:1, saturation:1, gamma:1, sharpness:0, zoom:1, mirror:false, flip:false };
const definitions = [
  ["brightness","Luminosité",-.5,.5,.01], ["contrast","Contraste",.5,2,.01],
  ["saturation","Saturation",0,2,.01], ["gamma","Gamma",.5,2,.01],
  ["sharpness","Netteté",0,1,.01], ["zoom","Zoom numérique",1,3,.01]
];
let values = {...defaults}, last = null, pendingImage = null, imageTimer = 0, slowFrames = 0;
function updateImage() {
  pendingImage = {...values};
  if (imageTimer) return;
  imageTimer = setTimeout(() => {
    imageTimer = 0;
    const value = pendingImage;
    pendingImage = null;
    send({command:"image",value});
  }, 30);
}
for (const [key,label,min,max,step] of definitions) {
  const row = document.createElement("label");
  const title = document.createElement("span"); title.className = "filter-title";
  const text = document.createElement("span"); text.dataset.label = label; text.textContent = label;
  const output = document.createElement("output"); output.id = key + "-value";
  title.append(text,output);
  const input = document.createElement("input");
  Object.assign(input,{id:key,type:"range",min,max,step,value:defaults[key]});
  input.addEventListener("input",()=>{values[key]=Number(input.value);output.textContent=Number(input.value).toFixed(2);updateImage()});
  input.addEventListener("change",updateImage);
  row.append(title,input);$("filters").append(row);
}
for (const id of ["resolution","fps","lens","gpu","lang","camera"]) {
  $(id).addEventListener("change",()=>{
    const value = $(id).value;
    send({command:({lang:"language",camera:"camera"})[id]||id,
      value:id==="camera"?value:id==="gpu"&&value==="auto"?null:Number(value)});
  });
}
for (const id of ["mirror","flip"]) $(id).addEventListener("change",()=>{
  values[id]=$(id).checked;send({command:"image",value:values});
});
$("reset").onclick=()=>{values={...defaults};send({command:"image",value:values});drawImageSettings()};
$("refresh").onclick=()=>send({command:"refresh"});
$("retry").onclick=()=>send({command:"retry"});
$("address").onsubmit=e=>{
  e.preventDefault();
  const ip=$("ip").value.trim();
  if(!/^(\d{1,3}\.){3}\d{1,3}$/.test(ip)||ip.split(".").some(n=>Number(n)>255)) {
    $("ip").setCustomValidity("IPv4");$("ip").reportValidity();return;
  }
  $("ip").setCustomValidity("");send({command:"camera",value:ip});send({command:"refresh"});
};
function options(id, rows, selected) {
  const control=$(id), signature=JSON.stringify(rows);
  if(control.dataset.options!==signature) {
    control.replaceChildren(...rows.map(([value,text])=>new Option(text,String(value))));
    control.dataset.options=signature;
  }
  control.value=String(selected);
}
function drawImageSettings() {
  for(const [key] of definitions) {
    if(document.activeElement!==$(key)) $(key).value=values[key];
    $(key+"-value").textContent=Number(values[key]).toFixed(2);
  }
  for(const id of ["mirror","flip"]) $(id).checked=values[id];
}
async function updateState() {
  try {
    const response=await fetch("/state");
    if(!response.ok)return;
    const state=await response.json(); last=state;
    if(!document.activeElement?.matches('input')) values={...state.image};
    drawImageSettings();
    document.documentElement.lang=["fr","en","es","ja","ru","de"][state.lang];
    document.querySelectorAll("[data-label]").forEach(node=>{node.textContent=state.labels[node.dataset.label]||node.dataset.label});
    options("lang",state.languages.map((name,index)=>[index,name]),state.lang);
    options("camera",state.cameras.length?state.cameras.map(ip=>[ip,(state.virtual_cameras.find(camera=>camera[0]===ip)||[])[1]||"GoPro · "+ip]):[["","—"]],state.ip);
    options("resolution",state.resolutions,state.resolution);
    options("fps",state.cadence,state.fps);
    options("lens",state.lenses.length?state.lenses:[["","—"]],state.lens);
    $("lens").disabled=!state.lenses.length||state.working;
    $("gpu-label").textContent=["Décodage vidéo · filtres sur CPU","Video decoding · CPU filters","Decodificación · filtros en CPU","動画デコード · フィルターは CPU","Декодирование · фильтры на CPU","Videodekodierung · CPU-Filter"][state.lang];
    options("gpu",[["auto","GPU · Auto"],[4294967295,"CPU"],...state.adapters.map((name,id)=>[id,name])],state.gpu===null?"auto":state.gpu);
    if(document.activeElement!==$("ip")) $("ip").value=state.ip;

    slowFrames = state.fps_actual>0 && state.fps_actual<state.fps*.85 ? slowFrames+1 : 0;
    const slowLabels=[
      "Débit inférieur à la cadence demandée","Output below requested frame rate","Salida inferior a los FPS solicitados",
      "指定したフレームレートに達していません","Частота ниже заданной","Bildrate liegt unter dem gewählten Wert"
    ];
    $("status").textContent=slowFrames>=8?slowLabels[state.lang]:state.status;
    $("status").style.color=slowFrames>=8?"#F5C700":state.status_color;
    $("metadata").textContent=[state.source,state.fps_actual>0?state.fps_actual.toFixed(1)+" FPS":""].filter(Boolean).join(" · ");
    $("metrics").replaceChildren(...state.usage.split("   ·   ").filter(Boolean).map(text=>{const item=document.createElement("span");item.textContent=text;return item}));
    $("recheck-modes").textContent=["Revérifier les modes caméra","Rescan camera modes","Volver a detectar modos","カメラモードを再検出","Перепроверить режимы камеры","Kameramodi erneut prüfen"][state.lang];
    $("recheck-modes").disabled=state.working;
    $("install-camera").hidden=state.modern_installed;
    $("install-camera").textContent=["Installer la caméra Windows","Install Windows camera","Instalar cámara Windows","Windows カメラをインストール","Установить камеру Windows","Windows-Kamera installieren"][state.lang];
    const virtual = state.virtual_cameras.find(camera=>camera[0]===state.ip);
    const message = (state.virtual_errors||[]).filter(Boolean).join("\n");
    $("virtual-status").textContent = message || (virtual ? virtual[1]+(virtual[2]?(state.modern_installed?" · Windows ✓":" · DirectShow ✓"):" · …") : "");
    const languages=[
      ["Rechercher","Résolution","Réessayer"],["Search","Resolution","Retry"],["Buscar","Resolución","Reintentar"],
      ["検索","解像度","再試行"],["Найти","Разрешение","Повторить"],["Suchen","Auflösung","Erneut versuchen"]
    ][state.lang];
    $("refresh").textContent=languages[0]+(state.working?" …":"");
    $("refresh").disabled=state.working;
    $("resolution-label").textContent=languages[1];$("retry").textContent=languages[2];
    $("retry").hidden=!state.retry;
  } catch(error) { /* Rust can still be initializing the first snapshot. */ }
  finally { setTimeout(updateState,250); }
}
const canvas=$("frame"), ctx=canvas.getContext("2d",{alpha:false});
let sequence=0;
async function updateFrame() {
  try {
    const response=await fetch("/frame?"+sequence);
    if(response.status===200) {
      const bytes=new Uint8ClampedArray(await response.arrayBuffer());
      if(bytes.length===800*450*4) {
        ctx.putImageData(new ImageData(bytes,800,450),0,0);
        sequence=Number(response.headers.get("X-GoPro-Sequence"));
        canvas.hidden=false;$("placeholder").hidden=true;
      }
    } else if(last&&last.fps_actual===0) {
      canvas.hidden=true;$("placeholder").hidden=false;
    }
  } catch(error) {}
  finally { setTimeout(updateFrame,10); }
}
updateState();updateFrame();

