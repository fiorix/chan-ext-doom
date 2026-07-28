cmake_minimum_required(VERSION 3.13)

# This entry point builds the browser engine and nothing else.
#
# The refusal below is deliberate, not a missing feature. This fork's source is
# not native-ready: fifteen translation units include <emscripten.h>
# unconditionally, there are twenty-one EM_ASM sites across video, audio,
# input, joystick, GIF capture, ENDOOM and the game layer, and d_main.c inverts
# the game loop through emscripten_set_main_loop. Selecting net_sdl.c instead
# of net_websockets.c does not change any of that. A native target would
# configure, then fail partway through compiling with a missing emscripten.h,
# which reads like a broken build rather than an unfinished port.
#
# The check runs before project() so configure fails before CMake compiles even
# its own compiler-identification program, and long before anything probes for
# SDL. Native UDP interop is an M2 exit requirement and net_sdl.c is kept as
# restored, reviewed source for it; it is not presently buildable.

set(DOOMIT_TARGETING_EMSCRIPTEN FALSE)

if(CMAKE_SYSTEM_NAME STREQUAL "Emscripten")
    set(DOOMIT_TARGETING_EMSCRIPTEN TRUE)
elseif(CMAKE_TOOLCHAIN_FILE MATCHES "Emscripten\\.cmake$")
    set(DOOMIT_TARGETING_EMSCRIPTEN TRUE)
endif()

if(NOT DOOMIT_TARGETING_EMSCRIPTEN)
    message(FATAL_ERROR
        "doomit-engine: browser-only build; refusing a non-Emscripten toolchain.\n"
        "\n"
        "Configure through emsdk instead:\n"
        "    source ~/dev/emsdk/emsdk_env.sh\n"
        "    emcmake cmake -S engine -B <build-dir>\n"
        "\n"
        "This is not a toolchain detection failure. The engine sources depend "
        "on Emscripten unconditionally, so no native engine can be produced "
        "from this tree today. A real native engine, including native UDP "
        "interop, is M2 work; src/net_sdl.c is restored source kept for it and "
        "is not presently buildable.")
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

if(NOT DOOMIT_COMPILER_IS_EMSCRIPTEN)
    message(FATAL_ERROR
        "doomit-engine: browser-only build; the configured C compiler is not "
        "an Emscripten compiler.\n"
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

# The transport is selected in two places that must agree: net_transport.h
# picks the net_module_t symbol, and the build picks the translation unit.
# Compiling the wrong one fails at the include (SDL_net.h) or at the link
# (undefined net_sdl_module).
#
# Only the WebSocket transport is ever built here. net_sdl.c stays in the tree
# as restored source for the M2 native engine and is excluded unconditionally,
# because there is no configuration of this entry point that builds it.

list(REMOVE_ITEM ENGINE_SOURCES ${ENGINE_ROOT}/src/net_sdl.c)

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
