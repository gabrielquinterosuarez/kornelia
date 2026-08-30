//! El entorno de arranque UEFI (D24).
//!
//! Este crate es el unico que sabe que existe UEFI. No lleva `asm!` ni
//! `target_arch`: UEFI de 64 bits tiene exactamente la misma forma en x86_64 y
//! en aarch64, y por eso se escribe una sola vez.
//!
//! # Que es UEFI
//!
//! El programa que vive en un chip de flash del motherboard y corre antes que
//! cualquier sistema operativo. No es un cargador tonto: sabe leer discos,
//! tiene asignador de memoria y drivers basicos. Cuando arranca este kernel,
//! el kernel todavia no es dueno de la maquina: es una *aplicacion* del
//! firmware, que le presta el CPU.
//!
//! # La ventana (D25)
//!
//! Al firmware se le pide todo lo que se le va a pedir en la vida, y despues se
//! le suelta la maquina con `ExitBootServices`. Despues de esa llamada, invocar
//! un Boot Service es un crash. No hay segunda oportunidad.
//!
//! # Sobre las estructuras de aca abajo
//!
//! Estan transcriptas a mano de la especificacion de UEFI (regla 5: sin
//! dependencias externas). Un campo corrido no da error de compilacion, da
//! basura — asi que hay dos redes:
//!
//! 1. `const _: () = assert!(offset_of!(...))` verifica las posiciones **en
//!    tiempo de compilacion**. Si una transcripcion esta mal, no compila.
//! 2. Cada tabla trae una firma de 64 bits que se verifica **en tiempo de
//!    ejecucion** antes de usarla. Si el puntero no apunta a lo que creemos,
//!    se detecta ahi y no mas adelante con un salto a la nada.

#![no_std]

use core::ffi::c_void;
use core::mem::{offset_of, size_of};
use kernel_core::memoria::{Clase, Maquina, Region};

// ---------------------------------------------------------------------------
// Codigos de estado
// ---------------------------------------------------------------------------

/// EFI_STATUS: 0 es exito; los errores tienen prendido el bit mas alto.
type Status = usize;

const EXITO: Status = 0;
const BIT_ERROR: Status = 1 << (usize::BITS - 1);
const BUFFER_CHICO: Status = BIT_ERROR | 5;
const PARAMETRO_INVALIDO: Status = BIT_ERROR | 2;

// ---------------------------------------------------------------------------
// Estructuras de UEFI
// ---------------------------------------------------------------------------

/// Encabezado comun a todas las tablas de UEFI. 24 bytes.
#[repr(C)]
struct Encabezado {
    firma: u64,
    revision: u32,
    tamano_encabezado: u32,
    crc32: u32,
    reservado: u32,
}

/// "IBI SYST" en little-endian.
const FIRMA_SYSTEM_TABLE: u64 = 0x5453_5953_2049_4249;
/// "BOOTSERV" en little-endian.
const FIRMA_BOOT_SERVICES: u64 = 0x5652_4553_544f_4f42;

/// La raiz de todo lo que ofrece UEFI. Es el puntero que el firmware pasa como
/// segundo argumento de `efi_main`.
#[repr(C)]
pub struct SystemTable {
    hdr: Encabezado,
    fabricante: *const u16,
    revision_firmware: u32,
    // repr(C) mete aca los 4 bytes de relleno que exige la alineacion del
    // puntero que sigue. No hay que escribirlos a mano.
    manejador_consola_entrada: *mut c_void,
    consola_entrada: *mut c_void,
    manejador_consola_salida: *mut c_void,
    consola_salida: *mut c_void,
    manejador_consola_error: *mut c_void,
    consola_error: *mut c_void,
    /// Servicios que sobreviven a `ExitBootServices`: hora, variables, reset.
    servicios_runtime: *mut c_void,
    /// Servicios que mueren al soltar la maquina. Lo que nos interesa.
    servicios_boot: *mut BootServices,
    cantidad_tablas_config: usize,
    /// Lista de (GUID -> puntero). Aca viven ACPI y el device tree.
    tablas_config: *mut c_void,
}

/// Los Boot Services. Solo se nombran los que se usan; el resto son punteros
/// opacos que estan para que las posiciones den bien.
#[repr(C)]
struct BootServices {
    hdr: Encabezado,

    // Prioridad de tareas
    subir_tpl: *mut c_void,
    bajar_tpl: *mut c_void,

    // Memoria
    asignar_paginas: *mut c_void,
    liberar_paginas: *mut c_void,
    /// El que devuelve el mapa de memoria fisica.
    obtener_mapa_memoria: unsafe extern "efiapi" fn(
        tamano: *mut usize,
        mapa: *mut u8,
        llave: *mut usize,
        tamano_descriptor: *mut usize,
        version_descriptor: *mut u32,
    ) -> Status,
    asignar_pool: *mut c_void,
    liberar_pool: *mut c_void,

    // Eventos y timers
    crear_evento: *mut c_void,
    fijar_timer: *mut c_void,
    esperar_evento: *mut c_void,
    senalar_evento: *mut c_void,
    cerrar_evento: *mut c_void,
    revisar_evento: *mut c_void,

