#pragma once
#include <windows.h>
#include <sddl.h>
#include <string>
#include <vector>
#include <cstring>

// One latest image, shared by every Media Foundation client of this camera.
// The service creates the global mapping; an ordinary user only opens it.
struct FrameHeader {
    UINT32 magic, width, height, generation;
    ULONGLONG tick;
    BYTE pixels[3840*2160*4];
};
struct FrameChannel {
    HANDLE mutex=nullptr, mapping=nullptr;
    FrameHeader* frame=nullptr;
    std::wstring stem, sid;
    FrameChannel(const std::wstring& userSid, int slot,bool local=false):sid(userSid) {
        stem=std::wstring(local?L"Local\\GoProUsbMF_":L"Global\\GoProUsbMF_")+sid+L"_"+std::to_wstring(slot);
    }
    ~FrameChannel(){close();}
    void close() {
        if(frame)UnmapViewOfFile(frame);
        if(mapping)CloseHandle(mapping);
        if(mutex)CloseHandle(mutex);
        frame=nullptr;mapping=mutex=nullptr;
    }
    bool open(bool service) {
        if(frame)return true;
        if(service) {
            PSID checked=nullptr;
            if(!ConvertStringSidToSidW(sid.c_str(),&checked))return false;
            LocalFree(checked);
            std::wstring sddl=L"D:P(A;;GA;;;SY)(A;;GA;;;LS)(A;;GA;;;"+sid+L")";
            PSECURITY_DESCRIPTOR descriptor=nullptr;
            if(!ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.c_str(),SDDL_REVISION_1,&descriptor,nullptr))return false;
            SECURITY_ATTRIBUTES attributes={sizeof(attributes),descriptor,FALSE};
            mutex=CreateMutexW(&attributes,FALSE,(stem+L"_lock").c_str());
            mapping=CreateFileMappingW(INVALID_HANDLE_VALUE,&attributes,PAGE_READWRITE,0,sizeof(FrameHeader),(stem+L"_frame").c_str());
            LocalFree(descriptor);
        } else {
            mutex=OpenMutexW(SYNCHRONIZE|MUTEX_MODIFY_STATE,FALSE,(stem+L"_lock").c_str());
            mapping=OpenFileMappingW(FILE_MAP_READ|FILE_MAP_WRITE,FALSE,(stem+L"_frame").c_str());
        }
        if(mapping&&mutex)frame=(FrameHeader*)MapViewOfFile(mapping,FILE_MAP_READ|FILE_MAP_WRITE,0,0,sizeof(FrameHeader));
        if(!frame){close();return false;}
        return true;
    }
    bool write(const BYTE* rgba,UINT32 width,UINT32 height) {
        if(!rgba||!width||!height||width>3840||height>2160||!open(false))return false;
        DWORD wait=WaitForSingleObject(mutex,2);
        if(wait!=WAIT_OBJECT_0&&wait!=WAIT_ABANDONED)return false;
        frame->width=width;frame->height=height;
        memcpy(frame->pixels,rgba,(size_t)width*height*4);
        frame->tick=GetTickCount64();frame->generation++;frame->magic=0x47504d46;
        ReleaseMutex(mutex);return true;
    }
    bool read(std::vector<BYTE>& rgba,UINT32& width,UINT32& height) {
        if(!open(true))return false;
        DWORD wait=WaitForSingleObject(mutex,10);
        if(wait!=WAIT_OBJECT_0&&wait!=WAIT_ABANDONED)return false;
        bool valid=frame->magic==0x47504d46&&frame->width&&frame->width<=3840&&
            frame->height&&frame->height<=2160&&GetTickCount64()-frame->tick<2000;
        if(valid) {
            width=frame->width;height=frame->height;
            rgba.resize((size_t)width*height*4);
            memcpy(rgba.data(),frame->pixels,rgba.size());
        }
        ReleaseMutex(mutex);return valid;
    }
};
inline std::wstring CurrentSid() {
    HANDLE token=nullptr;
    if(!OpenProcessToken(GetCurrentProcess(),TOKEN_QUERY,&token))return {};
    DWORD size=0;GetTokenInformation(token,TokenUser,nullptr,0,&size);
    std::vector<BYTE> data(size);
    bool ok=GetTokenInformation(token,TokenUser,data.data(),size,&size)!=FALSE;
    CloseHandle(token);
    LPWSTR sid=nullptr;
    if(!ok||!ConvertSidToStringSidW(((TOKEN_USER*)data.data())->User.Sid,&sid))return {};
    std::wstring result=sid;LocalFree(sid);return result;
}
