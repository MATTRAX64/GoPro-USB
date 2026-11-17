@echo off
setlocal
cd /d "%~dp0"
where cargo >nul 2>&1
if errorlevel 1 (
    echo Rust/Cargo introuvable. Installez Rust avec rustup, puis rouvrez ce script.
    pause
    exit /b 1
)
cargo +stable-x86_64-pc-windows-msvc build --release --locked
if errorlevel 1 (
    echo Echec de compilation. Consultez les messages ci-dessus et README.md.
    pause
    exit /b 1
)
copy /y "target\release\gopro-usb.exe" "GoPro USB.exe" >nul
if errorlevel 1 (
    echo Fermez GoPro USB et son processus de surveillance, puis recommencez.
    pause
    exit /b 1
)
echo Compilation terminee : GoPro USB.exe
pause
