set "TC=D:\staff_01\WS_WorkBuddy\rpa\.toolchain"
set "RUSTUP_HOME=%TC%\rustup"
set "CARGO_HOME=%TC%\cargo"
set "PATH=%CARGO_HOME%\bin;%TC%\rustup\toolchains\stable-x86_64-pc-windows-gnu\bin;%TC%\mingw64\bin;%PATH%"
set "CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=%TC%\mingw64\bin\gcc.exe"
cargo build -p desktop --features custom-protocol --release
