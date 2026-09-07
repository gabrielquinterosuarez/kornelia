/* Como se arma `blob.bin`.
 *
 * El kernel copia estos bytes a memoria que reclamo y **salta al byte cero**,
 * asi que la entrada tiene que ser lo primero de todo. Eso no se puede dejar
 * librado al orden en que el compilador emita las funciones: la entrada vive en
 * su propia seccion y aca se la pone adelante.
 *
 * Y todo se arma como si empezara en cero, porque la direccion de verdad recien
 * se sabe en tiempo de ejecucion — el kernel reclama la memoria en el arranque.
 * De ahi que el codigo tenga que ser independiente de donde lo carguen.
 */
ENTRY(blob_entry)

SECTIONS
{
    . = 0;

    .text : {
        *(.text.entry)
        *(.text .text.*)
    }

    .rodata : {
        *(.rodata .rodata.*)
    }

    .data : {
        *(.data .data.*)
    }

    /* No puede haber `.bss`. Un binario plano no la trae —son ceros que nadie
     * escribe— asi que una variable que viviera ahi arrancaria con basura. Si
     * esto se dispara, la causa es un `static` mutable: sacalo. */
    .bss (NOLOAD) : {
        *(.bss .bss.*)
        *(COMMON)
    }
    ASSERT(SIZEOF(.bss) == 0, "el blob no puede tener .bss: ver blob.ld")

    /* El blob no puede tener GOT, y esto es lo que lo hace fallar al compilar
     * en vez de fallar en la maquina.
     *
     * La GOT es una tabla de direcciones que normalmente rellena el enlazador
     * dinamico al cargar el programa. Aca no hay ninguno: el kernel copia los
     * bytes y salta. Asi que las entradas quedan con la direccion de tiempo de
     * enlace —basada en cero, no en donde lo cargaron— y el blob salta a
     * cualquier lado. Compilado independiente de la posicion quedan en cero
     * directamente, y salta a la direccion cero.
     *
     * Aparece cuando el codigo llama a un simbolo GLOBAL. En la practica eso
     * quiere decir `memset`/`memcpy`, que el compilador inventa solo cuando ve
     * una copia o una inicializacion grande. Por eso el blob las evita: los
     * buffers no se ponen en cero y las copias se hacen con bucles propios.
     *
     * Arreglarlo de verdad seria que el blob relocalice sus propias entradas al
     * arrancar, como hace un cargador. No alcanza con escribirlo en Rust —
     * llegar a las puntas de la tabla tambien pasa por la GOT, que es
     * justamente lo que todavia no esta armado— asi que necesita un arranque en
     * ensamblador. Cuando haga falta, ese es el camino. */
    .got : { *(.got) *(.got.plt) *(.plt) *(.plt.*) }
    ASSERT(SIZEOF(.got) == 0, "el blob no puede tener GOT: ver el comentario de arriba")

    /* Nada de esto va en un blob. Aparecen porque el codigo se compila
     * independiente de la posicion, y el enlazador da por hecho que eso quiere
     * decir "esto lo va a cargar un enlazador dinamico" — que aca no existe:
     * lo carga el kernel, que copia y salta. Si no se descartan, se cuelan
     * ADELANTE del codigo y la entrada deja de estar en el byte cero. */
    /DISCARD/ : {
        *(.dynsym) *(.dynstr) *(.hash) *(.gnu.hash)
        *(.dynamic) *(.interp) *(.gnu.version*)
        *(.eh_frame*)
        *(.comment)
        *(.note*)
    }
}
