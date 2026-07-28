#include <stdlib.h>
#include <string.h>

#include "i_gif.h"
#include "i_video.h"
#include "w_wad.h"
#include "deh_str.h"
#include "z_zone.h"
#include "gifenc.h"

#include "i_host.h"
#define GIF_FRAME_SIZE SCREENWIDTH * SCREENHEIGHT
#define GIF_MAX_FRAME_COUNT 128

ge_GIF *gif = NULL;
unsigned int frame_count = 0;

void I_StartGIF()
{
    gif = ge_new_gif("temp.gif", SCREENWIDTH, SCREENHEIGHT, W_CacheLumpName (DEH_String("PLAYPAL"), PU_CACHE), 8, 0);
    frame_count = 0;
}

void I_CloseGIF()
{
    if (gif == NULL)
        return;

    ge_close_gif(gif);
    gif = NULL;

    I_HostFileReady("I_CloseGIF", "temp.gif", "image/gif");
}

void I_AddFrameGIF()
{
    if (gif == NULL)
        return;

    memcpy(gif->frame, I_VideoBuffer, GIF_FRAME_SIZE);
    ge_add_frame(gif, 1);
    frame_count++;

    if (frame_count > GIF_MAX_FRAME_COUNT)
        I_CloseGIF();
}