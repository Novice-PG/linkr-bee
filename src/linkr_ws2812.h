/*
 * WS2812-compatible RGB status LED for the Linkr Bee X1.0.
 *
 * The board's U1 (Everlight EY-PB1615RGBIC-010F) is a single-wire,
 * WS2812-compatible part on GPIO8. Zephyr has no LED strip driver for the
 * ESP32-C3 (the in-tree gpio driver is nRF-only and the SPI driver would cost
 * a second pin plus DMA), and the strip is one pixel, so the waveform is
 * generated here instead: a 24-bit frame of 1.25 us cells, each cell high for
 * 350 ns (0) or 750 ns (1), written under an interrupt lock with the RISC-V
 * cycle counter as the clock. One pixel is 30 us of locked time.
 *
 * Board caveat that no firmware can fix: on rev X1.00 the schematic places
 * C31 (100 nF) on this same data line. Its 10 kOhm pull-up makes the edges
 * millisecond-scale, so the LED only works once C31 is removed or left
 * unpopulated. See docs/BEE_X1.0_HARDWARE.md.
 */
#ifndef LINKR_WS2812_H_
#define LINKR_WS2812_H_

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

struct linkr_rgb {
	uint8_t r;
	uint8_t g;
	uint8_t b;
};

/* True when the board description carries an addressable RGB status LED. */
bool linkr_ws2812_available(void);

/* Configure the data pin as an output. Returns -ENODEV when this board has no
 * RGB LED node, so callers can treat it as "feature absent". */
int linkr_ws2812_init(void);

/* Push one frame to the strip. Pixels are given in logical RGB order; the
 * byte order handed to the device comes from the devicetree. Interrupts are
 * locked for CHAIN_LENGTH * 30 us while the frame is clocked out. */
int linkr_ws2812_write(const struct linkr_rgb *pixels, size_t count);

#endif /* LINKR_WS2812_H_ */
