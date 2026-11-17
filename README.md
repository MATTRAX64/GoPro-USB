<div align="center">
  <img src="assets/camera.png" alt="GoPro USB camera logo" width="112">
  <h1>GoPro USB</h1>
  <p>A Windows desktop application for GoPro USB capture, live image adjustments, and virtual camera output.</p>
  <p><strong>Rust backend · Embedded web interface · Windows 11 x64</strong></p>
</div>

---

GoPro USB turns a compatible GoPro's USB webcam stream into an adjustable video source for recording and streaming applications. Preview your camera, change image settings while it runs, choose an output resolution and frame rate, and publish the result through a Windows virtual camera.

**Development status:** DirectShow is the primary virtual camera output. Media Foundation support is experimental and is not yet reliable across applications.

## Features

### Camera capture

- Automatically discover compatible GoPro cameras connected over USB.
- Confirm source modes by decoding an actual image.
- Remember confirmed resolutions, frame rates, and fields of view between launches.
- Keep separate capability records for each camera's serial number, model, and firmware.
- Choose the camera and supported field of view from the interface.
- Force a fresh capability scan when needed.

### Video output

| Setting | Available options |
| --- | --- |
| Resolution | 360p, 480p, 720p, 1080p HD, 1440p / 2K, 2160p / 4K |
| Frame rate | 15, 24, 30, 50, 60, 75, 100, 120 FPS |
| Image adjustments | Brightness, contrast, saturation, gamma, sharpness |
| Framing | Digital zoom, horizontal mirror, vertical mirror |
| Decoding | Automatic, CPU, or a selected D3D11 GPU |
| Virtual camera | DirectShow components for 64-bit and 32-bit clients |

Image adjustments are applied live without restarting the camera stream. Capture and virtual camera delivery use separate workers. Each output keeps only the latest pending image, so a slow client does not create a growing queue of old frames.

The preview and export handle their required image row order separately, without requiring the user to enable a mirror control to compensate.

### Desktop experience

- Embedded HTML, CSS, and JavaScript interface with a Rust backend.
- French, English, Spanish, Japanese, Russian, and German.
- CPU and GPU usage indicators.
- Settings saved automatically on the current PC.
- System tray operation and background launch mode.
- A separate watchdog that attempts to stop capture after the main process exits unexpectedly.

## Native, resized, and generated video

**Output settings do not unlock additional camera hardware modes.**

The USB webcam stream and the camera's internal recording modes are different. The tested HERO13 supplies approximately 30 FPS over USB at 720p or 1080p. Higher output settings are processed on the PC.

| Output type | What it means |
| --- | --- |
| Native | The selected source mode was confirmed from the camera stream. |
| Resized | An existing source image is scaled to a different output size. |
| Generated resolution | The output is enlarged beyond the confirmed source resolution; it does not add native sensor detail. |
| Generated frame rate | Additional frames are produced using frame blending above the source cadence. |
| Lower frame rate | Fewer source frames are selected for delivery. |

GPU selection currently accelerates **video decoding**. Image filters, scaling, and generated frames still use the **CPU**. Actual throughput and latency depend on the output settings, PC load, and receiving application.

## Getting started

### Runtime requirements

- **Windows 11 x64**.
- **Microsoft Edge WebView2 Runtime**.
- **FFmpeg** available in `PATH`, including the `zmq`, `framerate`, `eq`, `crop`, and `cas` filters. Development testing used a Gyan FFmpeg full build.
- Windows-provided `curl.exe` and PowerShell.
- A GoPro that supports the USB webcam interface used by this application.

FFmpeg and WebView2 are external dependencies; they are not bundled in this repository.

### Launch the application

1. Connect and power on a compatible GoPro using a USB data cable.
2. Open **GoPro USB.exe** from the repository root.
3. Allow the initial capability scan to finish.
4. Choose the output resolution, frame rate, field of view, and image adjustments.
5. In your recording or streaming application, select the published **GoPro USB** camera.

Confirmed modes are cached locally. Subsequent launches reuse that cache when the camera identity and firmware match. Use **USB → Rescan camera modes** to force another scan; the label follows the selected interface language.

The USB connection exposes a local network interface. The application uses that link to control the camera and receive video. Capture does not require an Internet connection or an account.

### Background operation

Closing the window minimizes the application to the system tray. Use the tray menu's **Quit** command to stop capture and exit.

To launch minimized:

```powershell
& ".\GoPro USB.exe" --background
```

## Virtual camera compatibility

