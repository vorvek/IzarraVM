/*
 * whetstone.c - the Whetstone (Curnow / Wichmann) double-precision benchmark,
 * version 1.2 (Rich Painter 1998, public domain), adapted to run freestanding
 * under the IzarraVM Neurketa harness exactly as dhrystone.exe was: every host
 * service is removed (no printf, no timing, no argv; POUT folds to a checksum).
 * The module sequence and the per-module loop weights (N2..N11) are the original,
 * so the standard floating-point operation MIX still holds - that is what makes
 * this the FP oracle for the per-mode fp_timing factor.
 *
 * The six libm transcendentals are provided as thin inline-8087 #pragma aux
 * wrappers (FSIN / FCOS / FPATAN / F2XM1 / FYL2X / FSQRT), so the .EXE links with
 * ZERO libraries, like dhrystone.exe. It is a small-model .EXE for the same
 * DGROUP-relocation reason (see cstart_whet.asm / the neurketa-c README).
 *
 * The reported iteration count is WHET_PASSES (the number of full Whetstone
 * sweeps). The harness multiplies iters/sec by the per-pass FLOP weight to print
 * MFLOPS (see crates/izarravm/src/main.rs BENCHES). WHET_LOOP / WHET_PASSES are
 * sized so a 586 run is a few tens of ms of guest time, like dhrystone.
 */

/* ---- the report-and-halt path (provided by cstart_whet.asm) ---- */
extern void report(unsigned iters, unsigned aux);
#pragma aux report parm [ax] [dx] modify [];

/* ---- inline-8087 transcendentals: argument and result on the 8087 stack ---- */
extern double whet_sqrt(double x);
#pragma aux whet_sqrt = "fsqrt" parm [8087] value [8087];

extern double whet_sin(double x);
#pragma aux whet_sin = "fsin" parm [8087] value [8087];

extern double whet_cos(double x);
#pragma aux whet_cos = "fcos" parm [8087] value [8087];

/* atan(x) = atan(x / 1): FPATAN computes atan(ST1/ST0); load 1.0 over x. */
extern double whet_atan(double x);
#pragma aux whet_atan = \
    "fld1"   \
    "fpatan" \
    parm [8087] value [8087];

/* ln(x) = ln2 * log2(x) via FYL2X (= ST1 * log2(ST0)); need x in ST0, ln2 in ST1. */
extern double whet_log(double x);
#pragma aux whet_log = \
    "fldln2" \
    "fxch"   \
    "fyl2x"  \
    parm [8087] value [8087];

/* e^x = 2^(x * log2e): split into integer i and fraction f in [-.5,.5], take
 * 2^f-1 with F2XM1, recombine with FSCALE. */
extern double whet_exp(double x);
#pragma aux whet_exp = \
    "fldl2e"         \
    "fmulp st(1),st" \
    "fld st(0)"      \
    "frndint"        \
    "fxch"           \
    "fsub st(0),st(1)" \
    "f2xm1"          \
    "fld1"           \
    "faddp st(1),st" \
    "fscale"         \
    "fstp st(1)"     \
    parm [8087] value [8087];

/* ---- shared state (DGROUP globals), exactly as the original Whetstone ---- */
double E1[5];          /* E1[1..4] are used (the original is 1-based) */
double T, T1, T2;
int J, K, L;

static void PA(double E[])
{
    int j = 0;
    do {
        E[1] = ( E[1] + E[2] + E[3] - E[4]) * T;
        E[2] = ( E[1] + E[2] - E[3] + E[4]) * T;
        E[3] = ( E[1] - E[2] + E[3] + E[4]) * T;
        E[4] = (-E[1] + E[2] + E[3] + E[4]) / T2;
        j += 1;
    } while (j < 6);
}

static void P3(double X, double Y, double *Z)
{
    double X1, Y1;
    X1 = X;
    Y1 = Y;
    X1 = T * (X1 + Y1);
    Y1 = T * (X1 + Y1);
    *Z = (X1 + Y1) / T2;
}

static void P0(void)
{
    E1[J] = E1[K];
    E1[K] = E1[L];
    E1[L] = E1[J];
}

/* Per-module weight multiplier (the original default is 1000) and the number of
 * full sweeps to run. The standard module weights (N2..N11) keep the original FP
 * operation MIX at any LOOP; LOOP and PASSES are sized only for runtime - a few
 * tens of ms of 586 guest time, like dhrystone. MFLOPS is clock-counted (not
 * wall-timed), so it stays accurate even at this small work size. */
