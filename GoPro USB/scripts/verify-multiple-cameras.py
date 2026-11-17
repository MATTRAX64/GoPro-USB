"""Exercise two independently registered DirectShow devices, then remove both."""
import ctypes
import pathlib
import subprocess
import threading
import time

root=pathlib.Path(__file__).resolve().parents[1]
exe=root/"target/release/gopro-usb.exe"
slots=[58,59]
names=["GoPro USB - Test A","GoPro USB - Test B"]
try:
    for slot,name in zip(slots,names):
        subprocess.run([str(exe),"--camera-register",str(slot),name,"1"],check=True,timeout=15)
    folder=pathlib.Path.home()/"AppData/Local/GoPro USB/Camera"
    path=max(folder.glob("GoProCapture-*.dll"),key=lambda p:p.stat().st_mtime)
    dll=ctypes.CDLL(str(path))
    dll.GoProSenderCreate.restype=ctypes.c_void_p
    dll.GoProSend.argtypes=[ctypes.c_void_p,ctypes.c_void_p,ctypes.c_int,ctypes.c_int]
    dll.GoProSenderDestroy.argtypes=[ctypes.c_void_p]
    jobs=[]
    for slot,name,color in zip(slots,names,[(25,75,125,255),(145,95,45,255)]):
        child=subprocess.Popen(["ffmpeg","-hide_banner","-loglevel","error","-f","dshow",
            "-video_size","800x450","-framerate","60","-i","video="+name,
            "-frames:v","5","-pix_fmt","rgba","-f","rawvideo","pipe:1"],
            stdout=subprocess.PIPE,stderr=subprocess.PIPE)
        sender=dll.GoProSenderCreate(slot)
        pixels=ctypes.create_string_buffer(bytes(color)*(1920*1080))
        stop=threading.Event()
        def feed(sender=sender,pixels=pixels,stop=stop):
            while not stop.is_set():
                dll.GoProSend(sender,pixels,1920,1080)
                time.sleep(1/60)
        thread=threading.Thread(target=feed)
        thread.start()
        jobs.append((child,sender,thread,stop,color,name))
    try:
        for child,sender,thread,stop,color,name in jobs:
            data,error=child.communicate(timeout=15)
            assert child.returncode==0,error.decode(errors="replace")
            assert len(data)==800*450*4*5,(len(data),error)
            assert bytes(color) in data, name+" received the wrong source"
        print("Two simultaneous DirectShow cameras: registration, HD frame delivery, 60 FPS negotiation and distinct pixel sources verified.")
    finally:
        for child,sender,thread,stop,color,name in jobs:
            stop.set();thread.join();dll.GoProSenderDestroy(sender)
            if child.poll() is None: child.kill();child.communicate()
finally:
    for slot,name in zip(slots,names):
        subprocess.run([str(exe),"--camera-register",str(slot),name,"0"],check=True,timeout=15)
    listing=subprocess.run(["ffmpeg","-hide_banner","-list_devices","true","-f","dshow","-i","dummy"],capture_output=True)
    assert all(name.encode() not in listing.stderr for name in names),"A test camera remains registered"
    print("Both test cameras were removed from device enumeration.")
