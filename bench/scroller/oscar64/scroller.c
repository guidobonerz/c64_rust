// The scroller example (examples/scroller) for the oscar64 C compiler: one
// text row moves left pixel by pixel. RUN/STOP ends.
//
// The two raster interrupts (shift and 38 columns above the row, the normal
// screen below it) are oscar64's rasterirq library: each is a short piece of
// generated code that stores one value.
//
//   -dTIMED   measure the updates of 256 frames with CIA 2's timer A, then
//             what the interrupts cost: the time of a fixed loop with them
//             and without any (screen off, so the VIC takes no cycles)

#include <c64/vic.h>
#include <c64/cia.h>
#include <c64/rasterirq.h>
#include <string.h>
#ifdef TIMED
#include <stdio.h>
#endif

#define ROW		12			// the screen row of the scroller
#define SPEED		2			// pixels per frame (1, 2 or 4)
#define ROW_LINE	(50 + ROW * 8)		// the first raster line of the row

#define NORMAL		0xc8			// $D016: 40 columns
#define SCROLLING	0xc0			// $D016: 38 columns, plus the shift

#define Screen		((char *)0x0400)
#define Color		((char *)0xd800)

// screen codes
static const char message[] =
	s"hello from rust64 on the commodore 64 ... "
	s"this text moves with the vic's xscroll register, set in a raster "
	s"interrupt for this one row ... written in rust, compiled to 6510 code "
	s"... press run/stop to end ...          ";

// The colours of the scroller's columns.
static const char rainbow[8] = {
	VCOL_RED, VCOL_ORANGE, VCOL_YELLOW, VCOL_LT_GREEN, VCOL_CYAN, VCOL_LT_BLUE, VCOL_PURPLE, VCOL_LT_RED
};

static RIRQCode scroll_on, scroll_off;

static void clear(void)
{
	__asm
	{
		lda #147
		jsr $ffd2
	}
}

// Whether RUN/STOP is pressed.
static bool stop(void)
{
	return __asm
	{
		jsr $ffe1
		php
		pla
		and #2
		sta accu
		lda #0
		sta accu + 1
	};
}

// Writes screen codes at a position with a colour.
static void put_str(char col, char row, const char * s, char color)
{
	unsigned off = 40 * row + col;
	char * p = Screen + off;
	char * c = Color + off;
	while (*s)
	{
		*p++ = *s++;
		*c++ = color;
	}
}

// Removes the raster interrupts: the KERNAL's interrupt alone again.
static void irq_stop(void)
{
	__asm
	{
		sei
		lda #0
		sta $d01a
		lda #$ff
		sta $d019
		lda #$31
		sta $0314
		lda #$ea
		sta $0315
		cli
	}
}

#ifdef TIMED
// A fixed amount of work: 3 * 65536 times dex/bne.
static void work(void)
{
	__asm
	{
		lda #3
		sta accu
		ldx #0
		ldy #0
	l:	dex
		bne l
		dey
		bne l
		dec accu
		bne l
	}
}

// CIA 2: timer A counts cycles, timer B its underflows.
static unsigned long timed_work(void)
{
	cia2.cra = 0;
	cia2.crb = 0;
	cia2.ta = 0xffff;
	cia2.tb = 0xffff;
	cia2.crb = 0x51;
	cia2.cra = 0x11;
	work();
	cia2.cra = 0;
	unsigned a = cia2.ta, b = cia2.tb;
	return ((unsigned long)~b << 16) | (unsigned)~a;
}
#endif

int main(void)
{
	vic.color_border = VCOL_BLACK;
	vic.color_back = VCOL_BLACK;
	clear();
	put_str(11, 4, s"rust64 soft scroller", VCOL_WHITE);
	put_str(9, 20, s"press run/stop to end", VCOL_MED_GREY);

	// The scroller's row: empty, in rainbow colours.
	char * row = Screen + 40 * ROW;
	for (char col = 0; col < 40; col++)
	{
		row[col] = ' ';
		Color[40 * ROW + col] = rainbow[(col / 5) & 7];
	}

	// Below the title (row 4), and just below the scroller.
	char xscroll = 7;
	rirq_init_kernal();
	rirq_build(&scroll_on, 1);
	rirq_write(&scroll_on, 0, &vic.ctrl2, SCROLLING | xscroll);
	rirq_set(0, ROW_LINE - 54, &scroll_on);
	rirq_build(&scroll_off, 1);
	rirq_write(&scroll_off, 0, &vic.ctrl2, NORMAL);
	rirq_set(1, ROW_LINE + 10, &scroll_off);
	rirq_sort();
	rirq_start();

	unsigned next = 0;
#ifdef TIMED
	unsigned long sum = 0;
	unsigned max = 0;
	unsigned frames = 256;
	while (frames--)
#else
	while (!stop())
#endif
	{
		// Change things while the beam is below the scroller.
		vic_waitFrame();
#ifdef TIMED
		cia2.cra = 0;
		cia2.ta = 0xffff;
		cia2.cra = 0x11;
#endif
		if (xscroll >= SPEED)
			xscroll -= SPEED;
		else
		{
			// One column to the left, a new character at the right edge.
			memmove(row, row + 1, 39);
			row[39] = message[next];
			next++;
			if (next == sizeof(message) - 1)
				next = 0;
			xscroll += 8 - SPEED;
		}
		rirq_data(&scroll_on, 0, SCROLLING | xscroll);
#ifdef TIMED
		cia2.cra = 0;
		unsigned t = 0xffff - cia2.ta;
		sum += t;
		if (t > max)
			max = t;
#endif
	}

#ifdef TIMED
	// screen off: no bad lines (bit 7 is the raster compare's bit 8: keep it 0)
	vic.ctrl1 = vic.ctrl1 & 0x6f;
	unsigned long with_irq = timed_work();
	__asm { sei }
	unsigned long without = timed_work();
	__asm { cli }
	vic.ctrl1 = (vic.ctrl1 & 0x7f) | 0x10;
#endif
	irq_stop();
	vic.ctrl2 = NORMAL;
	clear();
#ifdef TIMED
	printf("SUM %lu\nMAX %u\nWIRQ %lu\nWSEI %lu\n", sum, max, with_irq, without);
#endif
	return 0;
}
