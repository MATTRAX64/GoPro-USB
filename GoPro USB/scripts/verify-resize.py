"""Check resize and top/bottom orientation with a spatial test image via DirectShow."""
import ctypes,pathlib,subprocess,threading,time
root=pathlib.Path(__file__).resolve().parents[1]
exe=root/"target/release/gopro-usb.exe"
slot=57
name="GoPro USB - Spatial validation"
colors=[bytes((240,20,30,255)),bytes((15,220,45,255)),bytes((25,40,230,255)),bytes((230,210,25,255))]
subprocess.run([str(exe),"--camera-register",str(slot),name,"1"],check=True)
folder=pathlib.Path.home()/"AppData/Local/GoPro USB/Camera"
dll=ctypes.CDLL(str(max(folder.glob("GoProCapture-*.dll"),key=lambda p:p.stat().st_mtime)))
dll.GoProSenderCreate.restype=ctypes.c_void_p
dll.GoProSend.argtypes=[ctypes.c_void_p,ctypes.c_void_p,ctypes.c_int,ctypes.c_int]
dll.GoProSenderDestroy.argtypes=[ctypes.c_void_p]
try:
    for iw,ih,ow,oh in [(1920,1080,640,360),(854,480,1280,720),(1280,720,854,480),(1920,1080,2560,1440),(3840,2160,1920,1080),(1280,720,3840,2160)]:
        rows=[colors[0]*(iw//2)+colors[1]*(iw-iw//2), colors[2]*(iw//2)+colors[3]*(iw-iw//2)]
        pixels=ctypes.create_string_buffer(rows[0]*(ih//2)+rows[1]*(ih-ih//2))
        sender=dll.GoProSenderCreate(slot)
        stop=threading.Event()
        def feed():
            while not stop.is_set():
                dll.GoProSend(sender,pixels,iw,ih)
                time.sleep(1/30)
        worker=threading.Thread(target=feed);worker.start()
        try:
            result=subprocess.run(["ffmpeg","-hide_banner","-loglevel","error","-f","dshow","-pixel_format","bgr0","-video_size",f"{ow}x{oh}",
                "-i","video="+name,"-frames:v","3","-pix_fmt","rgba","-f","rawvideo","pipe:1"],
                capture_output=True,timeout=20)
            assert result.returncode==0,result.stderr.decode(errors="replace")
            data=result.stdout[-ow*oh*4:]
            assert len(data)==ow*oh*4
            for yf in [0.1,0.25,0.4,0.6,0.75,0.9]:
                for xf in [0.1,0.25,0.4,0.6,0.75,0.9]:
                    offset=(int(yf*oh)*ow+int(xf*ow))*4
                    expected=colors[(2 if yf<.5 else 0)+(1 if xf>.5 else 0)]
                    assert data[offset:offset+3]==expected[:3],(iw,ih,ow,oh,xf,yf,list(data[offset:offset+4]),list(expected))
            print(f"{iw}x{ih} -> {ow}x{oh}: orientation and 36 spatial samples OK")
        finally:
            stop.set();worker.join();dll.GoProSenderDestroy(sender)
finally:
    subprocess.run([str(exe),"--camera-register",str(slot),name,"0"],check=True)
