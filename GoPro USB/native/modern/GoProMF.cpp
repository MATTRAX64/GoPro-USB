// GoPro USB Media Foundation virtual camera. Windows 11+.
// Independent implementation of the documented IMFMediaSourceEx lifecycle.
#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>
#include <mfapi.h>
#include <mfidl.h>
#include <mferror.h>
#include <mfreadwrite.h>
#include <mfvirtualcamera.h>
#include <ks.h>
#include <ksmedia.h>
#include <ksproxy.h>
#include <wrl.h>
#include <propvarutil.h>
#include <atomic>
#include <mutex>
#include <condition_variable>
#include <deque>
#include <thread>
#include <chrono>
#include <memory>
#include <algorithm>
#include "ipc.h"
using namespace Microsoft::WRL;
static void Trace(const char* label,HRESULT result,int line) {
    char message[320];int length=sprintf_s(message,"%s hr=%08lx line=%d pid=%lu\n",label,result,line,GetCurrentProcessId());
    PSECURITY_DESCRIPTOR descriptor=nullptr;
    ConvertStringSecurityDescriptorToSecurityDescriptorW(L"D:P(A;;GA;;;SY)(A;;GA;;;LS)(A;;GR;;;BU)",SDDL_REVISION_1,&descriptor,nullptr);
    SECURITY_ATTRIBUTES sa={sizeof(sa),descriptor,FALSE};
    HANDLE file=CreateFileW(L"C:\\Windows\\Temp\\GoProUSB-MF.log",FILE_APPEND_DATA,FILE_SHARE_READ|FILE_SHARE_WRITE,&sa,OPEN_ALWAYS,FILE_ATTRIBUTE_NORMAL,nullptr);
    if(file!=INVALID_HANDLE_VALUE){DWORD written;WriteFile(file,message,length,&written,nullptr);CloseHandle(file);}
    if(descriptor)LocalFree(descriptor);
}
#define CHECK(call) do {HRESULT check_hr=(call);if(FAILED(check_hr)){Trace(#call,check_hr,__LINE__);return check_hr;}}while(0)
static std::atomic<long> objects{0};
static HMODULE module;
static const GUID USER_SID={0x2f1da840,0xd378,0x42fb,{0x93,0xab,0x53,0x21,0x92,0x7a,0x60,0x01}};
static GUID SourceGuid(int slot) {
    GUID id={0x39a43610,0x1c64,0x4a42,{0xb7,0xa6,0x34,0x0d,0x75,0x53,0x00,0x00}};
    id.Data4[7]=(BYTE)slot;return id;
}
struct Counted {Counted(){objects++;}~Counted(){objects--;}};
static HRESULT MakeType(UINT32 w,UINT32 h,UINT32 fps,const GUID& format,IMFMediaType** output) {
    ComPtr<IMFMediaType> type;CHECK(MFCreateMediaType(&type));
    CHECK(type->SetGUID(MF_MT_MAJOR_TYPE,MFMediaType_Video));
    CHECK(type->SetGUID(MF_MT_SUBTYPE,format));
    CHECK(MFSetAttributeSize(type.Get(),MF_MT_FRAME_SIZE,w,h));
    CHECK(MFSetAttributeRatio(type.Get(),MF_MT_FRAME_RATE,fps,1));
    CHECK(MFSetAttributeRatio(type.Get(),MF_MT_PIXEL_ASPECT_RATIO,1,1));
    CHECK(type->SetUINT32(MF_MT_INTERLACE_MODE,MFVideoInterlace_Progressive));
    CHECK(type->SetUINT32(MF_MT_ALL_SAMPLES_INDEPENDENT,TRUE));
    CHECK(type->SetUINT32(MF_MT_FIXED_SIZE_SAMPLES,TRUE));
    UINT32 bytes=format==MFVideoFormat_NV12?w*h*3/2:format==MFVideoFormat_YUY2?w*h*2:w*h*4;
    CHECK(type->SetUINT32(MF_MT_SAMPLE_SIZE,bytes));
    CHECK(type->SetUINT32(MF_MT_AVG_BITRATE,bytes*8*fps));
    CHECK(type->SetUINT32(MF_MT_DEFAULT_STRIDE,format==MFVideoFormat_NV12?w:format==MFVideoFormat_YUY2?w*2:w*4));
    CHECK(type->SetUINT32(MF_MT_VIDEO_NOMINAL_RANGE,format==MFVideoFormat_RGB32?MFNominalRange_0_255:MFNominalRange_16_235));
    return type.CopyTo(output);
}
#define EVENT_METHODS \
STDMETHODIMP GetEvent(DWORD flags,IMFMediaEvent** event)override{return events->GetEvent(flags,event);} \
STDMETHODIMP BeginGetEvent(IMFAsyncCallback* callback,IUnknown* state)override{return events->BeginGetEvent(callback,state);} \
STDMETHODIMP EndGetEvent(IMFAsyncResult* result,IMFMediaEvent** event)override{return events->EndGetEvent(result,event);} \
STDMETHODIMP QueueEvent(MediaEventType type,REFGUID extended,HRESULT status,const PROPVARIANT* value)override{return events->QueueEventParamVar(type,extended,status,value);}

