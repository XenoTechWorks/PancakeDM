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

/* Open a DRM device (NULL = /dev/dri/card0). Returns NULL on failure. */
pancake_device *pancake_low_open(const char *path);
uint32_t pancake_low_output_count(pancake_device *dev);
int pancake_low_output_info(pancake_device *dev, uint32_t index,
                            struct pancake_output_info *info);
/* Fill an output with one 0xXXRRGGBB color. 0 on success, <0 on error. */
int pancake_low_fill(pancake_device *dev, uint32_t index, uint32_t xrgb);
/* Restore the previous screen contents and close the device. */
void pancake_low_close(pancake_device *dev);

#endif
