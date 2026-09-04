/* A que ritmo sube el contador de ciclos del procesador.
 *
 * Es la misma medicion que hace el kernel de x86_64 de Kornelia cuando la
 * maquina no informa la frecuencia: contar cuanto sube el TSC mientras un
 * reloj de frecuencia conocida avanza un rato. La diferencia es contra que se
 * mide -- aca CLOCK_MONOTONIC, que se lo pedimos al kernel; alla el contador
 * de frecuencia fija que informa ACPI, porque no hay kernel abajo a quien
 * preguntarle.
 *
 *   cc -O2 -o tsc tsc.c && ./tsc
 */
#include <stdio.h>
#include <time.h>
#include <x86intrin.h>

static double monotonic_seconds(void)
{
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec / 1e9;
}

/* `lfence` antes de `rdtsc`, igual que en el kernel: sin eso el procesador
 * puede adelantar la lectura del contador, porque ejecuta fuera de orden, y
 * dos medidas seguidas pueden salir al reves. */
static unsigned long long ticks(void)
{
    __builtin_ia32_lfence();
    return __rdtsc();
}

int main(void)
{
    struct timespec pause = { 0, 300 * 1000 * 1000 }; /* 300 ms */

    double t0 = monotonic_seconds();
    unsigned long long c0 = ticks();

    nanosleep(&pause, NULL);

    unsigned long long c1 = ticks();
    double t1 = monotonic_seconds();

    double seconds = t1 - t0;
    unsigned long long elapsed = c1 - c0;

    printf("ciclos contados : %llu\n", elapsed);
    printf("segundos reales : %.6f\n", seconds);
    printf("frecuencia      : %.3f GHz\n", elapsed / seconds / 1e9);
    printf("un ciclo dura   : %.3f ns\n", seconds / elapsed * 1e9);
    printf("en un ciclo la senal recorre unos %.1f cm de cobre\n",
           seconds / elapsed * 1e9 * 20.0);
    return 0;
}