struct Stream final:RuntimeClass<RuntimeClassFlags<ClassicCom>,ChainInterfaces<IMFMediaStream2,IMFMediaStream,IMFMediaEventGenerator>>,Counted {
    ComPtr<IMFMediaEventQueue> events;
    ComPtr<IMFStreamDescriptor> descriptor;
    ComPtr<IMFAttributes> attributes;
    ComPtr<IMFMediaSource> parent;
    std::mutex lock;
    std::condition_variable wake;
    std::deque<ComPtr<IUnknown>> tokens;
    bool shutdown=false, running=false, announced=false;
    UINT32 width=1280,height=720,fps=30,epoch=0;
    GUID format=MFVideoFormat_NV12;
    std::thread worker;
    FrameChannel channel;
    Stream(const std::wstring& sid,int slot,bool local=false):channel(sid,slot,local){}
    ~Stream(){Close();}
    HRESULT Initialize(IMFMediaSource* source) {
        parent=source;CHECK(MFCreateEventQueue(&events));
        CHECK(MFCreateAttributes(&attributes,8));
        CHECK(attributes->SetGUID(MF_DEVICESTREAM_STREAM_CATEGORY,PINNAME_VIDEO_CAPTURE));
        CHECK(attributes->SetUINT32(MF_DEVICESTREAM_STREAM_ID,0));
        CHECK(attributes->SetUINT32(MF_DEVICESTREAM_FRAMESERVER_SHARED,TRUE));
        CHECK(attributes->SetUINT32(MF_DEVICESTREAM_ATTRIBUTE_FRAMESOURCE_TYPES,MFFrameSourceTypes_Color));
        std::vector<ComPtr<IMFMediaType>> owned;std::vector<IMFMediaType*> types;
        const UINT32 sizes[][2]={{1280,720},{1920,1080},{640,360},{854,480},{2560,1440},{3840,2160}};
        const UINT32 rates[]={30,15,24,50,60,75,120};
        for(const auto& sub:{MFVideoFormat_NV12,MFVideoFormat_YUY2,MFVideoFormat_RGB32})
            for(const auto& size:sizes)for(UINT32 rate:rates){
                ComPtr<IMFMediaType> type;CHECK(MakeType(size[0],size[1],rate,sub,&type));
                types.push_back(type.Get());owned.push_back(type);
            }
        CHECK(MFCreateStreamDescriptor(0,(DWORD)types.size(),types.data(),&descriptor));
        CHECK(attributes->CopyAllItems(descriptor.Get()));
        ComPtr<IMFMediaTypeHandler> handler;CHECK(descriptor->GetMediaTypeHandler(&handler));
        CHECK(handler->SetCurrentMediaType(types[0]));
        worker=std::thread([this]{Run();});return S_OK;
    }
    EVENT_METHODS
    STDMETHODIMP GetMediaSource(IMFMediaSource** output)override {
        std::lock_guard<std::mutex> guard(lock);
        return shutdown?MF_E_SHUTDOWN:parent.CopyTo(output);
    }
    STDMETHODIMP GetStreamDescriptor(IMFStreamDescriptor** output)override{return descriptor.CopyTo(output);}
    STDMETHODIMP RequestSample(IUnknown* token)override {
        std::lock_guard<std::mutex> guard(lock);
        if(shutdown)return MF_E_SHUTDOWN;
        if(!running)return MF_E_INVALIDREQUEST;
        if(tokens.size()>=8)return MF_E_NOTACCEPTING;
        tokens.emplace_back(token);wake.notify_one();return S_OK;
    }
    STDMETHODIMP SetStreamState(MF_STREAM_STATE state)override {
        Trace("Stream::SetStreamState",(HRESULT)state,__LINE__);
        std::lock_guard<std::mutex> guard(lock);
        if(shutdown)return MF_E_SHUTDOWN;
        running=state==MF_STREAM_STATE_RUNNING;
        if(!running){tokens.clear();epoch++;}
        wake.notify_all();return S_OK;
    }
    STDMETHODIMP GetStreamState(MF_STREAM_STATE* state)override {
        if(!state)return E_POINTER;std::lock_guard<std::mutex> guard(lock);
        if(shutdown)return MF_E_SHUTDOWN;
        *state=running?MF_STREAM_STATE_RUNNING:MF_STREAM_STATE_STOPPED;return S_OK;
    }
    HRESULT Start(IMFMediaType* type) {
        Trace("Stream::Start",S_OK,__LINE__);
        std::lock_guard<std::mutex> guard(lock);
        if(shutdown)return MF_E_SHUTDOWN;
        UINT32 denominator;
        CHECK(MFGetAttributeSize(type,MF_MT_FRAME_SIZE,&width,&height));
        CHECK(MFGetAttributeRatio(type,MF_MT_FRAME_RATE,&fps,&denominator));
        if(!denominator||fps%denominator)return MF_E_INVALIDMEDIATYPE;
        fps/=denominator;CHECK(type->GetGUID(MF_MT_SUBTYPE,&format));
        if(width<4||height<4||width>3840||height>2160||width%2||height%2||fps<15||fps>120||
            (format!=MFVideoFormat_NV12&&format!=MFVideoFormat_YUY2&&format!=MFVideoFormat_RGB32))return MF_E_INVALIDMEDIATYPE;
        ComPtr<IMFMediaTypeHandler> handler;CHECK(descriptor->GetMediaTypeHandler(&handler));
        CHECK(handler->SetCurrentMediaType(type));
        running=true;epoch++;tokens.clear();wake.notify_all();
        PROPVARIANT started;InitPropVariantFromInt64(MFGetSystemTime(),&started);
        return events->QueueEventParamVar(MEStreamStarted,GUID_NULL,S_OK,&started);
    }
    HRESULT Stop() {
        std::lock_guard<std::mutex> guard(lock);
        running=false;epoch++;tokens.clear();wake.notify_all();
        return events->QueueEventParamVar(MEStreamStopped,GUID_NULL,S_OK,nullptr);
    }
    void Close() {
        {std::lock_guard<std::mutex> guard(lock);shutdown=true;running=false;tokens.clear();wake.notify_all();}
        if(worker.joinable())worker.join();
        if(events)events->Shutdown();
        parent.Reset();
    }
    static BYTE Clip(int n){return (BYTE)std::clamp(n,0,255);}
    void Run() {
        CoInitializeEx(nullptr,COINIT_MULTITHREADED);
        auto due=std::chrono::steady_clock::now();
        std::vector<BYTE> image;
        UINT32 iw=0,ih=0,previousEpoch=0;
        std::unique_lock<std::mutex> guard(lock);
        while(!shutdown){
            wake.wait(guard,[this]{return shutdown||(running&&!tokens.empty());});
            if(shutdown)break;
            if(previousEpoch!=epoch){due=std::chrono::steady_clock::now();previousEpoch=epoch;}
            if(wake.wait_until(guard,due,[this]{return shutdown||!running;}))continue;
            auto token=tokens.front();tokens.pop_front();
            UINT32 w=width,h=height,rate=fps,generation=epoch;GUID sub=format;
            guard.unlock();
            HRESULT result=S_OK;
            ComPtr<IMFSample> sample;ComPtr<IMFMediaBuffer> buffer;
            UINT32 size=sub==MFVideoFormat_NV12?w*h*3/2:sub==MFVideoFormat_YUY2?w*h*2:w*h*4;
            result=MFCreateSample(&sample);
            if(SUCCEEDED(result))result=MFCreate2DMediaBuffer(w,h,sub.Data1,FALSE,&buffer);
            BYTE* output=nullptr;
            if(SUCCEEDED(result))result=buffer->Lock(&output,nullptr,nullptr);
            if(SUCCEEDED(result)){
                bool live=channel.read(image,iw,ih);
                if(!live){iw=w;ih=h;image.assign((size_t)w*h*4,0);}
                auto pixel=[&](UINT32 x,UINT32 y)->const BYTE*{
                    // Match the existing capture orientation; mirrors remain user controls.
                    return image.data()+((size_t)(ih-1-(UINT64)y*ih/h)*iw+(UINT64)x*iw/w)*4;
                };
                if(sub==MFVideoFormat_RGB32){
                    for(UINT32 y=0;y<h;y++)for(UINT32 x=0;x<w;x++){
                        const BYTE* p=pixel(x,y);BYTE* q=output+((size_t)y*w+x)*4;
                        q[0]=p[2];q[1]=p[1];q[2]=p[0];q[3]=255;
                    }
                }else{
                    for(UINT32 y=0;y<h;y+=2)for(UINT32 x=0;x<w;x+=2){
                        int sumR=0,sumG=0,sumB=0;
                        for(UINT32 dy=0;dy<2;dy++)for(UINT32 dx=0;dx<2;dx++){
                            const BYTE* p=pixel(x+dx,y+dy);int r=p[0],g=p[1],b=p[2];
                            BYTE Y=Clip(((66*r+129*g+25*b+128)>>8)+16);
                            if(sub==MFVideoFormat_NV12)output[(size_t)(y+dy)*w+x+dx]=Y;
                            else {
                                BYTE* q=output+((size_t)(y+dy)*w+x)*2;
                                q[dx*2]=Y;
                                if(dx==0){
                                    const BYTE* p2=pixel(x+1,y+dy);
                                    int rr=(r+p2[0])/2,gg=(g+p2[1])/2,bb=(b+p2[2])/2;
                                    q[1]=Clip(((-38*rr-74*gg+112*bb+128)>>8)+128);
                                    q[3]=Clip(((112*rr-94*gg-18*bb+128)>>8)+128);
                                }
                            }
                            sumR+=r;sumG+=g;sumB+=b;
                        }
                        if(sub==MFVideoFormat_NV12){
                            int r=sumR/4,g=sumG/4,b=sumB/4;BYTE* q=output+(size_t)w*h+(size_t)(y/2)*w+x;
                            q[0]=Clip(((-38*r-74*g+112*b+128)>>8)+128);
                            q[1]=Clip(((112*r-94*g-18*b+128)>>8)+128);
                        }
                    }
                }
                buffer->Unlock();buffer->SetCurrentLength(size);
                sample->AddBuffer(buffer.Get());sample->SetSampleTime(MFGetSystemTime());
                sample->SetSampleDuration(10000000/rate);
                sample->SetUINT32(MFSampleExtension_CleanPoint,TRUE);
                if(token)sample->SetUnknown(MFSampleExtension_Token,token.Get());
            }
            guard.lock();
            if(!shutdown&&running&&generation==epoch){
                if(SUCCEEDED(result))events->QueueEventParamUnk(MEMediaSample,GUID_NULL,S_OK,sample.Get());
                else {Trace("Sample",result,__LINE__);events->QueueEventParamVar(MEError,GUID_NULL,result,nullptr);}
            }
            due+=std::chrono::nanoseconds(1000000000/rate);
            auto now=std::chrono::steady_clock::now();
            if(due<now)due=now;
        }
        guard.unlock();CoUninitialize();
    }
};

