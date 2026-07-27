cmake_minimum_required(VERSION 3.13)
project(doomit-engine C)

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

if(EMSCRIPTEN)
    list(REMOVE_ITEM ENGINE_SOURCES ${ENGINE_ROOT}/src/net_sdl.c)
else()
    list(REMOVE_ITEM ENGINE_SOURCES ${ENGINE_ROOT}/src/net_websockets.c)
endif()

add_executable(doom ${ENGINE_SOURCES})

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
