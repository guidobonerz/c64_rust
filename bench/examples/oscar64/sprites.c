// The sprites example (examples/sprites) for the oscar64 C compiler: move a
// ball with the joystick in port 2; two raster interrupts paint a colour
// band behind the title line. Fire changes the colour.
//
// The raster interrupts are oscar64's rasterirq library: each is a short
// piece of generated code that stores one value.

#include <string.h>
#include <conio.h>
#include <c64/vic.h>
#include <c64/joystick.h>
#include <c64/sprites.h>
#include <c64/rasterirq.h>

#define Screen	((char *)0x0400)
#define Color	((char *)0xd800)

// A 24x21 ball, 3 bytes per line.
static const char ball[63] = {
	0x00, 0x7e, 0x00, 0x03, 0xff, 0xc0, 0x07, 0xff, 0xe0, 0x1f, 0xff, 0xf8, 0x1f, 0xff, 0xf8,
	0x3f, 0xff, 0xfc, 0x7f, 0xff, 0xfe, 0x7f, 0xff, 0xfe, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
	0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f, 0xff, 0xfe, 0x7f, 0xff, 0xfe,
	0x3f, 0xff, 0xfc, 0x1f, 0xff, 0xf8, 0x1f, 0xff, 0xf8, 0x07, 0xff, 0xe0, 0x03, 0xff, 0xc0,
	0x00, 0x7e, 0x00,
};

static RIRQCode band_on, band_off;

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

int main(void)
{
	vic.color_border = VCOL_BLACK;
	vic.color_back = VCOL_BLACK;
	clrscr();
	put_str(11, 1, s"rust64 sprite demo", VCOL_WHITE);
	put_str(8, 23, s"joystick 2 - fire: colour", VCOL_MED_GREY);

	memcpy((char *)(13 * 64), ball, 63);
	spr_init(Screen);
	spr_set(0, true, 160, 140, 13, VCOL_YELLOW, false, false, false);

	rirq_init(true);
	rirq_build(&band_on, 1);
	rirq_write(&band_on, 0, &vic.color_back, VCOL_BLUE);
	rirq_set(0, 50, &band_on);
	rirq_build(&band_off, 1);
	rirq_write(&band_off, 0, &vic.color_back, VCOL_BLACK);
	rirq_set(1, 66, &band_off);
	rirq_sort();
	rirq_start();

	int x = 160;
	char y = 140;
	char color = VCOL_YELLOW;
	bool fire_held = false;
	for (;;)
	{
		vic_waitFrame();
		joy_poll(0);
		if (joyx[0] < 0 && x > 24)
			x -= 2;
		if (joyx[0] > 0 && x < 320)
			x += 2;
		if (joyy[0] < 0 && y > 50)
			y -= 2;
		if (joyy[0] > 0 && y < 229)
			y += 2;
		bool fire = joyb[0];
		if (fire && !fire_held)
		{
			color = (color + 1) & 15;
			if (color == VCOL_BLACK)
				color = VCOL_WHITE;
			spr_color(0, color);
		}
		fire_held = fire;
		spr_move(0, x, y);
	}
	return 0;
}
