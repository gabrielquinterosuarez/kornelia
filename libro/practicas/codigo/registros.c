/* Donde se acaban los registros, y por que.
 *
 * Tres funciones, cada una para ver una cosa distinta en el desensamblado:
 *
 *   1. `sin_direccion` / `con_direccion` — que pasa cuando le pedis la
 *      direccion a un valor. Un registro no tiene direccion, asi que el
 *      compilador esta obligado a bajarlo a la pila.
 *   2. `doce_argumentos` — donde se le acaban los registros a la convencion de
 *      llamada. Los primeros seis vienen por registro; el resto, de la pila.
 *   3. `muchos_vivos` — que hace el compilador cuando hay mas valores vivos a
 *      la vez que registros: los derrama (*spill*).
 *
 *   cc -O2 -c -o registros.o registros.c && objdump -d registros.o
 */
#include <stdint.h>

void usar(uint64_t *p);

/* 1a. Nada toca la pila: todo vive en registros. */
uint64_t sin_direccion(uint64_t x)
{
    uint64_t y = x * 3;
    return y + 1;
}

/* 1b. El mismo calculo, pero alguien pide la direccion de `y`. */
uint64_t con_direccion(uint64_t x)
{
    uint64_t y = x * 3;
    usar(&y);
    return y + 1;
}

/* 2. Doce argumentos. La convencion de System V pasa seis por registro. */
uint64_t doce_argumentos(uint64_t a, uint64_t b, uint64_t c, uint64_t d,
                         uint64_t e, uint64_t f, uint64_t g, uint64_t h,
                         uint64_t i, uint64_t j, uint64_t k, uint64_t l)
{
    return a ^ b ^ c ^ d ^ e ^ f ^ g ^ h ^ i ^ j ^ k ^ l;
}

/* 3. Veinte valores vivos al mismo tiempo, mezclados para que ninguno se
 *    pueda descartar. Hay 16 registros y dos ya estan ocupados. */
uint64_t muchos_vivos(uint64_t s)
{
    uint64_t v00 = s + 1,  v01 = s + 2,  v02 = s + 3,  v03 = s + 5;
    uint64_t v04 = s + 7,  v05 = s + 11, v06 = s + 13, v07 = s + 17;
    uint64_t v08 = s + 19, v09 = s + 23, v10 = s + 29, v11 = s + 31;
    uint64_t v12 = s + 37, v13 = s + 41, v14 = s + 43, v15 = s + 47;
    uint64_t v16 = s + 53, v17 = s + 59, v18 = s + 61, v19 = s + 67;

    /* Cada uno se combina con otro, asi que todos siguen vivos hasta el final. */
    v00 *= v19; v01 *= v18; v02 *= v17; v03 *= v16; v04 *= v15;
    v05 *= v14; v06 *= v13; v07 *= v12; v08 *= v11; v09 *= v10;

    return v00 ^ v01 ^ v02 ^ v03 ^ v04 ^ v05 ^ v06 ^ v07 ^ v08 ^ v09
         ^ v10 ^ v11 ^ v12 ^ v13 ^ v14 ^ v15 ^ v16 ^ v17 ^ v18 ^ v19;
}
