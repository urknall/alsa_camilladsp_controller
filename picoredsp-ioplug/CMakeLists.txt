cmake_minimum_required(VERSION 3.16)
project(picoredsp-ioplug C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)   # for GNU extensions used by ALSA headers

# ---------------------------------------------------------------------------
# Compiler warnings / hardening
# ---------------------------------------------------------------------------
add_compile_options(
    -Wall -Wextra -Wpedantic -Werror
    -Wformat=2 -Wformat-security
    -Wno-unused-parameter        # ALSA callbacks often ignore parameters
    $<$<C_COMPILER_ID:Clang>:-Wno-gnu-zero-variadic-macro-arguments>
)

# Position-independent code (shared library)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

# ---------------------------------------------------------------------------
# Sanitiser builds
# ---------------------------------------------------------------------------
option(ASAN  "Build with AddressSanitiser"   OFF)
option(UBSAN "Build with UndefinedBehaviourSanitiser" OFF)
option(TSAN  "Build with ThreadSanitiser"    OFF)

if(ASAN)
    add_compile_options(-fsanitize=address -fno-omit-frame-pointer)
    add_link_options   (-fsanitize=address)
endif()
if(UBSAN)
    add_compile_options(-fsanitize=undefined)
    add_link_options   (-fsanitize=undefined)
endif()
if(TSAN)
    add_compile_options(-fsanitize=thread)
    add_link_options   (-fsanitize=thread)
endif()

# ---------------------------------------------------------------------------
# Find ALSA
# ---------------------------------------------------------------------------
find_package(PkgConfig REQUIRED)
pkg_check_modules(ALSA REQUIRED alsa)

# ---------------------------------------------------------------------------
# Plugin shared library
# ---------------------------------------------------------------------------
add_library(asound_module_pcm_picoredsp MODULE
    src/pcm.c
    src/ringbuffer.c
    src/ipc.c
    src/format.c
    src/timing.c
    src/pcm_worker.c
)

# pcm.c uses ALSA plugin macros (SND_PCM_PLUGIN_SYMBOL) that expand to
# code patterns not accepted by -Wpedantic; suppress it only for that file.
set_source_files_properties(src/pcm.c PROPERTIES COMPILE_OPTIONS "-Wno-pedantic")

target_include_directories(asound_module_pcm_picoredsp
    PRIVATE
        ${ALSA_INCLUDE_DIRS}
        ${CMAKE_CURRENT_SOURCE_DIR}/src
)

# ALSA's SND_DLSYM_BUILD_VERSION macro uses `#ifdef PIC` (not `__PIC__`) to
# switch between the shared-library and static-library plugin ABI.  GCC defines
# `__PIC__` for -fPIC but not `PIC`, so we add it explicitly here.
target_compile_definitions(asound_module_pcm_picoredsp PRIVATE PIC)

target_link_libraries(asound_module_pcm_picoredsp
    PRIVATE
        ${ALSA_LIBRARIES}
        pthread
)

# ALSA discovers external PCM modules as `libasound_module_pcm_<type>.so`.
# Keep the CMake target name stable for build scripts, but emit the real
# runtime filename directly instead of relying on a build-tree symlink.
set_target_properties(asound_module_pcm_picoredsp PROPERTIES
    OUTPUT_NAME "libasound_module_pcm_picoredsp"
    PREFIX ""
)

# Default install path matches alsa-lib convention
include(GNUInstallDirs)
install(TARGETS asound_module_pcm_picoredsp
    LIBRARY DESTINATION ${CMAKE_INSTALL_LIBDIR}/alsa-lib
)

# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------
enable_testing()

# Static library of plugin internals (without the ALSA entry point) for
# linking into unit tests without requiring a full ALSA plugin load.
add_library(pcdsp_internals STATIC
    src/ringbuffer.c
    src/ipc.c
    src/format.c
    src/timing.c
    src/pcm_worker.c
)

target_include_directories(pcdsp_internals
    PUBLIC
        ${ALSA_INCLUDE_DIRS}
        ${CMAKE_CURRENT_SOURCE_DIR}/src
)

target_link_libraries(pcdsp_internals
    PUBLIC
        ${ALSA_LIBRARIES}
        pthread
)

# Helper to register a test executable
function(pcdsp_add_test target source)
    add_executable(${target} ${source})
    target_link_libraries(${target} PRIVATE pcdsp_internals)
    add_test(NAME ${target} COMMAND ${target})
    set_tests_properties(${target} PROPERTIES TIMEOUT 30)
endfunction()

pcdsp_add_test(test_ringbuffer tests/test_ringbuffer.c)
pcdsp_add_test(test_format     tests/test_format.c)
pcdsp_add_test(test_timing     tests/test_timing.c)