struct Source final:RuntimeClass<RuntimeClassFlags<ClassicCom>,
    ChainInterfaces<IMFMediaSourceEx,IMFMediaSource,IMFMediaEventGenerator>,IMFGetService,IKsControl,IMFSampleAllocatorControl>,Counted {
    std::recursive_mutex lock;
    bool shutdown=false;
    ComPtr<IMFMediaEventQueue> events;
    ComPtr<IMFAttributes> attributes;
    ComPtr<IMFPresentationDescriptor> presentation;
    ComPtr<Stream> stream;
    HRESULT Initialize(const std::wstring& sid,int slot,bool local=false) {
        CHECK(MFCreateAttributes(&attributes,8));
        
        CHECK(attributes->SetGUID(MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID));
        CHECK(MFCreateEventQueue(&events));
        stream=Make<Stream>(sid,slot,local);if(!stream)return E_OUTOFMEMORY;
        CHECK(stream->Initialize(this));
        ComPtr<IMFStreamDescriptor> descriptor;CHECK(stream->GetStreamDescriptor(&descriptor));
        IMFStreamDescriptor* list[]={descriptor.Get()};
        CHECK(MFCreatePresentationDescriptor(1,list,&presentation));
        return presentation->SelectStream(0);
    }
    EVENT_METHODS
    STDMETHODIMP GetCharacteristics(DWORD* flags)override{if(!flags)return E_POINTER;*flags=MFMEDIASOURCE_IS_LIVE;return shutdown?MF_E_SHUTDOWN:S_OK;}
    STDMETHODIMP CreatePresentationDescriptor(IMFPresentationDescriptor** output)override{
        std::lock_guard<std::recursive_mutex> guard(lock);return shutdown?MF_E_SHUTDOWN:presentation->Clone(output);
    }
    STDMETHODIMP Start(IMFPresentationDescriptor* descriptor,const GUID* time,const PROPVARIANT* position)override {
        Trace("Source::Start",S_OK,__LINE__);
        std::lock_guard<std::recursive_mutex> guard(lock);
        if(shutdown)return MF_E_SHUTDOWN;
        if(!descriptor||!position)return E_INVALIDARG;
        if(time&&*time!=GUID_NULL)return MF_E_UNSUPPORTED_TIME_FORMAT;
        BOOL selected=FALSE;DWORD count=0;CHECK(descriptor->GetStreamDescriptorCount(&count));
        if(count!=1)return E_INVALIDARG;
        ComPtr<IMFStreamDescriptor> sd;CHECK(descriptor->GetStreamDescriptorByIndex(0,&selected,&sd));
        if(!selected)return E_INVALIDARG;
        DWORD id;CHECK(sd->GetStreamIdentifier(&id));if(id!=0)return E_INVALIDARG;
        ComPtr<IMFMediaTypeHandler> handler;ComPtr<IMFMediaType> type;
        CHECK(sd->GetMediaTypeHandler(&handler));CHECK(handler->GetCurrentMediaType(&type));
        CHECK(events->QueueEventParamUnk(stream->announced?MEUpdatedStream:MENewStream,GUID_NULL,S_OK,stream.Get()));
        stream->announced=true;CHECK(stream->Start(type.Get()));
        PROPVARIANT started;InitPropVariantFromInt64(MFGetSystemTime(),&started);
        return events->QueueEventParamVar(MESourceStarted,GUID_NULL,S_OK,&started);
    }
    STDMETHODIMP Stop()override{
        std::lock_guard<std::recursive_mutex> guard(lock);if(shutdown)return MF_E_SHUTDOWN;
        CHECK(stream->Stop());return events->QueueEventParamVar(MESourceStopped,GUID_NULL,S_OK,nullptr);
    }
    STDMETHODIMP Pause()override{return MF_E_INVALID_STATE_TRANSITION;}
    STDMETHODIMP Shutdown()override{
        std::lock_guard<std::recursive_mutex> guard(lock);if(shutdown)return S_OK;
        shutdown=true;if(stream){stream->Close();stream.Reset();}
        if(events)events->Shutdown();presentation.Reset();return S_OK;
    }
    STDMETHODIMP GetSourceAttributes(IMFAttributes** output)override{return attributes.CopyTo(output);}
    STDMETHODIMP GetStreamAttributes(DWORD id,IMFAttributes** output)override{
        std::lock_guard<std::recursive_mutex> guard(lock);if(shutdown)return MF_E_SHUTDOWN;
        return id==0?stream->attributes.CopyTo(output):MF_E_INVALIDSTREAMNUMBER;
    }
    STDMETHODIMP SetD3DManager(IUnknown*)override{return E_NOTIMPL;}
    STDMETHODIMP GetService(REFGUID,REFIID,void** output)override{if(!output)return E_POINTER;*output=nullptr;return MF_E_UNSUPPORTED_SERVICE;}
    STDMETHODIMP KsProperty(PKSPROPERTY,ULONG,LPVOID,ULONG,ULONG* count)override{if(count)*count=0;return HRESULT_FROM_WIN32(ERROR_SET_NOT_FOUND);}
    STDMETHODIMP KsMethod(PKSMETHOD,ULONG,LPVOID,ULONG,ULONG* count)override{if(count)*count=0;return HRESULT_FROM_WIN32(ERROR_SET_NOT_FOUND);}
    STDMETHODIMP KsEvent(PKSEVENT,ULONG,LPVOID,ULONG,ULONG* count)override{if(count)*count=0;return HRESULT_FROM_WIN32(ERROR_SET_NOT_FOUND);}
    STDMETHODIMP SetDefaultAllocator(DWORD,IUnknown*)override{return E_NOTIMPL;}
    STDMETHODIMP GetAllocatorUsage(DWORD id,DWORD* input,MFSampleAllocatorUsage* usage)override{
        if(!input||!usage)return E_POINTER;if(id)return MF_E_INVALIDSTREAMNUMBER;
        *input=0;*usage=MFSampleAllocatorUsage_UsesCustomAllocator;return S_OK;
    }
};

