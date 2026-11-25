// The sd2iec example (examples/sd2iec) for the oscar64 C compiler: lists the
// root directory of the card in drive 8, then walks into its first
// subdirectory, lists that, and comes back.
//
// The channels are read with oscar64's kernalio library; the commands of
// the SD2IEC and the lines of the directory are as in c64-sd2iec and
// c64::dos.

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <conio.h>
#include <c64/kernalio.h>

#define DEV	8

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

// Prints PETSCII as it is.
static void print_petscii(const char * s)
{
	while (*s)
		putpch(*s++);
}

// Sends a command to the drive.
static bool command(const char * cmd)
{
	krnio_setnam(cmd);
	bool ok = krnio_open(15, DEV, 15);
	krnio_close(15);
	return ok;
}

// The status line of the drive (without its RETURN) and its number;
// -1 if the drive does not answer.
static int status(char * buf)
{
	krnio_setnam("");
	if (!krnio_open(15, DEV, 15))
	{
		krnio_close(15);
		return -1;
	}
	int n = krnio_gets(15, buf, 40);
	krnio_close(15);
	if (n < 0)
		n = 0;
	if (n > 0 && buf[n - 1] == 13)
		n--;
	buf[n] = 0;
	return n >= 2 ? (char)((buf[0] - '0') * 10 + (buf[1] - '0')) : 0;
}

static void print_status(void)
{
	char s[40];
	if (status(s) >= 0)
	{
		print_petscii(s);
		puts("\n");
	}
}

// Whether the drive is an SD2IEC: its reset message says so.
static bool detect(void)
{
	char s[40];
	if (!command("UI") || status(s) < 0)
		return false;
	return strstr(s, "SD2IEC") != nullptr;
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

// Prints the directory; first gets the name of its first subdirectory
// (empty if there is none). False if the directory cannot be read.
static bool list(char * first)
{
	struct DirEntry e;

	first[0] = 0;
	done = false;
	krnio_setnam("$");
	if (!krnio_open(2, DEV, 0))
	{
		puts("CANNOT READ THE DIRECTORY: ERROR ");
		put_u(krnio_status());
		puts("\n");
		return false;
	}
	get();			// load address
	get();
	line(&e);		// the header
	while (line(&e) && e.name[0])
	{
		puts("  ");
		put_u(e.blocks);
		puts(" ");
		print_petscii(e.name);
		puts(" ");
		print_petscii(e.kind);
		puts("\n");
		if (!first[0] && !strcmp(e.kind, "DIR"))
			strcpy(first, e.name);
	}
	krnio_close(2);
	return true;
}

int main(void)
{
	char name[17], other[17], s[40];

	if (!detect())
	{
		puts("DRIVE ");
		put_u(DEV);
		puts(" IS NOT AN SD2IEC\n");
		return 0;
	}
	command("X?");
	print_status();

	command("CD//");
	puts("ROOT:\n");
	if (!list(name) || !name[0])
	{
		puts("NO SUBDIRECTORIES\n");
		return 0;
	}

	// The name came from the directory: it is PETSCII already, and the
	// command wants it as it is.
	char cmd[20] = "CD:";
	strcat(cmd, name);
	if (command(cmd) && status(s) < 20)
	{
		print_petscii(name);
		puts(":\n");
		list(other);
		command("CD_");
	}
	print_status();
	return 0;
}