### DirectShow

The application embeds and installs its DirectShow components for the current Windows user. A camera is registered after an image has been decoded, and its name includes the camera model when available.

The implementation supports separate virtual camera identities for multiple GoPro devices. Two independent virtual sources have been tested with DirectShow clients; simultaneous capture from multiple physical GoPro cameras still needs hardware validation.

### Media Foundation — experimental

An additional Windows Camera Frame Server component is included for applications that use Media Foundation instead of DirectShow.

Its installation requires a Windows administrator confirmation because the service loads a machine-registered component. Use **Install Windows camera** in the application when needed. Windows adds its own virtual-camera suffix to this device's display name.

**Known issue:** Windows can enumerate this camera, but SourceReader streaming has failed to start during testing. Installation or appearance in a device list does not establish working capture. TikTok LIVE Studio and universal application compatibility are not validated.

The Media Foundation output runs separately from capture and DirectShow delivery.

## Build from source

### Prerequisites

1. Install Rust using rustup, with the stable **x86_64-pc-windows-msvc** toolchain.
2. Install Visual Studio or Visual Studio Build Tools with:
   - **Desktop development with C++**
   - MSVC tools and libraries for **x64 and x86**
   - **Windows 11 SDK 10.0.26100** or later

All native source files required by the build are included. No Git submodule checkout is needed.

### Build

Double-click **build.cmd**, or run:

```powershell
cargo +stable-x86_64-pc-windows-msvc build --release --locked
```

The executable is produced at:

```text
target/release/gopro-usb.exe
```

The build script also copies it to **GoPro USB.exe** in the repository root. Exit the application before replacing an existing executable. The first build requires Internet access to download Rust dependencies.

`build.rs` compiles the DirectShow components for x64 and x86, compiles the Media Foundation component, and embeds the native binaries and application icon into the Rust executable. Interface resources are embedded as well.

## Testing

Run the Rust tests:

```powershell
cargo +stable-x86_64-pc-windows-msvc test --release --locked
```

Additional checks require Python x64 and FFmpeg:

```powershell
python scripts/verify-resize.py
python scripts/verify-multiple-cameras.py
python scripts/verify-modern-camera.py --local
```

These checks cover native resizing, independent DirectShow sources, and the Media Foundation source implementation. Virtual camera tests temporarily register test devices and remove them afterward. The local Media Foundation test does not validate Camera Frame Server integration.

The `measure_camera_modes` hardware test is ignored by default. It requires a connected camera and the desktop application to be stopped.

## Project structure

```text
.
├── assets/          Camera logo and application icons
├── native/          DirectShow and Media Foundation components
├── scripts/         Validation and diagnostic tools
├── src/             Rust application and capture pipeline
├── third_party/     Required upstream sources and notices
├── ui/              Embedded HTML, CSS, and JavaScript
├── build.cmd        Windows build helper
├── build.rs         Native component build and resource integration
├── Cargo.toml       Rust package manifest
├── Cargo.lock       Locked Rust dependencies
└── GoPro USB.exe    Ready-to-launch application
```

## Local data

Preferences, capability caches, camera identities, and extracted runtime components are stored outside the repository:

```text
%LOCALAPPDATA%\GoPro USB
```

The optional Media Foundation service component is installed under:

```text
%ProgramFiles%\GoPro USB
```

Capability caching stores mode metadata, not captured photos or video. Application settings and capability cache files are excluded from the repository.

## Current limitations

- Hardware testing has focused on the **HERO13 Black**. Other GoPro models are not yet validated.
- High output resolutions and frame rates may be generated rather than supplied natively by the camera.
- The requested frame rate is not a guarantee of the achieved frame rate or zero latency.
- GPU processing does not currently cover the entire pipeline.
- A receiving application can impose its own format, access, and virtual camera restrictions.
- Watchdog cleanup cannot be guaranteed if both the main process and its watchdog are forcibly terminated. Startup recovery remains available.

## Third-party acknowledgments

The DirectShow implementation adapts [UnityCapture](https://github.com/schellingb/UnityCapture), licensed under MIT and based on UnityCam. Upstream copyright and license notices are retained in the source files and in [third_party/UnityCapture/NOTICE.txt](third_party/UnityCapture/NOTICE.txt). The included DirectShow base classes retain their Microsoft notices.

Rust dependencies retain their respective licenses. FFmpeg and Microsoft WebView2 are external components.

---

**GoPro USB is an independent project and is not an official GoPro application.**

