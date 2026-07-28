cmake_minimum_required(VERSION 3.13)

# This entry point builds one of two declared targets, and never a mixture.
#
#   -DDOOMIT_TARGET=browser   WebAssembly, WebSocket transport, host page
#   -DDOOMIT_TARGET=native    host executable, SDL_net UDP transport
#
# Left unset it infers browser from an Emscripten toolchain and native
# otherwise, which is what emcmake and a plain cmake respectively want. Stating
# it explicitly is still better, because an inferred mode that disagrees with
# the toolchain is exactly the partial-native configure this contract exists to
# prevent: the mode and the compiler are cross-checked below, and a mismatch is
# an error rather than a silent adjustment.
#
# The two targets differ in more than a transport. The browser build carries
# Emscripten APIs, a WebAudio music module, and a main loop inverted so the
# browser event loop drives it; the native build carries none of those and runs
# its own loop. Those differences live in the source list below and in
# src/i_host.h, not scattered through the tree.

set(DOOMIT_TARGET "" CACHE STRING "browser or native")
set_property(CACHE DOOMIT_TARGET PROPERTY STRINGS "" browser native)

if(DOOMIT_TARGET STREQUAL "")
    if(CMAKE_SYSTEM_NAME STREQUAL "Emscripten"
       OR CMAKE_TOOLCHAIN_FILE MATCHES "Emscripten\\.cmake$")
        set(DOOMIT_TARGET browser)
    else()
        set(DOOMIT_TARGET native)
    endif()
    message(STATUS "doomit-engine: DOOMIT_TARGET not set, inferring ${DOOMIT_TARGET}")
endif()

if(NOT DOOMIT_TARGET STREQUAL "browser" AND NOT DOOMIT_TARGET STREQUAL "native")
    message(FATAL_ERROR
        "doomit-engine: DOOMIT_TARGET must be browser or native, got "
        "'${DOOMIT_TARGET}'.")
endif()

project(doomit-engine C)

# The pre-project check reads the toolchain the caller asked for, which is
# enough to turn away an ordinary native configure quickly but is not a
# verification of anything. Both signals it reads are caller-controlled
# strings: -DCMAKE_SYSTEM_NAME=Emscripten sets them with any compiler at all,
# and a file merely named Emscripten.cmake can select gcc. CMake's own
# EMSCRIPTEN variable is no better, because the platform module stamps it from
# the system name without consulting the compiler.
#
# So ask the compiler what it is. __EMSCRIPTEN__ is defined by emcc and by
# nothing else, and a spoofed configuration fails here with this project's own
# diagnostic rather than later on a raw Emscripten-only flag.
#
# This runs before the engine target exists, so a rejected toolchain never
# reaches the source list or the flags.

include(CheckCSourceCompiles)

# check_c_source_compiles skips the compile entirely when its result variable
# already exists, which means the caller can simply supply the answer:
# -DDOOMIT_COMPILER_IS_EMSCRIPTEN=TRUE on the command line, or a stale TRUE
# left in the cache of a build directory whose toolchain has since changed.
# Clearing both forms first makes the probe test the compiler that is actually
# configured, on every configure, rather than whatever the cache remembers.

unset(DOOMIT_COMPILER_IS_EMSCRIPTEN)
unset(DOOMIT_COMPILER_IS_EMSCRIPTEN CACHE)

check_c_source_compiles(
    "#ifndef __EMSCRIPTEN__
     #error the configured compiler is not emcc
     #endif
     int main(void) { return 0; }"
    DOOMIT_COMPILER_IS_EMSCRIPTEN)

if(DOOMIT_TARGET STREQUAL "browser" AND NOT DOOMIT_COMPILER_IS_EMSCRIPTEN)
    message(FATAL_ERROR
        "doomit-engine: DOOMIT_TARGET=browser needs an Emscripten compiler.\n"
        "\n"
        "    CMAKE_C_COMPILER = ${CMAKE_C_COMPILER}\n"
        "\n"
        "It does not define __EMSCRIPTEN__. Setting CMAKE_SYSTEM_NAME to "
        "Emscripten, or pointing CMAKE_TOOLCHAIN_FILE at a file named like "
        "Emscripten.cmake, does not make a native compiler into emcc. "
        "Configure through emsdk instead:\n"
        "    source ~/dev/emsdk/emsdk_env.sh\n"
        "    emcmake cmake -S engine -B <build-dir>")
