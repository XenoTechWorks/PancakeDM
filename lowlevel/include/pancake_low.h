#ifndef PANCAKE_LOW_H
#define PANCAKE_LOW_H

#include <stdint.h>

/* Version packed as 0x00MMmmpp. */
uint32_t pancake_low_version(void);

/* ---- DRM/KMS ---- */

typedef void pancake_device;

struct pancake_output_info {
    uint32_t width;
    uint32_t height;
    uint32_t refresh_mhz; /* refresh rate in millihertz */
    char name[32];        /* e.g. "HDMI-1" */
};

/* Open a DRM device (NULL = /dev/dri/card0) directly, without a seat. */
pancake_device *pancake_low_open(const char *path);
/* Use a DRM fd from pancake_seat_open_device; the seat keeps owning it. */
pancake_device *pancake_low_open_fd(int fd);
uint32_t pancake_low_output_count(pancake_device *dev);
int pancake_low_output_info(pancake_device *dev, uint32_t index,
                            struct pancake_output_info *info);
/* Fill an output with one 0xXXRRGGBB color. 0 on success, <0 on error. */
int pancake_low_fill(pancake_device *dev, uint32_t index, uint32_t xrgb);
/* Restore the previous screen contents and close the device. */
void pancake_low_close(pancake_device *dev);

/* ---- Hardware cursor ---- */

/* Largest cursor image the GPU takes (usually 64x64). */
void pancake_cursor_size(pancake_device *dev, uint32_t *w, uint32_t *h);
/* Premultiplied ARGB8888, w*h tightly packed. 0 ok; -3 = no cursor plane. */
int pancake_cursor_set(pancake_device *dev, uint32_t index, const uint32_t *pixels,
                       uint32_t w, uint32_t h, int32_t hot_x, int32_t hot_y);
/* Put the cursor's hotspot at (x, y). */
int pancake_cursor_move(pancake_device *dev, uint32_t index, int32_t x, int32_t y);
int pancake_cursor_hide(pancake_device *dev, uint32_t index);

/* ---- Seat (libseat: logind, elogind or seatd) ---- */

typedef void pancake_seat;

enum pancake_seat_event {
    PANCAKE_SEAT_ENABLED = 1,  /* VT is ours again: redraw everything */
    PANCAKE_SEAT_DISABLED = 2, /* stop using devices, then ack_disable */
};

/* Open the seat and wait until it's ours. NULL = no seat backend. */
pancake_seat *pancake_seat_open(void);
const char *pancake_seat_name(pancake_seat *seat);
int pancake_seat_fd(pancake_seat *seat);
/* Bitmask of pancake_seat_event; <0 on error. */
int pancake_seat_dispatch(pancake_seat *seat);
void pancake_seat_ack_disable(pancake_seat *seat);
int pancake_seat_active(pancake_seat *seat);
/* Switch VT/session, e.g. for Ctrl+Alt+F<n>. */
int pancake_seat_switch(pancake_seat *seat, int session);
int pancake_seat_open_device(pancake_seat *seat, const char *path);
void pancake_seat_close_device(pancake_seat *seat, int fd);
void pancake_seat_close(pancake_seat *seat);

/* ---- Virtual terminal (only used without a seat) ---- */

/* Take over the VT: no console text, keys don't reach the shell.
   0 ok, -2 not on a VT (fine). Restored on release, exit, crash or kill. */
int pancake_vt_take(void);
void pancake_vt_release(void);

/* ---- Input (libinput) ---- */

typedef void pancake_input;

enum pancake_input_kind {
    PANCAKE_INPUT_DEVICE_ADDED = 1,
    PANCAKE_INPUT_DEVICE_REMOVED = 2,
    PANCAKE_INPUT_KEY = 3,             /* code = evdev key, pressed */
    PANCAKE_INPUT_MOTION = 4,          /* x, y = relative delta */
    PANCAKE_INPUT_MOTION_ABSOLUTE = 5, /* x, y = 0..1 of the screen */
    PANCAKE_INPUT_BUTTON = 6,          /* code = evdev button, pressed */
    PANCAKE_INPUT_SCROLL = 7,          /* x, y = scroll amount */
};

struct pancake_input_event {
    uint32_t kind;
    uint32_t code;
    uint32_t pressed;
    uint32_t _pad;
    uint64_t time_usec;
    double x;
    double y;
    char name[64]; /* device name, for DEVICE_ADDED/REMOVED */
};

/* Read input from every device on the seat (NULL = open devices directly). */
pancake_input *pancake_input_open(pancake_seat *seat);
/* poll() this fd for readability, then drain with pancake_input_next. */
int pancake_input_fd(pancake_input *in);
/* 1 = event written to *out, 0 = no more events right now. */
int pancake_input_next(pancake_input *in, struct pancake_input_event *out);
/* Release / reopen devices when the seat is disabled / enabled. */
void pancake_input_suspend(pancake_input *in);
int pancake_input_resume(pancake_input *in);
void pancake_input_close(pancake_input *in);

#endif
