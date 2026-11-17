use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=assets/camera.ico");
    winresource::WindowsResource::new()
        .set_icon("assets/camera.ico")
        .compile()
        .expect("Could not embed application icon");

    for path in ["native", "third_party/UnityCapture/Source"] {
        println!("cargo:rerun-if-changed={path}");
    }
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    for architecture in ["x86_64-pc-windows-msvc", "i686-pc-windows-msvc"] {
        let output = output.join(if architecture.starts_with("i686") {
            "x86"
        } else {
            "x64"
        });
        std::fs::create_dir_all(&output).unwrap();
        let tool = cc::Build::new()
            .target(architecture)
            .cpp(true)
            .static_crt(true)
            .get_compiler();
        let mut command = tool.to_command();
        command.current_dir(&output).args([
            "/nologo",
            "/LD",
            "/MT",
            "/O2",
            "/EHsc",
            "/DWIN32",
            "/DNDEBUG",
            "/D_WINDOWS",
            "/D_USRDLL",
            "/D_WIN32_WINNT=0x0601",
        ]);
        command.arg(format!("/I{}", root.join("native").display()));
        command.arg(format!(
            "/I{}",
            root.join("third_party/UnityCapture/Source").display()
        ));
        command.arg(root.join("native/GoProCapture.cpp"));
        command.arg(root.join("third_party/UnityCapture/Source/streams.cpp"));
        command.args(["/link", "/NOLOGO"]);
        command.arg(format!(
            "/DEF:{}",
            root.join("native/GoProCapture.def").display()
        ));
        command.arg(format!(
            "/OUT:{}",
            output.join("GoProCapture.dll").display()
        ));
        command.args([
            "strmiids.lib",
            "ole32.lib",
            "oleaut32.lib",
            "uuid.lib",
            "advapi32.lib",
            "user32.lib",
            "gdi32.lib",
            "winmm.lib",
        ]);
        if architecture.starts_with("i686") {
            command.arg("/MACHINE:X86");
        }
        let result = command.output().expect("Could not run native compiler");
        if !result.status.success() {
            panic!(
                "Native camera build failed\n{}\n{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
        }
    }
    let modern_output=output.join("modern");
    std::fs::create_dir_all(&modern_output).unwrap();
    let compiler=cc::Build::new().target("x86_64-pc-windows-msvc").cpp(true).static_crt(true).get_compiler();
    let mut command=compiler.to_command();
    command.current_dir(&modern_output).args(["/nologo","/LD","/MT","/O2","/EHsc","/std:c++17",
        "/D_WIN32_WINNT=0x0A00","/DWINVER=0x0A00","/DNTDDI_VERSION=0x0A00000B"]);
    command.arg(root.join("native/modern/GoProMF.cpp"));
    command.args(["/link","/NOLOGO"]);
    command.arg(format!("/DEF:{}",root.join("native/modern/GoProMF.def").display()));
    command.arg(format!("/OUT:{}",modern_output.join("GoProMF.dll").display()));
    command.args(["mfplat.lib","mf.lib","mfuuid.lib","mfreadwrite.lib","mfsensorgroup.lib",
        "ole32.lib","oleaut32.lib","advapi32.lib","uuid.lib","propsys.lib","ksuser.lib"]);
    let result=command.output().expect("Could not compile Media Foundation camera");
    if !result.status.success(){panic!("Media Foundation build failed\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),String::from_utf8_lossy(&result.stderr));}

}
