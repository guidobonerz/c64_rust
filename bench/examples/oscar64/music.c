// The music example (examples/music) for the oscar64 C compiler: plays a
// melody on voice 1 with a bass on voice 2, timed with the jiffy clock.

#include <stdio.h>
#include <conio.h>
#include <c64/vic.h>
#include <c64/sid.h>

// A note: its number (48 = C4, for the border colour), the SID's frequency
// value and the length in jiffies.
struct Tone
{
	char		note;
	unsigned	freq;
	char		len;
};

#define N(name, number, len)	{number, SID_FREQ_PAL(name), len}
#define C4(len)	N(NOTE_C(4), 48, len)
#define D4(len)	N(NOTE_D(4), 50, len)
#define E4(len)	N(NOTE_E(4), 52, len)
#define G4(len)	N(NOTE_G(4), 55, len)

static const struct Tone melody[26] = {
	E4(15), D4(15), C4(15), D4(15),
	E4(15), E4(15), E4(30),
	D4(15), D4(15), D4(30),
	E4(15), G4(15), G4(30),
	E4(15), D4(15), C4(15), D4(15),
	E4(15), E4(15), E4(15), E4(15),
	D4(15), D4(15), E4(15), D4(15),
	C4(60),
};

static const unsigned bass[4] = {
	SID_FREQ_PAL(NOTE_C(3)), SID_FREQ_PAL(NOTE_G(3)), SID_FREQ_PAL(NOTE_A(3)), SID_FREQ_PAL(NOTE_F(3))
};

// The jiffy clock's low byte.
#define Jiffies	(*(volatile char *)0xa2)

static void wait(char jiffies)
{
	char start = Jiffies;
	while ((char)(Jiffies - start) < jiffies)
		;
}

static void reset(void)
{
	for (char v = 0; v < 3; v++)
	{
		sid.voices[v].ctrl = 0;
		sid.voices[v].attdec = 0;
		sid.voices[v].susrel = 0;
	}
	sid.fmodevol = 0;
	sid.resfilt = 0;
}

int main(void)
{
	vic.color_border = VCOL_BLUE;
	vic.color_back = VCOL_BLACK;
	clrscr();
	puts("RUST64 MUSIC\n");
	reset();
	sid.fmodevol = 15;
	sid.voices[0].attdec = 0x09;
	sid.voices[0].susrel = 0x44;
	sid.voices[0].pwm = 0x800;
	sid.voices[1].attdec = 0x04;
	sid.voices[1].susrel = 0xa6;
	for (char i = 0; i < 26; i++)
	{
		const struct Tone * t = melody + i;
		if ((i & 3) == 0)
		{
			sid.voices[1].freq = bass[(i >> 2) & 3];
			sid.voices[1].ctrl = SID_CTRL_TRI | SID_CTRL_GATE;
		}
		sid.voices[0].freq = t->freq;
		sid.voices[0].ctrl = SID_CTRL_RECT | SID_CTRL_GATE;
		vic.color_border = t->note & 15;
		wait(t->len - 3);
		sid.voices[0].ctrl = SID_CTRL_RECT;
		wait(3);
	}
	sid.voices[1].ctrl = SID_CTRL_TRI;
	wait(60);
	reset();
	puts("DONE\n");
	return 0;
}
