"""Verify the installed Windows Camera Frame Server device, independent of DirectShow."""
import ctypes,pathlib,subprocess,threading,time,sys
folder=pathlib.Path(r"C:\Program Files\GoPro USB")
local="--local" in sys.argv
if local:
    root=pathlib.Path(__file__).resolve().parents[1]
    dllpath=max((root/"target/release/build").glob("gopro-usb/*/out/modern/GoProMF.dll"),key=lambda p:p.stat().st_mtime)
else:
    version=(folder/"camera-version.txt").read_text().strip()
    dllpath=folder/f"GoProMF-{version}.dll"
dll=ctypes.CDLL(str(dllpath))
dll.GoProMFCreate.argtypes=[ctypes.c_int,ctypes.c_wchar_p,ctypes.POINTER(ctypes.c_void_p)]
dll.GoProMFCreate.restype=ctypes.c_long
dll.GoProMFDestroy.argtypes=[ctypes.c_void_p]
dll.GoProMFWrite.argtypes=[ctypes.c_void_p,ctypes.c_void_p,ctypes.c_int,ctypes.c_int]
if local:
    dll.GoProMFLocalCheck.argtypes=[ctypes.c_int,ctypes.POINTER(ctypes.c_int)]
    dll.GoProMFLocalCheck.restype=ctypes.c_long
    for fmt in range(3):
        mean=ctypes.c_int()
        code=dll.GoProMFLocalCheck(fmt,ctypes.byref(mean))
        print(f"Local source: format={fmt} HRESULT=0x{code&0xffffffff:08X} pixel_mean={mean.value}",flush=True)
        assert code>=0 and mean.value>80
elif len(__import__("sys").argv)>1:
    dll.GoProMFCheck.argtypes=[ctypes.c_wchar_p,ctypes.c_int,ctypes.POINTER(ctypes.c_int)]
    dll.GoProMFCheck.restype=ctypes.c_long
    mean=ctypes.c_int()
    code=dll.GoProMFCheck("GoPro USB - MF verification",int(__import__("sys").argv[1]),ctypes.byref(mean))
    print(f"format={__import__('sys').argv[1]} HRESULT=0x{code&0xffffffff:08X} pixel_mean={mean.value}",flush=True)
    assert code>=0
    assert mean.value>80,"No test image reached the Media Foundation client"
else:
    host=ctypes.c_void_p()
    print("Creating Windows virtual camera...",flush=True)
    code=dll.GoProMFCreate(56,"GoPro USB - MF verification",ctypes.byref(host))
    print(f"Creation returned 0x{code&0xffffffff:08X}",flush=True)
    assert code>=0,f"Create HRESULT=0x{code&0xffffffff:08X}"
    stop=threading.Event()
    pixels=ctypes.create_string_buffer(bytes((45,145,220,255))*(1280*720))
    def feed():
        while not stop.wait(1/30):
            dll.GoProMFWrite(host,pixels,1280,720)
    worker=threading.Thread(target=feed);worker.start()
    try:
        for fmt in [0,1,2]:
            result=subprocess.run([__import__("sys").executable,__file__,str(fmt)],capture_output=True,text=True,timeout=30)
            print(result.stdout,flush=True)
            assert result.returncode==0,result.stderr
        print("PnP enumeration, source activation, NV12/YUY2/RGB32 and actual pixel delivery verified.")
    finally:
        stop.set();worker.join();dll.GoProMFDestroy(host)