struct Activate final:RuntimeClass<RuntimeClassFlags<ClassicCom>,ChainInterfaces<IMFActivate,IMFAttributes>>,Counted {
    ComPtr<IMFAttributes> attributes;
    ComPtr<Source> source;
    std::mutex lock;
    int slot;
    Activate(int number):slot(number){}
    HRESULT Initialize(){return MFCreateAttributes(&attributes,8);}
    STDMETHODIMP GetItem(REFGUID key,PROPVARIANT* value)override{return attributes->GetItem(key,value);}
    STDMETHODIMP GetItemType(REFGUID key,MF_ATTRIBUTE_TYPE* value)override{return attributes->GetItemType(key,value);}
    STDMETHODIMP CompareItem(REFGUID key,REFPROPVARIANT value,BOOL* equal)override{return attributes->CompareItem(key,value,equal);}
    STDMETHODIMP Compare(IMFAttributes* other,MF_ATTRIBUTES_MATCH_TYPE kind,BOOL* equal)override{return attributes->Compare(other,kind,equal);}
    STDMETHODIMP GetUINT32(REFGUID key,UINT32* value)override{return attributes->GetUINT32(key,value);}
    STDMETHODIMP GetUINT64(REFGUID key,UINT64* value)override{return attributes->GetUINT64(key,value);}
    STDMETHODIMP GetDouble(REFGUID key,double* value)override{return attributes->GetDouble(key,value);}
    STDMETHODIMP GetGUID(REFGUID key,GUID* value)override{return attributes->GetGUID(key,value);}
    STDMETHODIMP GetStringLength(REFGUID key,UINT32* length)override{return attributes->GetStringLength(key,length);}
    STDMETHODIMP GetString(REFGUID key,LPWSTR value,UINT32 size,UINT32* length)override{return attributes->GetString(key,value,size,length);}
    STDMETHODIMP GetAllocatedString(REFGUID key,LPWSTR* value,UINT32* length)override{return attributes->GetAllocatedString(key,value,length);}
    STDMETHODIMP GetBlobSize(REFGUID key,UINT32* size)override{return attributes->GetBlobSize(key,size);}
    STDMETHODIMP GetBlob(REFGUID key,UINT8* value,UINT32 size,UINT32* length)override{return attributes->GetBlob(key,value,size,length);}
    STDMETHODIMP GetAllocatedBlob(REFGUID key,UINT8** value,UINT32* size)override{return attributes->GetAllocatedBlob(key,value,size);}
    STDMETHODIMP GetUnknown(REFGUID key,REFIID iid,LPVOID* value)override{return attributes->GetUnknown(key,iid,value);}
    STDMETHODIMP SetItem(REFGUID key,REFPROPVARIANT value)override{return attributes->SetItem(key,value);}
    STDMETHODIMP DeleteItem(REFGUID key)override{return attributes->DeleteItem(key);}
    STDMETHODIMP DeleteAllItems()override{return attributes->DeleteAllItems();}
    STDMETHODIMP SetUINT32(REFGUID key,UINT32 value)override{return attributes->SetUINT32(key,value);}
    STDMETHODIMP SetUINT64(REFGUID key,UINT64 value)override{return attributes->SetUINT64(key,value);}
    STDMETHODIMP SetDouble(REFGUID key,double value)override{return attributes->SetDouble(key,value);}
    STDMETHODIMP SetGUID(REFGUID key,REFGUID value)override{return attributes->SetGUID(key,value);}
    STDMETHODIMP SetString(REFGUID key,LPCWSTR value)override{return attributes->SetString(key,value);}
    STDMETHODIMP SetBlob(REFGUID key,const UINT8* value,UINT32 size)override{return attributes->SetBlob(key,value,size);}
    STDMETHODIMP SetUnknown(REFGUID key,IUnknown* value)override{return attributes->SetUnknown(key,value);}
    STDMETHODIMP LockStore()override{return attributes->LockStore();}
    STDMETHODIMP UnlockStore()override{return attributes->UnlockStore();}
    STDMETHODIMP GetCount(UINT32* count)override{return attributes->GetCount(count);}
    STDMETHODIMP GetItemByIndex(UINT32 index,GUID* key,PROPVARIANT* value)override{return attributes->GetItemByIndex(index,key,value);}
    STDMETHODIMP CopyAllItems(IMFAttributes* destination)override{return attributes->CopyAllItems(destination);}

