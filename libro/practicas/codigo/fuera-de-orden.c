/* La misma cantidad de sumas, encadenadas o independientes.
 *
 * Las sumas van en ensamblador en linea para que el compilador no las pueda
 * colapsar: `add $1, %0` con "+r" obliga a que el valor este en un registro y
 * a que la suma ocurra de verdad. Con C normal, `for (i) a = a + 1` se
 * convierte en `a += N` y no se mide nada.
 */
#include <stdio.h>
#include <stdint.h>
#include <time.h>

#define N 1000000000ULL
#define SUMA(x) __asm__ volatile("add $1, %0" : "+r"(x))

static double ns(void) {
    struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec * 1e9 + t.tv_nsec;
}

int main(void) {
    volatile uint64_t sink;

    /* UNA cadena: cada suma necesita el resultado de la anterior. */
    uint64_t a = 1;
    double t0 = ns();
    for (uint64_t i = 0; i < N; i++) SUMA(a);
    double t1 = ns();
    sink = a;

    /* OCHO cadenas independientes: la misma cantidad de sumas. */
    uint64_t b0=1,b1=1,b2=1,b3=1,b4=1,b5=1,b6=1,b7=1;
    double t2 = ns();
    for (uint64_t i = 0; i < N/8; i++) {
        SUMA(b0); SUMA(b1); SUMA(b2); SUMA(b3);
        SUMA(b4); SUMA(b5); SUMA(b6); SUMA(b7);
    }
    double t3 = ns();
    sink = b0^b1^b2^b3^b4^b5^b6^b7; (void)sink;

    double dep = (t1 - t0) / N;      /* ns por suma, encadenadas */
    double ind = (t3 - t2) / N;      /* ns por suma, independientes */

    /* Una suma dependiente de la anterior tarda EXACTAMENTE un ciclo: esa es
     * la latencia del `add`. Asi que la cadena sirve de frecuencimetro, y con
     * esa frecuencia se puede convertir la otra medicion a ciclos. */
    double ghz = 1.0 / dep;

    printf("             %10s %12s\n", "ns/suma", "sumas/ciclo");
    printf("1 cadena     %10.3f %12.2f\n", dep, 1.0);
    printf("8 cadenas    %10.3f %12.2f\n", ind, dep / ind);
    printf("\nmismas %llu sumas, %.1fx de diferencia\n", N, dep / ind);
    printf("frecuencia deducida de la cadena: %.2f GHz\n", ghz);
    return 0;
}