    // Protocolos
    instalar_protocolo: *mut c_void,
    reinstalar_protocolo: *mut c_void,
    desinstalar_protocolo: *mut c_void,
    manejar_protocolo: *mut c_void,
    reservado: *mut c_void,
    registrar_aviso_protocolo: *mut c_void,
    ubicar_manejador: *mut c_void,
    ubicar_ruta_dispositivo: *mut c_void,
    instalar_tabla_config: *mut c_void,

    // Imagenes
    cargar_imagen: *mut c_void,
    arrancar_imagen: *mut c_void,
    salir: *mut c_void,
    descargar_imagen: *mut c_void,
    /// El apreton de manos: despues de esto la maquina es nuestra.
    salir_de_boot_services:
        unsafe extern "efiapi" fn(imagen: *mut c_void, llave: usize) -> Status,
    // De aca en adelante hay mas servicios que todavia no se usan.
}

/// Un renglon del mapa de memoria. 40 bytes segun la especificacion.
#[repr(C)]
struct Descriptor {
    tipo: u32,
    // repr(C) mete 4 bytes de relleno aca.
    inicio_fisico: u64,
    inicio_virtual: u64,
    /// UEFI cuenta siempre en paginas de 4 KiB, en toda arquitectura.
    paginas: u64,
    atributos: u64,
}

/// UEFI define la pagina del mapa como 4 KiB, sin importar la arquitectura ni
/// el tamano de pagina que use la MMU.
const PAGINA_UEFI: u64 = 4096;

// --- Red 1: las posiciones se verifican al compilar -------------------------
// Si una transcripcion de arriba quedo corrida, esto no compila. Los numeros
// salen de la especificacion de UEFI para 64 bits.
const _: () = assert!(size_of::<Encabezado>() == 24);
const _: () = assert!(offset_of!(SystemTable, servicios_boot) == 96);
const _: () = assert!(offset_of!(SystemTable, tablas_config) == 112);
const _: () = assert!(size_of::<SystemTable>() == 120);
const _: () = assert!(offset_of!(BootServices, obtener_mapa_memoria) == 56);
const _: () = assert!(offset_of!(BootServices, salir_de_boot_services) == 232);
const _: () = assert!(offset_of!(Descriptor, inicio_fisico) == 8);
const _: () = assert!(offset_of!(Descriptor, paginas) == 24);
const _: () = assert!(size_of::<Descriptor>() == 40);

// ---------------------------------------------------------------------------
// Espacio para el mapa
// ---------------------------------------------------------------------------

// El mapa hay que pedirlo a un buffer, y no se puede usar el asignador del
// firmware para conseguirlo: asignar memoria *cambia el mapa* y con eso invalida
// la llave que `ExitBootServices` exige. Un arreglo estatico esquiva el problema
// entero — no asigna nada, asi que el mapa no se mueve entre que se pide y que
// se sale.

/// 32 KiB. Con descriptores de 40 bytes son mas de 800 regiones; QEMU reporta
/// decenas y una maquina real, cientos.
const TAM_BUFFER: usize = 32 * 1024;
static mut BUFFER: [u8; TAM_BUFFER] = [0; TAM_BUFFER];

const MAX_REGIONES: usize = 512;
static mut REGIONES: [Region; MAX_REGIONES] = [Region::vacia(); MAX_REGIONES];

/// `ExitBootServices` se llama una sola vez en la vida de la maquina (D25).
static mut YA_SE_TOMO: bool = false;

// ---------------------------------------------------------------------------
// La ventana
// ---------------------------------------------------------------------------

