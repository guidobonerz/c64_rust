// The diagonal example (examples/diagonal) for the oscar64 C compiler: the
// whole screen scrolls smoothly up and to the left over an endless pattern
// drawn with a custom character set. RUN/STOP ends.
//
// Custom characters at $3000, two screens ($0400 and $3C00): while one is
// shown and fine-scrolled, the next position is built in the other one, a
// few rows per frame.
//
//   -dTIMED   measure the start (characters, tile rows, colours, first
//             screen) and the updates of 256 frames with CIA 2's timers and
//             print the cycles instead of running until RUN/STOP

#include <c64/vic.h>
#include <c64/cia.h>
#include <string.h>
#ifdef TIMED
#include <stdio.h>
#endif

#define CHARSET		((char *)0x3000)
#define SCREEN_A	((char *)0x0400)
#define SCREEN_B	((char *)0x3c00)
#define COLOR_RAM	((char *)0xd800)

// $D018: screen at $0400 / $3C00, character set at $3000.
#define SHOW_A		0x1c
#define SHOW_B		0xfc
// $D018 at power-on: screen at $0400, the ROM character set.
#define SHOW_DEFAULT	0x15

#define TILE_W		16
#define TILE_H		8
// A tile row repeated often enough to copy 40 columns from any offset.
#define WIDE		(40 + TILE_W)
// Rows of the hidden screen built per frame.
#define ROWS_PER_FRAME	7
#define HEIGHT		25
#define WIDTH		40

// The custom characters, 8 bytes each (one byte per pixel row).
static const char chars[12][8] = {
	{0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00}, // 0: empty
	{0xc0, 0xe0, 0x70, 0x38, 0x1c, 0x0e, 0x07, 0x03}, // 1: backslash
	{0x03, 0x07, 0x0e, 0x1c, 0x38, 0x70, 0xe0, 0xc0}, // 2: /
	{0x18, 0x3c, 0x7e, 0xff, 0xff, 0x7e, 0x3c, 0x18}, // 3: diamond
	{0x00, 0x00, 0x18, 0x3c, 0x3c, 0x18, 0x00, 0x00}, // 4: dot
	{0x3c, 0x66, 0xc3, 0xc3, 0xc3, 0xc3, 0x66, 0x3c}, // 5: ring
	{0xfc, 0xc6, 0xc6, 0xfc, 0xd8, 0xcc, 0xc6, 0x00}, // 6: R
	{0xc6, 0xc6, 0xc6, 0xc6, 0xc6, 0xc6, 0x7c, 0x00}, // 7: U
	{0x7c, 0xc6, 0xc0, 0x7c, 0x06, 0xc6, 0x7c, 0x00}, // 8: S
	{0xfe, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x00}, // 9: T
	{0x3c, 0x60, 0xc0, 0xfc, 0xc6, 0xc6, 0x7c, 0x00}, // 10: 6
	{0x1c, 0x3c, 0x6c, 0xcc, 0xfe, 0x0c, 0x0c, 0x00}, // 11: 4
};

// The tile as text; code() turns each character into a character number.
static const char tile[TILE_H][TILE_W + 1] = {
	"RUST64    /\\    ",
	"         /  \\   ",
	"  .     /  # \\  ",
	"        \\    /  ",
	"   o     \\  /   ",
	"          \\/    ",
	" #     .        ",
	"            o   ",
};

static char code(char c)
{
	switch (c)
	{
	case '\\': return 1;
	case '/': return 2;
	case '#': return 3;
	case '.': return 4;
	case 'o': return 5;
	case 'R': return 6;
	case 'U': return 7;
	case 'S': return 8;
	case 'T': return 9;
	case '6': return 10;
	case '4': return 11;
	default: return 0;
	}
}

// The colour bands, top to bottom.
static const char bands[5] = {VCOL_LT_RED, VCOL_YELLOW, VCOL_LT_GREEN, VCOL_CYAN, VCOL_LT_BLUE};

// Each tile row as character numbers, repeated to WIDE columns.
static char rows[TILE_H][WIDE];

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

// Copies the custom characters into the character set and prepares the
// tile rows.
static void init(void)
{
	memcpy(CHARSET, chars, sizeof(chars));
	for (char r = 0; r < TILE_H; r++)
		for (char i = 0; i < WIDE; i++)
			rows[r][i] = code(tile[r][i & (TILE_W - 1)]);
	// colour RAM: bands of five rows
	for (char row = 0; row < HEIGHT; row++)
		memset(COLOR_RAM + WIDTH * row, bands[row / 5], WIDTH);
}

// Builds rows first..first + count of the screen for the world position
// (ox, oy): row y shows tile row oy + y from column ox on.
static void build(char * screen, char ox, char oy, char first, char count)
{
	char * dst = screen + WIDTH * first;
	for (char y = first; y < first + count; y++)
	{
		memcpy(dst, rows[(oy + y) & (TILE_H - 1)] + ox, WIDTH);
		dst += WIDTH;
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

static unsigned long stop_timer(void)
{
	cia2.cra = 0;
	unsigned a = cia2.ta, b = cia2.tb;
	return ((unsigned long)~b << 16) | (unsigned)~a;
}
#endif

int main(void)
{
	vic.color_border = VCOL_BLACK;
	vic.color_back = VCOL_BLACK;
#ifdef TIMED
	start();
#endif
	init();
	build(SCREEN_A, 0, 0, 0, HEIGHT);
#ifdef TIMED
	unsigned long init_time = stop_timer();
	cia2.crb = 0;
#endif

	char ox = 0, oy = 0;		// world position of the visible screen
	char fine = 7;			// the pixel shift, 7 down to 0
	bool a_visible = true;
	char built = 0;			// rows of the hidden screen that are ready
	vic.memptr = SHOW_A;

#ifdef TIMED
	unsigned long sum = 0;
	unsigned max = 0;
	unsigned frames = 256;
	while (frames--)
#else
	while (!stop())
#endif
	{
		vic_waitFrame();
#ifdef TIMED
		cia2.cra = 0;
		cia2.ta = 0xffff;
		cia2.cra = 0x11;
#endif
		// In the border: the next pixel position, or the next screen.
		if (fine == 0)
		{
			fine = 7;
			a_visible = !a_visible;
			vic.memptr = a_visible ? SHOW_A : SHOW_B;
			ox = (ox + 1) & (TILE_W - 1);
			oy = (oy + 1) & (TILE_H - 1);
			built = 0;
		}
		else
			fine--;
		vic.ctrl2 = 0xc0 | fine;	// 38 columns, XSCROLL
		vic.ctrl1 = 0x10 | fine;	// screen on, 24 rows, YSCROLL

		// Build a part of the hidden screen: one cell further.
		if (built < HEIGHT)
		{
			char * hidden = a_visible ? SCREEN_B : SCREEN_A;
			char count = HEIGHT - built;
			if (count > ROWS_PER_FRAME)
				count = ROWS_PER_FRAME;
			build(hidden, (ox + 1) & (TILE_W - 1), (oy + 1) & (TILE_H - 1), built, count);
			built += count;
		}
#ifdef TIMED
		cia2.cra = 0;
		unsigned t = 0xffff - cia2.ta;
		sum += t;
		if (t > max)
			max = t;
#endif
	}

	vic.ctrl1 = 0x1b;
	vic.ctrl2 = 0xc8;
	vic.memptr = SHOW_DEFAULT;
	clear();
#ifdef TIMED
	printf("INIT %lu\nSUM %lu\nMAX %u\n", init_time, sum, max);
#endif
	return 0;
}
