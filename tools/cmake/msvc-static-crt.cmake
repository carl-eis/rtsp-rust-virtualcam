# Passed to CMake builds of C libraries (libopus, through songbird) by .cargo/config.toml.
# The Rust side links the C runtime statically (+crt-static); without this, CMake 3.15+ builds
# the library against the DLL runtime (/MD) and the two runtimes clash at link time (LNK4098).
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded")
# libopus picks its runtime with its own option, overriding the variable above.
set(OPUS_STATIC_RUNTIME ON CACHE BOOL "Build libopus with the static C runtime" FORCE)