# test_ipc links against the ipc object directly; it also needs pthread for
# the mock-server thread used by the connection tests.
add_executable(test_ipc tests/test_ipc.c src/ipc.c)
target_include_directories(test_ipc PRIVATE ${ALSA_INCLUDE_DIRS} ${CMAKE_CURRENT_SOURCE_DIR}/src)
target_link_libraries(test_ipc PRIVATE ${ALSA_LIBRARIES} pthread)
# test_ipc creates AF_UNIX sockets with unique paths in /tmp so tests are
# fully isolated and can run in parallel.
add_test(NAME test_ipc COMMAND test_ipc)
set_tests_properties(test_ipc PROPERTIES TIMEOUT 60)

# test_pcm_worker — unit tests for the pipe-drain / null-sink helpers extracted
# from the worker thread (src/pcm_worker.c).
pcdsp_add_test(test_pcm_worker tests/test_pcm_worker.c)

# test_audio_integrity — M11 bit-transparency tests for all sample formats
# and rates.  Tests the pipe-drain helper directly (no ALSA needed).
pcdsp_add_test(test_audio_integrity tests/test_audio_integrity.c)

# test_pcm_integration — ALSA ioplug plugin integration tests.
# The plugin shared library is loaded by the ALSA API; we tell ALSA where to
# find it via the ALSA_PLUGIN_DIR environment variable.
add_executable(test_pcm_integration
    tests/test_pcm_integration.c
    src/ipc.c
)
target_include_directories(test_pcm_integration
    PRIVATE ${ALSA_INCLUDE_DIRS} ${CMAKE_CURRENT_SOURCE_DIR}/src)
target_link_libraries(test_pcm_integration
    PRIVATE ${ALSA_LIBRARIES} pthread)
add_test(NAME test_pcm_integration COMMAND test_pcm_integration)
# Set ALSA_PLUGIN_DIR so ALSA can find our freshly-built plugin.
set_tests_properties(test_pcm_integration PROPERTIES
    TIMEOUT 60
    ENVIRONMENT "ALSA_PLUGIN_DIR=${CMAKE_CURRENT_BINARY_DIR}"
)

# ---------------------------------------------------------------------------
# Optional: microbenchmarks
# ---------------------------------------------------------------------------
# Build with -DBENCH=ON to compile and run the bench/ programs.
# Benchmarks are NOT registered as CTest tests (they are not pass/fail) —
# run them explicitly after building:
#
#   cmake -DBENCH=ON ..
#   make bench_ringbuffer bench_timing bench_pcm_worker
#   ./bench_ringbuffer && ./bench_timing && ./bench_pcm_worker
#
# The convenience target `run_benches` builds and immediately runs all three.
# ---------------------------------------------------------------------------
option(BENCH "Build microbenchmarks" OFF)

if(BENCH)
    # Helper: build one benchmark executable.
    # All benchmarks link pcdsp_internals (ring buffer, timing, pcm_worker) and
    # require pthread + libm (for sqrt in bench_harness.h).
    function(pcdsp_add_bench target source)
        add_executable(${target} ${source})
        target_include_directories(${target}
            PRIVATE
                ${CMAKE_CURRENT_SOURCE_DIR}/bench
                ${CMAKE_CURRENT_SOURCE_DIR}/src
                ${ALSA_INCLUDE_DIRS}
        )
        target_link_libraries(${target} PRIVATE pcdsp_internals pthread m)
        # Benchmarks are O2-optimised regardless of the overall build type so
        # results are representative of production code.
        target_compile_options(${target} PRIVATE -O2)
    endfunction()

    pcdsp_add_bench(bench_ringbuffer bench/bench_ringbuffer.c)
    pcdsp_add_bench(bench_timing     bench/bench_timing.c)
    pcdsp_add_bench(bench_pcm_worker bench/bench_pcm_worker.c)

    # Convenience target: build all benchmarks and run them in sequence.
    add_custom_target(run_benches
        COMMAND bench_ringbuffer
        COMMAND bench_timing
        COMMAND bench_pcm_worker
        DEPENDS bench_ringbuffer bench_timing bench_pcm_worker
        WORKING_DIRECTORY ${CMAKE_CURRENT_BINARY_DIR}
        COMMENT "Running all picoredsp-ioplug microbenchmarks"
    )
endif()

# ---------------------------------------------------------------------------
# Optional: clang-tidy static analysis
# ---------------------------------------------------------------------------
option(CLANG_TIDY "Run clang-tidy static analysis during build" OFF)

if(CLANG_TIDY)
    find_program(CLANG_TIDY_EXE NAMES clang-tidy)
    if(CLANG_TIDY_EXE)
        set_target_properties(asound_module_pcm_picoredsp PROPERTIES
            C_CLANG_TIDY "${CLANG_TIDY_EXE};--warnings-as-errors=*;--checks=-clang-analyzer-security.insecureAPI.DeprecatedOrUnsafeBufferHandling"
        )
    else()
        message(WARNING "CLANG_TIDY requested but clang-tidy not found")
    endif()
endif()