    STDMETHODIMP ActivateObject(REFIID iid,void** output)override {
        if(!output)return E_POINTER;*output=nullptr;
        std::lock_guard<std::mutex> guard(lock);
        if(!source){
            wchar_t sid[192]={};
            CHECK(attributes->GetString(USER_SID,sid,192,nullptr));
            source=Make<Source>();if(!source)return E_OUTOFMEMORY;
            HRESULT result=source->Initialize(sid,slot);
            if(FAILED(result)){source->Shutdown();source.Reset();return result;}
        }
        return source.CopyTo(iid,output);
    }
    STDMETHODIMP ShutdownObject()override{
        std::lock_guard<std::mutex> guard(lock);
        if(source){source->Shutdown();source.Reset();}return S_OK;
    }
    STDMETHODIMP DetachObject()override{std::lock_guard<std::mutex> guard(lock);source.Reset();return S_OK;}
};
struct Factory final:RuntimeClass<RuntimeClassFlags<ClassicCom>,IClassFactory>,Counted {
    int slot;Factory(int number):slot(number){}
    STDMETHODIMP CreateInstance(IUnknown* outer,REFIID iid,void** output)override {
        if(outer)return CLASS_E_NOAGGREGATION;
        auto activation=Make<Activate>(slot);if(!activation)return E_OUTOFMEMORY;
        CHECK(activation->Initialize());return activation.CopyTo(iid,output);
    }
    STDMETHODIMP LockServer(BOOL value)override{objects+=value?1:-1;return S_OK;}
};
extern "C" HRESULT __stdcall DllGetClassObject(REFCLSID id,REFIID iid,void** output) {
    if(!output)return E_POINTER;*output=nullptr;
    for(int slot=0;slot<60;slot++)if(id==SourceGuid(slot)){
        auto factory=Make<Factory>(slot);return factory?factory.CopyTo(iid,output):E_OUTOFMEMORY;
    }
    return CLASS_E_CLASSNOTAVAILABLE;
}
extern "C" HRESULT __stdcall DllCanUnloadNow(){return objects.load()==0?S_OK:S_FALSE;}
BOOL WINAPI DllMain(HMODULE instance,DWORD reason,LPVOID){
    if(reason==DLL_PROCESS_ATTACH){module=instance;DisableThreadLibraryCalls(instance);}return TRUE;
}
extern "C" __declspec(dllexport) HRESULT GoProMFInstall() {
    wchar_t filename[32768]={};
    if(!GetModuleFileNameW(module,filename,32768))return HRESULT_FROM_WIN32(GetLastError());
    for(int slot=0;slot<60;slot++){
        wchar_t guid[64];StringFromGUID2(SourceGuid(slot),guid,64);
        std::wstring path=L"SOFTWARE\\Classes\\CLSID\\"+std::wstring(guid)+L"\\InprocServer32";
        HKEY key=nullptr;
        LONG result=RegCreateKeyExW(HKEY_LOCAL_MACHINE,path.c_str(),0,nullptr,0,KEY_SET_VALUE|KEY_WOW64_64KEY,nullptr,&key,nullptr);
        if(result)return HRESULT_FROM_WIN32(result);
        result=RegSetValueExW(key,nullptr,0,REG_SZ,(BYTE*)filename,(DWORD)((wcslen(filename)+1)*sizeof(wchar_t)));
        if(!result)result=RegSetValueExW(key,L"ThreadingModel",0,REG_SZ,(BYTE*)L"Both",10);
        RegCloseKey(key);if(result)return HRESULT_FROM_WIN32(result);
    }
    return S_OK;
}
struct Host {
    ComPtr<IMFVirtualCamera> camera;
    std::unique_ptr<FrameChannel> channel;
    bool com=false,started=false;
    ~Host(){
        if(camera){camera->Stop();camera->Shutdown();camera.Reset();}
        if(started)MFShutdown();
        if(com)CoUninitialize();
    }
};
extern "C" __declspec(dllexport) HRESULT GoProMFCreate(int slot,const wchar_t* name,void** output) {
    if(!output||!name||slot<0||slot>=60)return E_INVALIDARG;
    *output=nullptr;auto host=std::make_unique<Host>();
    HRESULT com=CoInitializeEx(nullptr,COINIT_MULTITHREADED);
    if(FAILED(com)&&com!=RPC_E_CHANGED_MODE)return com;
    host->com=SUCCEEDED(com);
    CHECK(MFStartup(MF_VERSION));host->started=true;
    std::wstring sid=CurrentSid();if(sid.empty())return E_ACCESSDENIED;
    host->channel=std::make_unique<FrameChannel>(sid,slot);
    wchar_t guid[64];StringFromGUID2(SourceGuid(slot),guid,64);
    CHECK(MFCreateVirtualCamera(MFVirtualCameraType_SoftwareCameraSource,MFVirtualCameraLifetime_Session,
        MFVirtualCameraAccess_CurrentUser,name,guid,nullptr,0,&host->camera));
    CHECK(host->camera->SetString(USER_SID,sid.c_str()));
    CHECK(host->camera->Start(nullptr));
    *output=host.release();return S_OK;
}
extern "C" __declspec(dllexport) void GoProMFDestroy(void* pointer){delete (Host*)pointer;}
extern "C" __declspec(dllexport) int GoProMFWrite(void* pointer,const BYTE* pixels,int width,int height) {
    if(!pointer)return -1;return ((Host*)pointer)->channel->write(pixels,width,height)?1:0;
}
// Real external-client verification: enumerate a PnP camera and read through Media Foundation.
// No direct activation of our own source; this exercises Windows Camera Frame Server.
extern "C" __declspec(dllexport) HRESULT GoProMFCheck(const wchar_t* prefix,int requestedFormat,int* mean) {
    if(!prefix||!mean)return E_INVALIDARG;
    CoInitializeEx(nullptr,COINIT_MULTITHREADED);CHECK(MFStartup(MF_VERSION));
    ComPtr<IMFAttributes> attrs;CHECK(MFCreateAttributes(&attrs,2));
    CHECK(attrs->SetGUID(MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID));
    IMFActivate** devices=nullptr;UINT32 count=0;CHECK(MFEnumDeviceSources(attrs.Get(),&devices,&count));
    ComPtr<IMFActivate> chosen;
    for(UINT32 i=0;i<count;i++){
        wchar_t name[512]={};devices[i]->GetString(MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME,name,512,nullptr);
        if(wcsstr(name,prefix))chosen=devices[i];
        devices[i]->Release();
    }
    CoTaskMemFree(devices);if(!chosen)return HRESULT_FROM_WIN32(ERROR_NOT_FOUND);
    ComPtr<IMFMediaSource> source;CHECK(chosen->ActivateObject(IID_PPV_ARGS(&source)));
    ComPtr<IMFSourceReader> reader;
    HRESULT result=MFCreateSourceReaderFromMediaSource(source.Get(),nullptr,&reader);
    if(SUCCEEDED(result)){
        ComPtr<IMFMediaType> type;
        GUID sub=requestedFormat==1?MFVideoFormat_YUY2:requestedFormat==2?MFVideoFormat_RGB32:MFVideoFormat_NV12;
        result=MakeType(1280,720,30,sub,&type);
        if(SUCCEEDED(result))result=reader->SetCurrentMediaType(0,nullptr,type.Get());
        for(int i=0;i<5&&SUCCEEDED(result);i++){
            DWORD flags=0;ComPtr<IMFSample> sample;
            result=reader->ReadSample(0,0,nullptr,&flags,nullptr,&sample);
            if(SUCCEEDED(result)&&!sample){result=E_UNEXPECTED;break;}
            if(SUCCEEDED(result)){
                ComPtr<IMFMediaBuffer> buffer;result=sample->ConvertToContiguousBuffer(&buffer);
                BYTE* pixels=nullptr;DWORD length=0;
                if(SUCCEEDED(result))result=buffer->Lock(&pixels,nullptr,&length);
                if(SUCCEEDED(result)){
                    UINT64 sum=0;for(DWORD k=0;k<length;k++)sum+=pixels[k];
                    *mean=length?(int)(sum/length):0;buffer->Unlock();
                    if(!length)result=E_UNEXPECTED;
                }
            }
        }
    }
    reader.Reset();source->Shutdown();chosen->ShutdownObject();MFShutdown();CoUninitialize();return result;
}

