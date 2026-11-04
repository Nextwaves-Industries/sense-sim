# Source this in Git Bash before cargo: puts the GNU Rust toolchain + WinLibs MinGW on PATH.
export RUSTUP_TOOLCHAIN=stable-x86_64-pc-windows-gnu
export PATH="$(ls -d /c/Users/DeanOS/AppData/Local/Microsoft/WinGet/Packages/BrechtSanders.WinLibs.POSIX.MSVCRT*/mingw64/bin):$HOME/.cargo/bin:$PATH"