endif()

if(DOOMIT_TARGET STREQUAL "native" AND DOOMIT_COMPILER_IS_EMSCRIPTEN)
    message(FATAL_ERROR
        "doomit-engine: DOOMIT_TARGET=native cannot be built with emcc.\n"
        "\n"
        "    CMAKE_C_COMPILER = ${CMAKE_C_COMPILER}\n"
        "\n"
        "The native target links SDL2_net and runs its own main loop, neither "
        "of which Emscripten provides. Configure with a host compiler, or ask "
        "for -DDOOMIT_TARGET=browser.")
endif()

# This mirrors the single-emcc recipe in docs/provenance.md, which is the
# reference: the pinned artifact hashes come from that recipe.
#
# It does not promise byte-identical output. CMake compiles to separate
# objects and links them, while the recipe hands every source to one emcc
# invocation, and the two order internal wasm functions differently. The
# flags and the source set match; the wasm bytes do not have to.

set(ENGINE_ROOT ${CMAKE_CURRENT_LIST_DIR})

# CMake's own Release defaults would add -O3 -DNDEBUG on top of the -Oz the
# reference recipe uses, and NDEBUG would additionally strip assertions the
# reference keeps. Cleared so the compilation semantics match.

set(CMAKE_C_FLAGS_RELEASE "" CACHE STRING "" FORCE)
set(CMAKE_C_FLAGS_MINSIZEREL "" CACHE STRING "" FORCE)
set(CMAKE_C_FLAGS_RELWITHDEBINFO "" CACHE STRING "" FORCE)
set(CMAKE_C_FLAGS_DEBUG "" CACHE STRING "" FORCE)


include_directories(
    ${ENGINE_ROOT}/gifenc
    ${ENGINE_ROOT}/opl
    ${ENGINE_ROOT}/sdl_mixer
    ${ENGINE_ROOT}/src
    ${ENGINE_ROOT}/src/doom
    )

# Globbed per source root rather than recursively from the project root. A
# recursive glob also swallows test/, whose translation units carry their own
# main(), and would link them into the engine.

