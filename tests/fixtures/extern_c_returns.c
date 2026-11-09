/* The C side of extern_c_returns_test.siox: one function per platform
 * struct-return convention, a 128-bit integer by pointer and in registers,
 * and floats inside a struct. Linked with `sioxc --test --link`. */
#include <stdint.h>

typedef struct { int64_t a, b, c; } Big;     /* 24 bytes: sret */
Big make_big(int64_t x) { Big r = { x, x * 2, x * 3 }; return r; }

typedef struct { double x, y; } Pt;          /* two SSE eightbytes / HFA */
Pt make_pt(double v) { Pt p = { v, v + 1 }; return p; }

typedef struct { int32_t a; double b; } Mix; /* INTEGER then SSE */
Mix make_mix(int32_t a) { Mix m = { a, a * 0.5 }; return m; }

typedef struct { float f, g; } Two;          /* one SSE eightbyte of floats */
Two make_two(float v) { Two t = { v, v * 4 }; return t; }

/* Floats are four bytes each (#540): twelve bytes in two SSE eightbytes,
   and sixteen in two full ones. */
typedef struct { float a, b, c; } Three;
Three make_three(float a, float b, float c) { Three t = { a, b, c }; return t; }

typedef struct { float a, b, c, d; } Four;
Four make_four(float a, float b, float c, float d) { Four f = { a, b, c, d }; return f; }

unsigned __int128 wide_next(const unsigned __int128 *p) { return *p + 1; }
