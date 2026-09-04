/* Cuanto cuesta traer un dato, segun donde este.
 *
 * Recorre una lista enlazada **desordenada** que vive dentro de un bloque de
 * tamano creciente. Desordenada a proposito: si los accesos fueran
 * secuenciales, el prefetcher del procesador traeria la linea siguiente antes
 * de que la pidamos y estariamos midiendo el prefetcher, no la memoria.
 *
 * Cada salto depende del anterior, asi que el procesador tampoco puede
 * adelantarse: es una cadena, no un paquete de accesos independientes. Eso es
 * lo que hace que el numero que sale sea la latencia de verdad.
 *
 * Cuando el bloque cabe en L1 el paso cuesta unos pocos ciclos; cuando pasa el
 * tamano de la L3, cuesta cien veces mas. El salto en la tabla es donde esta
 * el limite de cada cache: se pueden leer los tamanos de las caches de tu
 * maquina sin preguntarselos a nadie.
 *
 *   cc -O2 -o latencias latencias.c && ./latencias 3.6
 *                                                  ^ tus GHz, para la ultima
 *                                                    columna (opcional)
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#define LINE 64           /* el paso: una linea de cache */
#define STEPS (20u << 20) /* saltos por medicion */

static double monotonic_ns(void)
{
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec * 1e9 + t.tv_nsec;
}

int main(int argc, char **argv)
{
    double ghz = argc > 1 ? atof(argv[1]) : 0.0;

    srand(1); /* siempre la misma permutacion: la medida tiene que repetirse */

    printf("%10s  %10s  %s\n", "tamano", "ns/acceso", ghz > 0 ? "ciclos" : "");
    for (size_t bytes = 4u << 10; bytes <= 128u << 20; bytes <<= 1) {
        size_t count = bytes / LINE;
        size_t *order = malloc(count * sizeof *order);
        char *block = aligned_alloc(4096, bytes);
        if (!order || !block) {
            fprintf(stderr, "sin memoria para %zu bytes\n", bytes);
            return 1;
        }
        memset(block, 0, bytes);

        for (size_t i = 0; i < count; i++)
            order[i] = i;
        for (size_t i = count - 1; i > 0; i--) { /* Fisher-Yates */
            size_t j = (size_t)rand() % (i + 1);
            size_t tmp = order[i];
            order[i] = order[j];
            order[j] = tmp;
        }
        /* Cada linea guarda el offset de la siguiente: la cadena. */
        for (size_t i = 0; i < count; i++)
            *(size_t *)(block + order[i] * LINE) = order[(i + 1) % count] * LINE;

        volatile size_t sink;
        size_t offset = 0;
        double start = monotonic_ns();
        for (size_t s = 0; s < STEPS; s++)
            offset = *(size_t *)(block + offset);
        double end = monotonic_ns();
        sink = offset; /* que el compilador no borre el bucle */
        (void)sink;

        double per = (end - start) / STEPS;
        if (ghz > 0)
            printf("%7zu KiB  %10.2f  %6.0f\n", bytes >> 10, per, per * ghz);
        else
            printf("%7zu KiB  %10.2f\n", bytes >> 10, per);

        free(order);
        free(block);
    }
    return 0;
}
