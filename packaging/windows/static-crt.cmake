# Used by the Windows release build (see .github/workflows/release.yml). Makes
# libopus, which libopus_sys builds with CMake, use the static C runtime to
# match -C target-feature=+crt-static, so peek.exe needs no Visual C++ runtime DLLs.
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded" CACHE STRING "")
set(OPUS_STATIC_RUNTIME ON CACHE BOOL "")