/// Le saca al firmware todo lo que se le va a pedir y le suelta la maquina.
///
/// Al volver de aca, los Boot Services ya no existen y el kernel es dueno del
/// hardware. Nunca falla de forma fatal: si algo sale mal devuelve una `Maquina`
/// muda con el motivo, porque el cordon umbilical sigue vivo y hay que poder
/// contar que paso (P5).
///
/// # Safety
///
/// `imagen` y `systab` tienen que ser los que el firmware paso a `efi_main`.
pub unsafe fn tomar_la_maquina(imagen: *mut c_void, systab: *mut SystemTable) -> Maquina {
    if YA_SE_TOMO {
        return Maquina::muda("ExitBootServices ya se llamo una vez (D25)");
    }
    if systab.is_null() {
        return Maquina::muda("el firmware paso un System Table nulo");
    }

    // --- Red 2: verificar que los punteros son lo que decimos que son -------
    if (*systab).hdr.firma != FIRMA_SYSTEM_TABLE {
        return Maquina::muda("el System Table no tiene la firma 'IBI SYST'");
    }
    let bs = (*systab).servicios_boot;
    if bs.is_null() {
        return Maquina::muda("el System Table no trae Boot Services");
    }
    if (*bs).hdr.firma != FIRMA_BOOT_SERVICES {
        return Maquina::muda("los Boot Services no tienen la firma 'BOOTSERV'");
    }

    let buffer = &raw mut BUFFER as *mut u8;
    // Sin valor inicial: el unico camino que sale del bucle de abajo es el que
    // los deja escritos, y asi lo verifica el compilador en vez de nosotros.
    let tamano_usado: usize;
    let tamano_descriptor: usize;

    // El firmware puede cambiar el mapa entre que lo pedimos y que salimos (una
    // interrupcion suya que asigne memoria alcanza). En ese caso ExitBootServices
    // devuelve PARAMETRO_INVALIDO y la especificacion manda volver a pedir el
    // mapa y reintentar. Tres vueltas y si no, se avisa.
    let mut intento = 0;
    loop {
        intento += 1;
        if intento > 3 {
            return Maquina::muda("el mapa de memoria cambio tres veces seguidas");
        }

        let mut tamano = TAM_BUFFER;
        let mut llave: usize = 0;
        let mut paso: usize = 0;
        let mut version: u32 = 0;

        let st = ((*bs).obtener_mapa_memoria)(
            &mut tamano,
            buffer,
            &mut llave,
            &mut paso,
            &mut version,
        );

        if st == BUFFER_CHICO {
            return Maquina::muda("el mapa de memoria no entra en 32 KiB");
        }
        if st != EXITO {
            return Maquina::muda("GetMemoryMap fallo");
        }
        if paso < size_of::<Descriptor>() {
            return Maquina::muda("el firmware reporto un descriptor imposible");
        }

        // Sin nada entremedio: la llave tiene que seguir fresca.
        let st = ((*bs).salir_de_boot_services)(imagen, llave);
        if st == EXITO {
            // Unico camino que sale del bucle, y por eso el unico que escribe.
            tamano_usado = tamano;
            tamano_descriptor = paso;
            break;
        }
        if st != PARAMETRO_INVALIDO {
            return Maquina::muda("ExitBootServices fallo");
        }
    }

    YA_SE_TOMO = true;

    // Desde aca no se puede llamar a ningun Boot Service. El buffer es memoria
    // nuestra, asi que interpretarlo ahora es seguro.
    let cantidad = interpretar(buffer, tamano_usado, tamano_descriptor);

    Maquina {
        regiones: core::slice::from_raw_parts(&raw const REGIONES as *const Region, cantidad),
        fallo: None,
    }
}

/// Traduce los descriptores de UEFI al vocabulario del nucleo.
///
/// # Safety
///
/// `buffer` tiene que traer `tamano` bytes validos de descriptores.
unsafe fn interpretar(buffer: *const u8, tamano: usize, paso: usize) -> usize {
    let destino = &raw mut REGIONES as *mut Region;
    let mut n = 0;
    let mut off = 0;

    // Se avanza con el `paso` que informo el firmware y NO con el tamano de la
    // estructura: la especificacion permite descriptores mas grandes que los 40
    // bytes de hoy, para poder crecer sin romper a nadie. Asumir 40 es un bug
    // clasico que aparece recien en el firmware que decide crecer.
    while off + paso <= tamano && n < MAX_REGIONES {
        let d = buffer.add(off) as *const Descriptor;

        let region = Region {
            inicio: (*d).inicio_fisico,
            bytes: (*d).paginas.saturating_mul(PAGINA_UEFI),
            clase: clasificar((*d).tipo),
        };

        if region.bytes > 0 {
            destino.add(n).write(region);
            n += 1;
        }
        off += paso;
    }
    n
}

/// Los quince tipos de memoria de UEFI, traducidos a lo que le importa a quien
/// va a reclamar memoria.
fn clasificar(tipo: u32) -> Clase {
    match tipo {
        0 => Clase::Reservada,

        // 1 y 2 son LoaderCode y LoaderData: somos nosotros, el kernel que el
        // firmware cargo.
        1 | 2 => Clase::Kernel,

        // 3 y 4 son BootServicesCode y BootServicesData. Son libres *porque ya
        // salimos*: mientras los Boot Services vivian, era su codigo. Si esta
        // funcion se llamara antes de ExitBootServices, esto seria mentira.
        //
        // OJO (deuda anotada en DISENO.md): la pila sobre la que corre este
        // mismo codigo suele salir de aca. Marcarla libre esta bien para
        // *informar* el mapa, pero `mem.claim` no puede entregarla sin que el
        // kernel se haya mudado antes a una pila propia.
        3 | 4 => Clase::Libre,

        // 5 y 6 son RuntimeServicesCode y Data: el firmware los sigue usando
        // despues de soltarnos la maquina.
        5 | 6 => Clase::Firmware,

        7 => Clase::Libre,   // ConventionalMemory: la RAM de verdad
        8 => Clase::Rota,    // el firmware la reporta defectuosa
        9 => Clase::TablasAcpi,
        10 => Clase::FirmwareNvs,
        11 | 12 => Clase::Mmio, // MemoryMappedIO y IOPortSpace
        13 => Clase::Firmware,  // PalCode
        14 => Clase::Persistente,

        otro => Clase::Otra(otro),
    }
}
