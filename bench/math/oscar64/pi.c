// Pi: the counterpart of examples/pi for oscar64. The first 100 digits
// with the spigot algorithm of Rabinowitz and Wagon: an array of 16-bit
// numbers, 32-bit multiplications and divisions, four digits per pass.
//
//   -dTIMED   measure the computation (not the printing) with CIA 2's
//             timers and print the cycles and the sum of the 25 groups
#include <c64/cia.h>
#ifdef TIMED
#include <stdio.h>
#endif

#define DIGITS 100
#define GROUPS (DIGITS / 4)
#define LEN (GROUPS * 14)

static unsigned f[LEN + 1];
static unsigned out[GROUPS];

// Computes the digits, four per element of out.
static void compute(void)
{
	for (unsigned i = 0; i < LEN; i++)
		f[i] = 2000;
	f[LEN] = 0;
	unsigned e = 0;
	unsigned c = LEN;
	for (char k = 0; k < GROUPS; k++)
	{
		unsigned long d = 0;
		unsigned long g = c * 2;
		unsigned b = c;
		for (;;)
		{
			d += (unsigned long)f[b] * 10000;
			g--;
			f[b] = (unsigned)(d % g);
			d /= g;
			g--;
			b--;
			if (b == 0)
				break;
			d *= b;
		}
		out[k] = e + (unsigned)(d / 10000);
		e = (unsigned)(d % 10000);
		c -= 14;
	}
}

static void chrout(char c)
{
	__asm {
		lda c
		jsr 0xffd2
	}
}

static const unsigned units[4] = {1000, 100, 10, 1};

// Prints v with four digits.
static void print4(unsigned v)
{
	for (char i = 0; i < 4; i++)
	{
		char digit = '0';
		while (v >= units[i])
		{
			v -= units[i];
			digit++;
		}
		chrout(digit);
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
#endif

int main(void)
{
#ifdef TIMED
	start();
#endif
	compute();
#ifdef TIMED
	unsigned long time = stop();
#endif
	chrout(147);
	for (char k = 0; k < GROUPS; k++)
		print4(out[k]);
	chrout(13);
#ifdef TIMED
	unsigned long sum = 0;
	for (char k = 0; k < GROUPS; k++)
		sum += out[k];
	printf("TIME %lu\nSUM %lu\n", time, sum);
#endif
	return 0;
}
