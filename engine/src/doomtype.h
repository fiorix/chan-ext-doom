//
// Copyright(C) 1993-1996 Id Software, Inc.
// Copyright(C) 2005-2014 Simon Howard
//
// This program is free software; you can redistribute it and/or
// modify it under the terms of the GNU General Public License
// as published by the Free Software Foundation; either version 2
// of the License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// DESCRIPTION:
//	Simple basic typedefs, isolated here to make it easier
//	 separating modules.
//    


#ifndef __DOOMTYPE__
#define __DOOMTYPE__

#include "config.h"

#ifndef MAX
#define MAX(a,b) ((a)>(b)?(a):(b))
#endif
#ifndef MIN
#define MIN(a,b) ((a)<(b)?(a):(b))
#endif
#ifndef BETWEEN
#define BETWEEN(l,u,x) ((l)>(x)?(l):(x)>(u)?(u):(x))
#endif

#if defined(_MSC_VER) && !defined(__cplusplus)
#define inline __inline
#endif

// #define macros to provide functions missing in Windows.
// Outside Windows, we use strings.h for str[n]casecmp.


#if !HAVE_DECL_STRCASECMP || !HAVE_DECL_STRNCASECMP

#include <string.h>
#if !HAVE_DECL_STRCASECMP
#define strcasecmp stricmp
#endif
#if !HAVE_DECL_STRNCASECMP
#define strncasecmp strnicmp
#endif

#else

#include <strings.h>

#endif


//
// The packed attribute forces structures to be packed into the minimum 
// space necessary.  If this is not done, the compiler may align structure
// fields differently to optimize memory access, inflating the overall
// structure size.  It is important to use the packed attribute on certain
// structures where alignment is important, particularly data read/written
// to disk.
//

#ifdef __GNUC__

#if defined(_WIN32) && !defined(__clang__)
#define PACKEDATTR __attribute__((packed,gcc_struct))
#else
#define PACKEDATTR __attribute__((packed))
#endif

#define PRINTF_ATTR(fmt, first) __attribute__((format(printf, fmt, first)))
#define PRINTF_ARG_ATTR(x) __attribute__((format_arg(x)))

#else
#define PACKEDATTR
#define PRINTF_ATTR(fmt, first)
#define PRINTF_ARG_ATTR(x)
#endif

#ifdef __WATCOMC__
#define PACKEDPREFIX _Packed
#else
#define PACKEDPREFIX
#endif

#define PACKED_STRUCT(...) PACKEDPREFIX struct __VA_ARGS__ PACKEDATTR

// C99 integer types; with gcc we just use this.  Other compilers
// should add conditional statements that define the C99 types.

// What is really wanted here is stdint.h; however, some old versions
// of Solaris don't have stdint.h and only have inttypes.h (the 
// pre-standardisation version).  inttypes.h is also in the C99 
// standard and defined to include stdint.h, so include this. 

#include <inttypes.h>

// One fixed width, in every translation unit, whatever the include order.
//
// Upstream picks builtin bool whenever <stdbool.h> has already been seen and
// a four-byte enum otherwise, which makes this type's ABI depend on what a
// file happened to include first. That is not survivable here: g_game.c
// reaches <emscripten.h>, and so stdbool.h, before these headers, while
// p_setup.c reaches these headers first. player_t carries boolean members
// (cards[], backpack, didsecret), so sizeof(player_t) differed between those
// two objects, and a write to players[i] used one stride against an
// allocation made with the other. That overruns into neighbouring globals:
// it corrupted gameepisode and deathmatch, which surfaced far away as a demo
// header recording the wrong level.
//
// int rather than bool keeps this lineage's historical four-byte layout, and
// matches the enum branch's semantics exactly: neither normalises an
// assignment to 0 or 1, so no existing code changes behaviour.

typedef int boolean;

#if !defined(__cplusplus) && !defined(__bool_true_false_are_defined)

// stdbool.h defines these identically, so a later include of it is a legal
// repeat definition rather than a conflict.

#define false 0
#define true 1

#endif

typedef uint8_t byte;
typedef uint8_t pixel_t;
typedef int16_t dpixel_t;

#include <limits.h>

#ifdef _WIN32

#define DIR_SEPARATOR '\\'
#define DIR_SEPARATOR_S "\\"
#define PATH_SEPARATOR ';'

#else

#define DIR_SEPARATOR '/'
#define DIR_SEPARATOR_S "/"
#define PATH_SEPARATOR ':'

#endif

#define arrlen(array) (sizeof(array) / sizeof(*array))

#endif

