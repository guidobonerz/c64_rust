// The border example (examples/border) for the oscar64 C compiler: the
// smallest program, one store.

int main(void)
{
	*(volatile char *)0xd020 = 5;
	return 0;
}
