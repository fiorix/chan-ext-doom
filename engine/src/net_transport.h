//
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
//     Selects the net_module_t that carries packets off this machine.
//
//     Both transports speak the same Chocolate wire protocol, so everything
//     above this header is transport-agnostic. Only the carrier differs:
//     browser builds reach a room router over a WebSocket, native builds
//     talk UDP so they can interoperate with stock chocolate-doom.
//

#ifndef NET_TRANSPORT_H
#define NET_TRANSPORT_H

#include "net_defs.h"

#ifdef __EMSCRIPTEN__

#include "net_websockets.h"
#define NET_TRANSPORT_MODULE net_websockets_module

#else

#include "net_sdl.h"
#define NET_TRANSPORT_MODULE net_sdl_module

#endif

#endif /* #ifndef NET_TRANSPORT_H */
