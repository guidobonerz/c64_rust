// The joystick example (examples/joystick) for the oscar64 C compiler: the
// joystick in port 2 moves a ball (fire turns it red), pressed keys are
// shown, RUN/STOP returns to BASIC.

#include <stdio.h>
#include <conio.h>
#include <c64/vic.h>
#include <c64/joystick.h>

#define BALL	81		// screen code of the filled circle
#define TOP	5

#define Screen	((char *)0x0400)
#define Color	((char *)0xd800)

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

// The next key from the keyboard buffer (PETSCII), 0 if there is none.
static char key(void)
{
	return __asm
	{
		jsr $ffe4
		sta accu
		lda #0
		sta accu + 1
	};
}

// PETSCII to a screen code.
static char petscii_to_screen(char c)
{
	if (c == 0xff)
		return 0x5e;
	if (c < 0x20)
		return c + 0x80;
	if (c < 0x40)
		return c;
	if (c < 0x60)
		return c - 0x40;
	if (c < 0x80)
		return c - 0x20;
	if (c < 0xa0)
		return c + 0x40;
	if (c < 0xc0)
		return c - 0x40;
	return c - 0x80;
}

static void put_char(char col, char row, char code, char color)
{
	unsigned off = 40 * row + col;
	Screen[off] = code;
	Color[off] = color;
}

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
	clrscr();
	puts("JOYSTICK 2 MOVES THE BALL, KEYS ARE\n");
	puts("SHOWN, RUN/STOP ENDS.\n");
	put_str(0, 3, s"key:", VCOL_LT_BLUE);
	char x = 20, y = 14;
	char frame = 0;
	for (;;)
	{
		vic_waitFrame();
		if (stop())
			break;
		char k = key();
		if (k)
			put_char(5, 3, petscii_to_screen(k), VCOL_YELLOW);
		joy_poll(0);
		frame++;
		if ((frame & 3) == 0)
		{
			put_char(x, y, ' ', VCOL_WHITE);
			if (joyy[0] < 0 && y > TOP)
				y--;
			if (joyy[0] > 0 && y < 24)
				y++;
			if (joyx[0] < 0 && x > 0)
				x--;
			if (joyx[0] > 0 && x < 39)
				x++;
		}
		put_char(x, y, BALL, joyb[0] ? VCOL_RED : VCOL_WHITE);
	}
	clrscr();
	return 0;
}
