cmake_minimum_required(VERSION 3.13)
project(doomit-engine C)

# This mirrors the single-emcc recipe in docs/provenance.md. That recipe is the
# reference: it is what the pinned artifact hashes are reproduced from, and
# anything that makes the two disagree is a bug in this file.

set(ENGINE_ROOT ${CMAKE_CURRENT_LIST_DIR})

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

if(EMSCRIPTEN)
    list(REMOVE_ITEM ENGINE_SOURCES ${ENGINE_ROOT}/src/net_sdl.c)
else()
    list(REMOVE_ITEM ENGINE_SOURCES ${ENGINE_ROOT}/src/net_websockets.c)
endif()

add_executable(doom ${ENGINE_SOURCES})

target_compile_options(doom PRIVATE
    -Oz
    -Wno-#warnings
    -Wno-macro-redefined
    -Wno-switch
    )

if(EMSCRIPTEN)
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
    # Native builds carry the UDP transport, so they need SDL2 and SDL2_net.
    # These fail loudly at configure time rather than as a confusing missing
    # include later.

    find_package(PkgConfig REQUIRED)
    pkg_check_modules(SDL2 REQUIRED sdl2)
    pkg_check_modules(SDL2_NET REQUIRED SDL2_net)

    target_include_directories(doom PRIVATE
        ${SDL2_INCLUDE_DIRS} ${SDL2_NET_INCLUDE_DIRS})
    target_link_libraries(doom PRIVATE
        ${SDL2_LIBRARIES} ${SDL2_NET_LIBRARIES} m)
endif()
