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
)

# pcm.c uses ALSA plugin macros (SND_PCM_PLUGIN_SYMBOL) that expand to
# code patterns not accepted by -Wpedantic; suppress it only for that file.
set_source_files_properties(src/pcm.c PROPERTIES COMPILE_OPTIONS "-Wno-pedantic")

target_include_directories(asound_module_pcm_picoredsp
    PRIVATE
        ${ALSA_INCLUDE_DIRS}
        ${CMAKE_CURRENT_SOURCE_DIR}/src
)

target_link_libraries(asound_module_pcm_picoredsp
    PRIVATE
        ${ALSA_LIBRARIES}
        pthread
)

# ALSA modules must not have a 'lib' prefix
set_target_properties(asound_module_pcm_picoredsp PROPERTIES
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

# ---------------------------------------------------------------------------
# Optional: clang-tidy static analysis
# ---------------------------------------------------------------------------
option(CLANG_TIDY "Run clang-tidy static analysis during build" OFF)

if(CLANG_TIDY)
    find_program(CLANG_TIDY_EXE NAMES clang-tidy)
    if(CLANG_TIDY_EXE)
        set_target_properties(asound_module_pcm_picoredsp PROPERTIES
            C_CLANG_TIDY "${CLANG_TIDY_EXE};--warnings-as-errors=*"
        )
    else()
        message(WARNING "CLANG_TIDY requested but clang-tidy not found")
    endif()
endif()
