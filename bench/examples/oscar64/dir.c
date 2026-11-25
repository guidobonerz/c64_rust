// The dir example (examples/dir) for the oscar64 C compiler: lists the
// directory of drive 8 and the drive status.
//
// The channel is read with oscar64's kernalio library; the lines of the
// directory are taken apart as in c64::dos.

#include <stdio.h>
#include <stdlib.h>
#include <conio.h>
#include <c64/kernalio.h>

struct DirEntry
{
	unsigned	blocks;
	char		name[17];
	char		kind[4];
};

// Prints a number.
static void put_u(unsigned n)
{
	char buf[6];
	utoa(n, buf, 10);
	puts(buf);
}

static bool done;

// One byte of the directory, -1 at its end.
static int get(void)
{
	if (done)
		return -1;
	int c = krnio_getch(2);
	if (c < 0)
	{
		done = true;
		return -1;
	}
	if (c & 0x100)		// the last byte
		done = true;
	return c & 0xff;
}

// The next line of the directory: false at its end.
static bool line(struct DirEntry * e)
{
	int lo = get(), hi = get();
	if (lo < 0 || hi < 0 || (lo == 0 && hi == 0))
		return false;
	lo = get();
	hi = get();
	e->blocks = (lo < 0 || hi < 0) ? 0 : ((unsigned)hi << 8 | lo);
	char state = 0;		// 0 before the name, 1 in it, 2 after it
	char n = 0, t = 0;
	int c;
	e->kind[0] = e->kind[1] = e->kind[2] = ' ';
	e->kind[3] = 0;
	while ((c = get()) > 0)
	{
		if (c == '"')
			state++;
		else if (state == 1)
		{
			if (n < 16)
				e->name[n++] = c;
		}
		else if (state == 2 && c != ' ' && t < 3)
			e->kind[t++] = c;
	}
	e->name[n] = 0;
	return true;
}

// Prints PETSCII as it is.
static void print_petscii(const char * s)
{
	while (*s)
		putpch(*s++);
}

int main(void)
{
	struct DirEntry e;
	unsigned free = 0;

	krnio_setnam("$");
	if (!krnio_open(2, 8, 0))
	{
		puts("CANNOT OPEN DIRECTORY, ERROR ");
		put_u(krnio_status());
		puts("\n");
		return 0;
	}
	get();			// load address
	get();
	puts("DISK: ");
	if (line(&e))
		print_petscii(e.name);
	puts("\n");
	while (line(&e))
	{
		if (e.name[0] == 0)
		{
			// the last line: the free blocks
			free = e.blocks;
			break;
		}
		put_u(e.blocks);
		puts(" ");
		print_petscii(e.name);
		puts(" ");
		print_petscii(e.kind);
		puts("\n");
	}
	put_u(free);
	puts(" BLOCKS FREE\n");
	krnio_close(2);

	char buf[40];
	krnio_setnam("");
	if (krnio_open(15, 8, 15))
	{
		int n = krnio_gets(15, buf, 40);
		krnio_close(15);
		if (n > 0)
		{
			// (the line ends with a RETURN)
			print_petscii(buf);
		}
	}
	return 0;
}
