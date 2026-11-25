// The ui example (examples/ui) for the oscar64 C compiler: boxes drawn with
// the PETSCII line characters: square and round corners, with titles, and
// a panel on top of the others.
//
// oscar64's library has no boxes: box() is c64::ui's routine in C.

#include <string.h>
#include <c64/vic.h>

#define Screen	((char *)0x0400)
#define Color	((char *)0xd800)

#define SQUARE	0
#define ROUND	1
#define FILL	2		// style bit: clear the inside

static void put_char(char col, char row, char code, char color)
{
	unsigned off = 40 * row + col;
	Screen[off] = code;
	Color[off] = color;
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

// A box of w columns and h rows, lines included; the title (screen codes)
// goes into the top line from the third column and is cut off at w - 4.
// A box that is smaller than 2 x 2 or does not fit is not drawn.
static void box(char col, char row, char w, char h, char style, char color, const char * title)
{
	static const char corners[2][4] = {{0x70, 0x6e, 0x6d, 0x7d}, {0x55, 0x49, 0x4a, 0x4b}};

	if (w < 2 || h < 2 || col + w > 40 || row + h > 25)
		return;
	char right = col + w - 1, bottom = row + h - 1;
	for (char x = col + 1; x < right; x++)
	{
		put_char(x, row, 0x40, color);
		put_char(x, bottom, 0x40, color);
	}
	for (char y = row + 1; y < bottom; y++)
	{
		put_char(col, y, 0x5d, color);
		put_char(right, y, 0x5d, color);
		if (style & FILL)
		{
			for (char x = col + 1; x < right; x++)
				put_char(x, y, 0x20, color);
		}
	}
	const char * c = corners[style & 1];
	put_char(col, row, c[0], color);
	put_char(right, row, c[1], color);
	put_char(col, bottom, c[2], color);
	put_char(right, bottom, c[3], color);
	for (char i = 0; title[i] && i + 4 < w; i++)
		put_char(col + 2 + i, row, title[i], color);
}

int main(void)
{
	vic.color_border = VCOL_BLACK;
	vic.color_back = VCOL_BLACK;
	memset(Screen, 0x20, 1000);
	memset(Color, VCOL_LT_GREY, 1000);

	box(0, 0, 40, 25, ROUND, VCOL_BLUE, s" c64-ui ");

	box(2, 2, 17, 7, SQUARE, VCOL_WHITE, "");
	put_str(4, 4, s"square", VCOL_LT_GREY);

	box(21, 2, 17, 7, ROUND, VCOL_YELLOW, "");
	put_str(23, 4, s"round", VCOL_LT_GREY);

	box(2, 10, 17, 7, SQUARE, VCOL_CYAN, s" files ");
	put_str(4, 12, s"hello.prg", VCOL_LT_GREY);
	put_str(4, 13, s"notes.seq", VCOL_LT_GREY);

	box(21, 10, 17, 7, ROUND, VCOL_GREEN, s" status ");
	put_str(23, 12, s"all is well", VCOL_LT_GREY);

	// A panel clears what is under it.
	box(11, 15, 18, 6, ROUND | FILL, VCOL_LT_RED, s" quit? ");
	put_str(14, 17, s"yes     no", VCOL_WHITE);

	for (;;)
		;
	return 0;
}