#define WHET_LOOP   1
#define WHET_PASSES 50

int whet_main(void)
{
    long N2, N3, N4, N6, N7, N8, N9, N11;
    long I;
    unsigned pass;
    double X, Y, Z;
    double s, c, cxy, cxmy;
    unsigned checksum;
    unsigned char *bp;
    int bi;

    T  = 0.499975;
    T1 = 0.50025;
    T2 = 2.0;

    N2  = 12L  * WHET_LOOP;
    N3  = 14L  * WHET_LOOP;
    N4  = 345L * WHET_LOOP;
    N6  = 210L * WHET_LOOP;
    N7  = 32L  * WHET_LOOP;
    N8  = 899L * WHET_LOOP;
    N9  = 616L * WHET_LOOP;
    N11 = 93L  * WHET_LOOP;

    for (pass = 0; pass < WHET_PASSES; pass++) {

        /* Module 2: array elements */
        E1[1] = 1.0; E1[2] = -1.0; E1[3] = -1.0; E1[4] = -1.0;
        for (I = 1; I <= N2; I++) {
            E1[1] = ( E1[1] + E1[2] + E1[3] - E1[4]) * T;
            E1[2] = ( E1[1] + E1[2] - E1[3] + E1[4]) * T;
            E1[3] = ( E1[1] - E1[2] + E1[3] + E1[4]) * T;
            E1[4] = (-E1[1] + E1[2] + E1[3] + E1[4]) * T;
        }

        /* Module 3: array as parameter */
        for (I = 1; I <= N3; I++)
            PA(E1);

        /* Module 4: conditional jumps (integer) */
        J = 1;
        for (I = 1; I <= N4; I++) {
            if (J == 1) J = 2; else J = 3;
            if (J > 2)  J = 0; else J = 1;
            if (J < 1)  J = 1; else J = 0;
        }

        /* Module 6: integer arithmetic with FP stores */
        J = 1; K = 2; L = 3;
        for (I = 1; I <= N6; I++) {
            J = J * (K - J) * (L - K);
            K = L * K - (L - J) * K;
            L = (L - K) * (K + J);
            E1[L - 1] = J + K + L;
            E1[K - 1] = J * K * L;
        }

        /* Module 7: trigonometric functions (temporaries keep the 8087 stack shallow) */
        X = 1.0; Y = 1.0;
        for (I = 1; I <= N7; I++) {
            s = whet_sin(X); c = whet_cos(X);
            cxy = whet_cos(X + Y); cxmy = whet_cos(X - Y);
            X = T * whet_atan(T2 * s * c / (cxy + cxmy - 1.0));
            s = whet_sin(Y); c = whet_cos(Y);
            cxy = whet_cos(X + Y); cxmy = whet_cos(X - Y);
            Y = T * whet_atan(T2 * s * c / (cxy + cxmy - 1.0));
        }

        /* Module 8: procedure calls */
        X = 1.0; Y = 1.0; Z = 1.0;
        for (I = 1; I <= N8; I++)
            P3(X, Y, &Z);

        /* Module 9: array references */
        J = 1; K = 2; L = 3;
        E1[1] = 1.0; E1[2] = 2.0; E1[3] = 3.0;
        for (I = 1; I <= N9; I++)
            P0();

        /* Module 11: standard functions */
        X = 0.75;
        for (I = 1; I <= N11; I++)
            X = whet_sqrt(whet_exp(whet_log(X) / T1));
    }

    /* Fold the final FP state into a 16-bit checksum: a wrong x87 result yields a
     * wrong checksum. The value is deterministic and identical across CPU modes
     * (only timing differs), so the harness pins it as the per-mode self-check. */
    checksum = 0;
    bp = (unsigned char *) &X;     for (bi = 0; bi < 8;  bi++) checksum += bp[bi];
    bp = (unsigned char *) &Y;     for (bi = 0; bi < 8;  bi++) checksum += bp[bi];
    bp = (unsigned char *) &Z;     for (bi = 0; bi < 8;  bi++) checksum += bp[bi];
    bp = (unsigned char *) &E1[1]; for (bi = 0; bi < 32; bi++) checksum += bp[bi];

    report((unsigned) WHET_PASSES, checksum);
    return 0;
}
