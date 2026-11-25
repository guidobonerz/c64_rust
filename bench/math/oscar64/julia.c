// A Julia set: the counterpart of examples/julia for oscar64.
//
//   oscar64 -Os julia.c                 the text screen (PETSCII)
//   oscar64 -Os -dMULTICOLOR julia.c    the multicolour bitmap
//   -dTIMED   measure the work with CIA 2's timers and print the cycles
//             and the sum of all rounds, in place of waiting for a key
//
// Fixed point: 16 bits with 11 bits behind the point, products in 32 bits.
#include <c64/vic.h>
#include <c64/cia.h>
#include <string.h>
#ifdef MULTICOLOR
#include <gfx/mcbitmap.h>
#endif
#ifdef TIMED
#include <stdio.h>
#endif

#define MAX_ITER 24
#define ONE (1L << 11)

#ifdef MULTICOLOR
#define W 160
#define H 200
#define X0 (-3260)
#define Y0 (-2488)
#define DX 41
#define DY 25
#define MATRIX ((char *)0x4000)
#define BITMAP ((char *)0x6000)
static Bitmap bm;
#else
#define W 40
#define H 25
#define X0 (-3198)
#define Y0 (-2400)
#define DX 164
#define DY 200
#define SCREEN ((char *)0x0400)
#endif
#define COLOR ((char *)0xd800)

// The constant of the set: -0.8 + 0.156i.
#define CX (-1638)
#define CY 319

// The rounds that z = z * z + c takes to leave the circle of radius 2,
// starting at z = (x, y); MAX_ITER if it stays.
static char rounds(int cx, int cy, int x, int y)
{
	char n = 0;
	while (n < MAX_ITER)
	{
		long x2 = ((long)x * x) >> 11;
		long y2 = ((long)y * y) >> 11;
		if (x2 + y2 > 4 * ONE)
			break;
		long xy = ((long)x * y) >> 10;
		x = (int)(x2 - y2) + cx;
		y = (int)xy + cy;
		n++;
	}
	return n;
}

// Draws the picture; the sum of the rounds of all points.
static unsigned long draw(void)
{
#ifdef MULTICOLOR
	bm_init(&bm, BITMAP, 40, 25);
	bm_fill(&bm, 0);
	memset(MATRIX, VCOL_BLUE << 4 | VCOL_LT_BLUE, 1000);
	memset(COLOR, VCOL_WHITE, 1000);
	vic.color_back = VCOL_BLACK;
	vic_setmode(VICM_HIRES_MC, MATRIX, BITMAP);
#else
	memset(SCREEN, 160, 1000);
	memset(COLOR, VCOL_BLACK, 1000);
	char * cell = COLOR;
#endif
	unsigned long sum = 0;
	int py = Y0;
	for (char row = 0; row < H; row++)
	{
		int px = X0;
		for (char col = 0; col < W; col++)
		{
			char n = rounds(CX, CY, px, py);
			sum += n;
#ifdef MULTICOLOR
			if (n < MAX_ITER)
				bmmc_put(&bm, 2 * col, row, n % 3 + 1);
#else
			*cell++ = n < MAX_ITER ? n % 15 + 1 : 0;
#endif
			px += DX;
		}
		py += DY;
	}
	return sum;
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
#endif

static void chrout(char c)
{
	__asm {
		lda c
		jsr 0xffd2
	}
}

int main(void)
{
	vic.color_border = VCOL_BLACK;
	vic.color_back = VCOL_BLACK;
#ifdef TIMED
	start();
	unsigned long sum = draw();
	unsigned long time = stop();
#else
	draw();
	char ch;
	do
	{
		ch = __asm {
			jsr 0xffe4
			sta accu
		};
	} while (!ch);
#endif
#ifdef MULTICOLOR
	vic_setmode(VICM_TEXT, (char *)0x0400, (char *)0x1000);
#endif
	chrout(154);
	chrout(147);
#ifdef TIMED
	printf("TIME %lu\nSUM %lu\n", time, sum);
#endif
	return 0;
}