// Runs the exact media source with a session-local transport fixture, without installation.
extern "C" __declspec(dllexport) HRESULT GoProMFLocalCheck(int requestedFormat,int* mean) {
    if(!mean)return E_POINTER;
    CoInitializeEx(nullptr,COINIT_MULTITHREADED);CHECK(MFStartup(MF_VERSION));
    auto source=Make<Source>();CHECK(source->Initialize(CurrentSid(),55,true));
    FrameChannel channel(CurrentSid(),55,true);
    if(!channel.open(true))return HRESULT_FROM_WIN32(GetLastError());
    std::vector<BYTE> pixels(1280*720*4);
    for(size_t k=0;k<pixels.size();k+=4){pixels[k]=45;pixels[k+1]=145;pixels[k+2]=220;pixels[k+3]=255;}
    channel.write(pixels.data(),1280,720);
    ComPtr<IMFSourceReader> reader;
    HRESULT result=MFCreateSourceReaderFromMediaSource(source.Get(),nullptr,&reader);
    if(SUCCEEDED(result)){
        ComPtr<IMFMediaType> type;GUID format=requestedFormat==1?MFVideoFormat_YUY2:requestedFormat==2?MFVideoFormat_RGB32:MFVideoFormat_NV12;
        result=MakeType(1280,720,30,format,&type);
        if(SUCCEEDED(result))result=reader->SetCurrentMediaType(0,nullptr,type.Get());
        for(int i=0;i<5&&SUCCEEDED(result);i++){
            ComPtr<IMFSample> sample;DWORD flags=0;
            result=reader->ReadSample(0,0,nullptr,&flags,nullptr,&sample);
            if(SUCCEEDED(result)&&!sample){result=E_UNEXPECTED;break;}
            if(SUCCEEDED(result)){
                ComPtr<IMFMediaBuffer> buffer;result=sample->ConvertToContiguousBuffer(&buffer);
                BYTE* data=nullptr;DWORD size=0;
                if(SUCCEEDED(result))result=buffer->Lock(&data,nullptr,&size);
                if(SUCCEEDED(result)){
                    UINT64 sum=0;for(DWORD k=0;k<size;k++)sum+=data[k];
                    *mean=size?(int)(sum/size):0;buffer->Unlock();
                }
            }
        }
    }
    reader.Reset();source->Shutdown();MFShutdown();CoUninitialize();return result;
}
