// The drawing example (examples/drawing) for the oscar64 C compiler, with
// the same memory layout: colour matrix at $4000, bitmap at $6000.
//
//   -dTIMED      measure the phases with the CIA 2 timers and print the
//                cycles instead of waiting for a key
//   -dUNCLIPPED  bmu_line and no bounds check in the circle (rust64's
//                routines always clip)

#include <c64/vic.h>
#include <c64/cia.h>
#include <gfx/bitmap.h>
#include <string.h>
#ifdef TIMED
#include <stdio.h>
#endif

#define MATRIX ((char *)0x4000)
#define BITMAP ((char *)0x6000)

static Bitmap bm;
#ifndef UNCLIPPED
// not const: the installed oscar64 crashes at -O2 on a constant clip rectangle
static ClipRect cr = {0, 0, 320, 200};
#endif

static void put(int x, int y)
{
#ifndef UNCLIPPED
	if (x < 0 || y < 0 || x >= 320 || y >= 200)
		return;
#endif
	bm_set(&bm, x, y);
}

static void line(int x0, int y0, int x1, int y1)
{
#ifdef UNCLIPPED
	bmu_line(&bm, x0, y0, x1, y1, 0xff, LINOP_SET);
#else
	bm_line(&bm, &cr, x0, y0, x1, y1, 0xff, LINOP_SET);
#endif
}

// The outline of the rectangle with the corners (x, y) and (r, b).
static void rect(int x, int y, int r, int b)
{
	line(x, y, r, y);
	line(x, b, r, b);
	line(x, y, x, b);
	line(r, y, r, b);
}

// A circle (midpoint algorithm), as in lib/c64/src/gfx.rs.
static void circle(int cx, int cy, int r)
{
	int x = r, y = 0;
	int err = 1 - r;
	while (x >= y)
	{
		put(cx + x, cy + y);
		put(cx + y, cy + x);
		put(cx - y, cy + x);
		put(cx - x, cy + y);
		put(cx - x, cy - y);
		put(cx - y, cy - x);
		put(cx + y, cy - x);
		put(cx + x, cy - y);
		y++;
		if (err < 0)
			err += 2 * y + 1;
		else
		{
			x--;
			err += 2 * (y - x) + 1;
		}
	}
}

#ifdef TIMED
// CIA 2: timer A counts cycles, timer B its underflows.
static void start(void)
{
	cia2.cra = 0;
	cia2.crb = 0;
	cia2.ta = 0xffff;
	cia2.tb = 0xffff;
	cia2.crb = 0x51;
	cia2.cra = 0x11;
}

static unsigned long stop(void)
{
	cia2.cra = 0;
	unsigned a = cia2.ta, b = cia2.tb;
	return ((unsigned long)~b << 16) | (unsigned)~a;
}

static unsigned long t[5];
#define START	start()
#define STOP(i)	t[i] = stop()
#else
#define START
#define STOP(i)
#endif

int main(void)
{
	vic.color_border = VCOL_BLACK;

	START;
	bm_init(&bm, BITMAP, 40, 25);
	bm_fill(&bm, 0);
	memset(MATRIX, VCOL_LT_GREEN << 4 | VCOL_BLACK, 1000);
	vic_setmode(VICM_HIRES, MATRIX, BITMAP);
	STOP(0);

	START;
	for (int i = 0; i < 320; i += 16)
		line(i, 0, 319 - i, 199);
	STOP(1);

	START;
	rect(40, 30, 279, 169);
	STOP(2);

	START;
	for (int r = 10; r <= 60; r += 10)
		circle(160, 100, r);
	STOP(3);

	START;
	for (char c = 0; c < 40; c++)
		MATRIX[24 * 40 + c] = (c & 15 | 1) << 4;
	STOP(4);

#ifdef TIMED
	vic_setmode(VICM_TEXT, (char *)0x0400, (char *)0x1000);
	printf("SETUP %lu\nLINES %lu\nRECT %lu\nCIRCLES %lu\nCELLS %lu\n", t[0], t[1], t[2], t[3], t[4]);
	printf("TOTAL %lu\n", t[0] + t[1] + t[2] + t[3] + t[4]);
#else
	char ch;
	do
	{
		ch = __asm {
			jsr 0xffe4
			sta accu
		};
	} while (!ch);
	vic_setmode(VICM_TEXT, (char *)0x0400, (char *)0x1000);
#endif
	return 0;
}