file(GLOB_RECURSE ENGINE_SOURCES
    ${ENGINE_ROOT}/gifenc/*.c
    ${ENGINE_ROOT}/opl/*.c
    ${ENGINE_ROOT}/sdl_mixer/*.c
    ${ENGINE_ROOT}/src/*.c
    )

# Sorted for the same reason the recipe pipes find through LC_ALL=C sort: emcc
# assigns wasm function indices and EM_ASM string addresses in command-line
# order, so an unsorted list makes the artifact depend on readdir order.
list(SORT ENGINE_SOURCES)

# The partition between the two targets, in one place.
#
# The transport is selected twice and the two must agree: net_transport.h picks
# the net_module_t symbol by preprocessor, and this picks the translation unit.
# Compiling the wrong one fails at the include (SDL_net.h) or at the link
# (undefined net_sdl_module), which is a deliberate property: the mismatch
# cannot produce a working binary that talks to nothing.
#
# i_host_web.c and i_host_null.c are the two implementations of the host
# notification seam declared in src/i_host.h. Exactly one is compiled, so a
# notification added to only one of them fails to link rather than silently
# doing nothing on the other target.

set(BROWSER_ONLY_SOURCES
    ${ENGINE_ROOT}/src/net_websockets.c
    ${ENGINE_ROOT}/src/net_ws_frame.c
    ${ENGINE_ROOT}/src/i_webmusic.c
    ${ENGINE_ROOT}/src/i_host_web.c
    )

set(NATIVE_ONLY_SOURCES
    ${ENGINE_ROOT}/src/net_sdl.c
    ${ENGINE_ROOT}/src/i_host_null.c
    )

if(DOOMIT_TARGET STREQUAL "browser")
    list(REMOVE_ITEM ENGINE_SOURCES ${NATIVE_ONLY_SOURCES})
else()
    list(REMOVE_ITEM ENGINE_SOURCES ${BROWSER_ONLY_SOURCES})
endif()

add_executable(doom ${ENGINE_SOURCES})

# Both build routes pin GNU C99 explicitly, and the recipe in
# docs/provenance.md passes the same -std=gnu99. The reason is parity and
# reproducibility, not a broken default: emcc 6.0.3 defaults to GNU C17
# (__STDC_VERSION__ 201710L) and compiles this tree fine. But GNU C17 and GNU
# C99 do not produce the same artifact, they differ by 83 wasm bytes, so an
# unpinned route makes the pinned hashes depend on whatever the toolchain
# default happens to be. Leaving one route defaulted and the other pinned is
# exactly what once made a language difference look like a linker artifact.
#
# The extensions are not incidental. sdl_mixer/music.c calls strtok_r, which
# strict -std=c99 hides behind __STRICT_ANSI__, so C_EXTENSIONS OFF does not
# build. It is set explicitly rather than left to CMake's default so the
# emitted -std cannot change underneath the comparison.

set_target_properties(doom PROPERTIES
    C_STANDARD 99
    C_STANDARD_REQUIRED YES
    C_EXTENSIONS ON)

# -Wno-#warnings must be quoted: CMake would otherwise treat the # as the
# start of a comment and pass the invalid flag -Wno- to the compiler.
#
# -ffile-prefix-map keeps absolute build paths out of the artifact. CMake
# compiles with absolute source paths and this tree uses __FILE__, so without
# it the wasm embeds wherever the build happened to run.

target_compile_options(doom PRIVATE
    -Oz
    "-Wno-#warnings"
    -Wno-macro-redefined
    -Wno-switch
    -ffile-prefix-map=${ENGINE_ROOT}/=
    )

if(DOOMIT_TARGET STREQUAL "browser")

set_target_properties(doom PROPERTIES SUFFIX ".js")

target_compile_options(doom PRIVATE -sUSE_SDL=2 -sUSE_LIBPNG=1)

# Kept in step with docs/provenance.md. ASYNCIFY is not optional: I_Sleep
# yields through it, and without that the browser event loop never runs
# while the network code is waiting, so every connect times out.
# -lwebsocket.js supplies the transport's emscripten_websocket_* symbols.

target_link_options(doom PRIVATE
    -Oz
    -sWASM=1
    -sUSE_SDL=2
    -sUSE_LIBPNG=1
    -sALLOW_MEMORY_GROWTH=1
    -sNO_EXIT_RUNTIME=1
    -sEXPORTED_RUNTIME_METHODS=FS,UTF8ToString
    -sMODULARIZE=1
    -sASSERTIONS=0
    -sASYNCIFY
    -sASYNCIFY_STACK_SIZE=32768
    -lwebsocket.js
    )

else()

# Native links the real libraries the browser build gets from emscripten ports.
# SDL2_net is required rather than optional: it is what net_sdl.c needs, and a
# native engine without its transport is not a target this contract offers.

# The vendored sdl_mixer is partial: mixer.c keeps SDL_mixer's format dispatch
# in Mix_LoadWAV_RW, whose AIFF and VOC branches call loaders this tree does
# not carry. Nothing reaches that function. Doom loads its sfx from DMX lumps
# through i_sdlsound.c, and the browser build has always dropped the whole
# function during link, which is why those references have never been visible:
# the symbols do not appear in doom.wasm at all.
#
# Sectioned compilation plus --gc-sections gives the native link the same
# outcome by the same mechanism, rather than stubbing symbols the engine would
# be unable to use or vendoring loaders it never calls.

target_compile_options(doom PRIVATE -ffunction-sections -fdata-sections)
target_link_options(doom PRIVATE -Wl,--gc-sections)

find_package(PkgConfig REQUIRED)
pkg_check_modules(SDL2 REQUIRED sdl2)
pkg_check_modules(SDL2_NET REQUIRED SDL2_net)
pkg_check_modules(PNG REQUIRED libpng)

target_include_directories(doom PRIVATE
    ${SDL2_INCLUDE_DIRS} ${SDL2_NET_INCLUDE_DIRS} ${PNG_INCLUDE_DIRS})

target_link_directories(doom PRIVATE
    ${SDL2_LIBRARY_DIRS} ${SDL2_NET_LIBRARY_DIRS} ${PNG_LIBRARY_DIRS})

target_link_libraries(doom PRIVATE
    ${SDL2_LIBRARIES} ${SDL2_NET_LIBRARIES} ${PNG_LIBRARIES} m)

endif()
