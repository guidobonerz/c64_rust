// The hello example (examples/hello) for the oscar64 C compiler: prints a
// greeting and the first Fibonacci numbers, then turns the border green.
// Text goes through puts and utoa: printf alone is about 3.7 KB.

#include <stdio.h>
#include <stdlib.h>
#include <c64/vic.h>

// Prints a number.
static void put_u(unsigned n)
{
	char buf[6];
	utoa(n, buf, 10);
	puts(buf);
}

static unsigned fib(char n)
{
	unsigned a = 0, b = 1;
	for (char i = 0; i < n; i++)
	{
		unsigned c = a + b;
		a = b;
		b = c;
	}
	return a;
}

int main(void)
{
	puts("HELLO FROM RUST64\n");
	for (char i = 0; i < 10; i++)
	{
		put_u(fib(i));
		puts(" ");
	}
	puts("\n");
	vic.color_border = VCOL_GREEN;
	return 0;
}
