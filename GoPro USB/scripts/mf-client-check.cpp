
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <mfapi.h>
#include <mfidl.h>
#include <mfreadwrite.h>
#include <wrl.h>
#include <cstdio>
using namespace Microsoft::WRL;
#define RUN(x) {HRESULT r=(x);printf(#x " -> 0x%08lx\n",r);fflush(stdout);if(FAILED(r))return 1;}
int wmain(int argc,wchar_t** argv){
CoInitializeEx(nullptr,COINIT_MULTITHREADED);RUN(MFStartup(MF_VERSION));
ComPtr<IMFAttributes> a;RUN(MFCreateAttributes(&a,2));
RUN(a->SetGUID(MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID));
IMFActivate** list=nullptr;UINT32 n=0;RUN(MFEnumDeviceSources(a.Get(),&list,&n));
ComPtr<IMFActivate> chosen;
for(UINT32 i=0;i<n;i++){wchar_t name[512]={};list[i]->GetString(MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME,name,512,nullptr);
wprintf(L"%s\n",name);if(wcsstr(name,argc>1?argv[1]:L"GoPro USB 1"))chosen=list[i];list[i]->Release();}
CoTaskMemFree(list);if(!chosen)return 2;
ComPtr<IMFMediaSource> source;RUN(chosen->ActivateObject(IID_PPV_ARGS(&source)));
ComPtr<IMFSourceReader> reader;RUN(MFCreateSourceReaderFromMediaSource(source.Get(),nullptr,&reader));
for(int i=0;i<5;i++){ComPtr<IMFSample> sample;DWORD flags=0;RUN(reader->ReadSample(0,0,nullptr,&flags,nullptr,&sample));
printf("sample=%p flags=%lu\n",sample.Get(),flags);fflush(stdout);if(!sample)return 3;
ComPtr<IMFMediaBuffer> b;RUN(sample->ConvertToContiguousBuffer(&b));BYTE* data;DWORD size;RUN(b->Lock(&data,nullptr,&size));
unsigned long long sum=0;for(DWORD k=0;k<size;k++)sum+=data[k];b->Unlock();printf("bytes=%lu mean=%llu\n",size,sum/size);}
reader.Reset();source->Shutdown();MFShutdown();CoUninitialize();return 0;}
