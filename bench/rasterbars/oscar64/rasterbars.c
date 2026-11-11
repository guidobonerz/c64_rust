// The rasterbars example (examples/rasterbars) for the oscar64 C compiler:
// three colour bars swing up and down behind the text. RUN/STOP ends.
//
// The stable raster interrupt is the same code as raster.rat, as inline
// assembly; C builds the colour table once per frame.
//
//   -dTIMED   measure the table building of 256 frames with CIA 2's timer A
//             and print the cycles instead of running until RUN/STOP

#include <c64/vic.h>
#include <c64/cia.h>
#include <string.h>
#ifdef TIMED
#include <stdio.h>
#endif

#define RASTER_LINES	96		// a multiple of 8
#define RASTER_FIRST	99		// a bad line: (line & 7) == 3
#define RASTER_IRQ	(RASTER_FIRST - 4)
#define BAR		11

// The colour table, one byte per raster line, with 8 bytes in front of it:
// table - 8, x stays in the page.
char raster_mem[8 + RASTER_LINES];
#pragma align(raster_mem, 256)
#define raster_table (raster_mem + 8)

volatile char raster_frame;

// The raster interrupt, one piece of assembly (its parts refer to each
// other).
//
// irq1, at line RASTER_IRQ (the KERNAL has pushed A, X, Y), arms the second
// interrupt and runs NOPs until that comes.
//
// irq2, at line RASTER_IRQ + 2, interrupted a NOP and starts at cycle 40 or
// 41 of the line. From `sync` on every cycle is counted; that part must be
// in one page (branches that cross a page take a cycle more).
//
// The code has a page of its own ($0900), in front of the rest of the
// program.
#pragma region(main, 0x0a00, 0xa000, , , {code, data, bss, heap, stack})
#pragma section(irqcode, 0)
#pragma region(irq, 0x0900, 0x0a00, , , {irqcode})
#pragma code(irqcode)

__asm raster_irq
{
irq1:
	lda #1
	sta $d019
	lda #RASTER_IRQ + 2
	sta $d012
	lda #<irq2
	sta $0314
	lda #>irq2
	sta $0315
	tsx
	cli
	nop
	nop
	nop
	nop
	nop
	nop
	nop
	nop
	nop
	nop
	nop
	nop
	nop
	nop
	nop
	nop
	nop
	nop
	nop
	nop
	nop
	nop
	nop
	nop
	nop
	nop
	nop
	nop
	nop
	nop
	nop
	nop
	nop
	nop
	nop
	nop
	nop
	nop
	nop
	nop
	jmp $ea81
irq2:
	txs
	bit $ea
	nop
	nop
	nop
	nop
	nop
	nop
	lda $d012
	cmp $d012
	beq sync
sync:
	ldx #0
	ldy #9
wait:
	dey
	bne wait
	nop
	nop
	nop
group:
	lda raster_mem + 8, x
	sta $d020
	sta $d021
	txa
	clc
	adc #8
	tax
	lda raster_mem + 1, x
	sta $d020
	sta $d021
	jsr d51
	lda raster_mem + 2, x
	sta $d020
	sta $d021
	jsr d51
	lda raster_mem + 3, x
	sta $d020
	sta $d021
	jsr d51
	lda raster_mem + 4, x
	sta $d020
	sta $d021
	jsr d51
	lda raster_mem + 5, x
	sta $d020
	sta $d021
	jsr d51
	lda raster_mem + 6, x
	sta $d020
	sta $d021
	jsr d51
	lda raster_mem + 7, x
	sta $d020
	sta $d021
	jsr d46
	cpx #RASTER_LINES
	bne group
	lda #0
	sta $d020
	sta $d021
	jmp done
d51:
	bit $ea
	nop
d46:
	ldy #6
d:
	dey
	bne d
	bit $ea
	rts
done:
	lda #<irq1
	sta $0314
	lda #>irq1
	sta $0315
	lda #RASTER_IRQ
	sta $d012
	lda #1
	sta $d019
	inc raster_frame
	jmp $ea31
}
#pragma code(code)

static void raster_start(void)
{
	__asm
	{
		sei
		lda #$7f
		sta $dc0d
		lda $dc0d
		lda #RASTER_IRQ
		sta $d012
		lda $d011
		and #$7f
		sta $d011
		lda #<raster_irq
		sta $0314
		lda #>raster_irq
		sta $0315
		lda #1
		sta $d01a
		sta $d019
		cli
	}
}

static void raster_stop(void)
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
		lda #$81
		sta $dc0d
		cli
	}
}

// Waits until the bars of this frame have been drawn.
static void raster_wait(void)
{
	char f = raster_frame;
	while (f == raster_frame)
		;
}

// The colours of the bars, from the top edge to the bottom edge.
static const char bars[3][BAR] = {
	{9, 2, 8, 10, 7, 1, 7, 10, 8, 2, 9},
	{11, 5, 5, 3, 13, 1, 13, 3, 5, 5, 11},
	{6, 4, 14, 14, 3, 1, 3, 14, 14, 4, 6},
};

// The top line of a bar over time: a sine curve from 0 to LINES - BAR.
static const char sine[64] = {
	42, 47, 51, 55, 59, 63, 66, 69, 73, 75, 78, 80, 82, 83, 84, 85, 85, 85, 84, 83, 82, 80, 78,
	75, 73, 69, 66, 63, 59, 55, 51, 47, 43, 38, 34, 30, 26, 22, 19, 16, 12, 10, 7, 5, 3, 2, 1, 0,
	0, 0, 1, 2, 3, 5, 7, 10, 12, 16, 19, 22, 26, 30, 34, 38,
};

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
	char * p = (char *)0x0400 + off;
	char * c = (char *)0xd800 + off;
	while (*s)
	{
		*p++ = *s++;
		*c++ = color;
	}
}

int main(void)
{
	vic.color_border = VCOL_BLACK;
	vic.color_back = VCOL_BLACK;
	clear();
	put_str(10, 2, s"rust64 raster bars", VCOL_WHITE);
	put_str(4, 11, s"one colour per raster line, with", VCOL_WHITE);
	put_str(4, 13, s"a stable raster interrupt", VCOL_WHITE);
	put_str(9, 22, s"press run/stop to end", VCOL_MED_GREY);

	raster_start();

	char phase = 0;
#ifdef TIMED
	unsigned long sum = 0;
	unsigned max = 0;
	unsigned frames = 256;
	while (frames--)
#else
	while (!stop())
#endif
	{
		// The beam is below the bars: time to build the next frame.
		raster_wait();
#ifdef TIMED
		cia2.cra = 0;
		cia2.ta = 0xffff;
		cia2.cra = 0x11;
#endif
		memset(raster_table, 0, RASTER_LINES);
		char at = phase;
		for (char b = 0; b < 3; b++)
		{
			memcpy(raster_table + sine[at & 63], bars[b], BAR);
			at += 9;
		}
		phase++;
#ifdef TIMED
		cia2.cra = 0;
		unsigned t = 0xffff - cia2.ta;
		sum += t;
		if (t > max)
			max = t;
#endif
	}

	raster_stop();
	clear();
#ifdef TIMED
	printf("SUM %lu\nMAX %u\n", sum, max);
#endif
	return 0;
}
