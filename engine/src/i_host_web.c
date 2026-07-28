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
//     Host notifications for the browser: CustomEvents on the document, and
//     object URLs for files the page wants to offer as downloads.
//
//     The event names and detail field names are the page's API. They are
//     kept exactly as the loader pages already listen for them.
//

#include <emscripten.h>

#include "i_host.h"

void I_HostLevelLoaded(const char *mapname)
{
    EM_ASM_({
        document.dispatchEvent(new CustomEvent("G_DoLoadLevel", { detail: { mapname: Module.UTF8ToString($0) } }));
    }, mapname);
}

void I_HostLevelCompleted(const char *mapname, int maxkills, int maxitems,
                          int maxsecret, int partime, int killcount,
                          int itemcount, int secretcount, int leveltime)
{
    EM_ASM_({
        var maxkills = $1;
        var maxitems = $2;
        var maxsecret = $3;
        var partime = $4;
        var killcount = $5;
        var itemcount = $6;
        var secretcount = $7;
        var leveltime = $8;
        document.dispatchEvent(new CustomEvent("G_DoCompleted", {
            detail: {
                mapname: Module.UTF8ToString($0),
                maxkills: maxkills,
                maxitems: maxitems,
                maxsecret: maxsecret,
                partime: partime / 35,
                killcount: killcount,
                itemcount: itemcount,
                secretcount: secretcount,
                leveltime: leveltime / 35
            }
        }));
    }, mapname, maxkills, maxitems, maxsecret, partime, killcount, itemcount,
       secretcount, leveltime);
}

void I_HostGameStarted(int skill, int episode, int map)
{
    EM_ASM_({
        document.dispatchEvent(new CustomEvent("G_InitNew", { detail: { skill: $0, episode: $1, map: $2 } }));
    }, skill, episode, map);
}

void I_HostKill(const char *mapname, const char *source)
{
    EM_ASM_({
        document.dispatchEvent(new CustomEvent("P_KillMobj", { detail: { mapname: Module.UTF8ToString($0), source: Module.UTF8ToString($1) } }));
    }, mapname, source);
}

void I_HostFileReady(const char *event, const char *filename, const char *mime)
{
    EM_ASM({
        var name = Module.UTF8ToString($0);
        var url = URL.createObjectURL(new Blob([Module.FS.readFile(name)], {type: Module.UTF8ToString($1)}));
        document.dispatchEvent(new CustomEvent(Module.UTF8ToString($2), { detail: { url: url } }));
        Module.FS.unlink(name);
    }, filename, mime, event);
}

void I_HostSaveWritten(const char *filename)
{
    EM_ASM_({
        try{
            var filename = Module.UTF8ToString($0);
            var buffer = Module.FS.readFile(filename).buffer;
            document.dispatchEvent(new CustomEvent("G_SaveGame", { detail: { filename: filename, buffer: buffer } }));
        }catch(err){}
    }, filename);
}

void I_HostError(const char *message)
{
    EM_ASM_({
        document.dispatchEvent(new CustomEvent("I_Error", { detail: { errorMsg: Module.UTF8ToString($0) } }));
    }, message);
}

void I_HostEndoom(void)
{
    EM_ASM(
        document.dispatchEvent(new CustomEvent("I_Endoom", { detail: {} }));
    );
}

void I_HostResizeCanvas(boolean textmode)
{
    if (textmode)
    {
        EM_ASM(
            if (Module && Module.canvas && typeof Module.canvas.calcRatio == "function"){
                Module.canvas.calcRatio(true);
            }
        );
    }
    else
    {
        EM_ASM(
            if (Module && Module.canvas && typeof Module.canvas.calcRatio == "function"){
                Module.canvas.calcRatio();
            }
        );
    }
}

boolean I_HostIsMobile(void)
{
    return EM_ASM_INT(return +(typeof navigator.maxTouchPoints == "number" ? navigator.maxTouchPoints > 0 : "ontouchstart" in window));
}
