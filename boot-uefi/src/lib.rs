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
use kernel_core::machine::{Blob, Machine, Tables};
use kernel_core::memory::{Caching, Kind, Region};

// ---------------------------------------------------------------------------
// Codigos de estado
// ---------------------------------------------------------------------------

/// EFI_STATUS: 0 es exito; los errores tienen prendido el bit mas alto.
type Status = usize;

const SUCCESS: Status = 0;
const ERROR_BIT: Status = 1 << (usize::BITS - 1);
const BUFFER_TOO_SMALL: Status = ERROR_BIT | 5;
const INVALID_PARAMETER: Status = ERROR_BIT | 2;

// ---------------------------------------------------------------------------
// Estructuras de UEFI
// ---------------------------------------------------------------------------

/// Encabezado comun a todas las tablas de UEFI. 24 bytes.
#[repr(C)]
struct TableHeader {
    signature: u64,
    revision: u32,
    header_size: u32,
    crc32: u32,
    reserved: u32,
}

/// "IBI SYST" en little-endian.
const SYSTEM_TABLE_SIGNATURE: u64 = 0x5453_5953_2049_4249;
/// "BOOTSERV" en little-endian.
const BOOT_SERVICES_SIGNATURE: u64 = 0x5652_4553_544f_4f42;

/// La raiz de todo lo que ofrece UEFI. Es el puntero que el firmware pasa como
/// segundo argumento de `efi_main`.
#[repr(C)]
pub struct SystemTable {
    hdr: TableHeader,
    vendor: *const u16,
    firmware_revision: u32,
    // repr(C) mete aca los 4 bytes de relleno que exige la alineacion del
    // puntero que sigue. No hay que escribirlos a mano.
    console_in_handle: *mut c_void,
    con_in: *mut c_void,
    console_out_handle: *mut c_void,
    con_out: *mut c_void,
    stderr_handle: *mut c_void,
    stderr: *mut c_void,
    /// Servicios que sobreviven a `ExitBootServices`: hora, variables, reset.
    runtime_services: *mut c_void,
    /// Servicios que mueren al soltar la maquina. Lo que nos interesa.
    boot_services: *mut BootServices,
    config_table_len: usize,
    /// Lista de (GUID -> puntero). Aca viven ACPI y el device tree.
    config_table: *mut c_void,
}

/// Los Boot Services. Solo se nombran los que se usan; el resto son punteros
/// opacos que estan para que las posiciones den bien.
#[repr(C)]
struct BootServices {
    hdr: TableHeader,

    // Prioridad de tareas
    raise_tpl: *mut c_void,
    restore_tpl: *mut c_void,

    // Memoria
    allocate_pages: *mut c_void,
    free_pages: *mut c_void,
    /// El que devuelve el mapa de memoria fisica.
    get_memory_map: unsafe extern "efiapi" fn(
        size: *mut usize,
        map: *mut u8,
        key: *mut usize,
        descriptor_size: *mut usize,
        descriptor_version: *mut u32,
    ) -> Status,
    allocate_pool: *mut c_void,
    free_pool: *mut c_void,

    // Eventos y timers
    create_event: *mut c_void,
    set_timer: *mut c_void,
    wait_for_event: *mut c_void,
    signal_event: *mut c_void,
    close_event: *mut c_void,
    check_event: *mut c_void,

    // Protocolos
    install_protocol_interface: *mut c_void,
    reinstall_protocol_interface: *mut c_void,
    uninstall_protocol_interface: *mut c_void,
    /// Pregunta si un handle ofrece un protocolo, y devuelve su tabla de
    /// funciones. Es como se llega al sistema de archivos: **lo unico que sabe
    /// leer FAT32 es el firmware** (D25).
    handle_protocol: unsafe extern "efiapi" fn(
        handle: *mut c_void,
        guid: *const Guid,
        interface: *mut *mut c_void,
    ) -> Status,
    reserved: *mut c_void,
    register_protocol_notify: *mut c_void,
    locate_handle: *mut c_void,
    locate_device_path: *mut c_void,
    install_configuration_table: *mut c_void,

    // Imagenes
    load_image: *mut c_void,
    start_image: *mut c_void,
    exit: *mut c_void,
    unload_image: *mut c_void,
    /// El apreton de manos: despues de esto la maquina es nuestra.
    exit_boot_services: unsafe extern "efiapi" fn(image: *mut c_void, key: usize) -> Status,
    // De aca en adelante hay mas servicios que todavia no se usan.
}

/// Un identificador de 128 bits. UEFI los usa para ofrecer cosas opcionales sin
/// que el que pregunta tenga que saber de antemano cuales hay: se recorre la
/// lista comparando GUIDs y se usa lo que aparezca.
///
/// Los primeros tres campos son numeros (y por lo tanto little-endian en
/// memoria); los ultimos ocho son bytes sueltos. Por eso un GUID escrito como
/// texto no se lee igual que sus bytes en memoria, y aca se transcribe campo
/// por campo en vez de como un arreglo.
#[repr(C)]
#[derive(PartialEq, Eq)]
struct Guid(u32, u16, u16, [u8; 8]);

/// Un renglon de la Configuration Table: que es, y donde esta.
#[repr(C)]
struct ConfigEntry {
    guid: Guid,
    table: *mut c_void,
}

/// Tablas de ACPI 2.0 en adelante. El puntero es al RSDP.
const ACPI_20: Guid = Guid(
    0x8868_e871,
    0xe4f1,
    0x11d3,
    [0xbc, 0x22, 0x00, 0x80, 0xc7, 0x3c, 0x88, 0x81],
);
/// Tablas de ACPI 1.0. Se usa solo si no esta la de 2.0.
const ACPI_10: Guid = Guid(
    0xeb9d_2d30,
    0x2d88,
    0x11d3,
    [0x9a, 0x16, 0x00, 0x90, 0x27, 0x3f, 0xc1, 0x4d],
);
/// Device tree aplanado.
const DTB: Guid = Guid(
    0xb1b6_21d5,
    0xf19c,
    0x41a5,
    [0x83, 0x0b, 0xd9, 0x15, 0x2c, 0x69, 0xaa, 0xe0],
);
/// SMBIOS 3.x (entrada de 64 bits).
const SMBIOS3: Guid = Guid(
    0xf2fd_1544,
    0x9794,
    0x4a2c,
    [0x99, 0x2e, 0xe5, 0xbb, 0xcf, 0x20, 0xe3, 0x94],
);
/// SMBIOS clasico. Se usa solo si no esta el 3.x.
const SMBIOS: Guid = Guid(
    0xeb9d_2d31,
    0x2d88,
    0x11d3,
    [0x9a, 0x16, 0x00, 0x90, 0x27, 0x3f, 0xc1, 0x4d],
);

/// Un renglon del mapa de memoria. 40 bytes segun la especificacion.
#[repr(C)]
struct Descriptor {
    /// El tipo de memoria segun UEFI. En la especificacion se llama `Type`,
    /// que en Rust es palabra reservada.
    kind: u32,
    // repr(C) mete 4 bytes de relleno aca.
    physical_start: u64,
    virtual_start: u64,
    /// UEFI cuenta siempre en paginas de 4 KiB, en toda arquitectura.
    pages: u64,
    attributes: u64,
}

/// UEFI define la pagina del mapa como 4 KiB, sin importar la arquitectura ni
/// el tamano de pagina que use la MMU.
const UEFI_PAGE: u64 = 4096;

// --- Red 1: las posiciones se verifican al compilar -------------------------
// Si una transcripcion de arriba quedo corrida, esto no compila. Los numeros
// salen de la especificacion de UEFI para 64 bits.
const _: () = assert!(size_of::<TableHeader>() == 24);
const _: () = assert!(offset_of!(SystemTable, boot_services) == 96);
const _: () = assert!(offset_of!(SystemTable, config_table) == 112);
const _: () = assert!(size_of::<SystemTable>() == 120);
const _: () = assert!(offset_of!(BootServices, get_memory_map) == 56);
const _: () = assert!(offset_of!(BootServices, exit_boot_services) == 232);
const _: () = assert!(offset_of!(Descriptor, physical_start) == 8);
const _: () = assert!(offset_of!(Descriptor, pages) == 24);
const _: () = assert!(size_of::<Descriptor>() == 40);
const _: () = assert!(size_of::<Guid>() == 16);
const _: () = assert!(offset_of!(ConfigEntry, table) == 16);
const _: () = assert!(size_of::<ConfigEntry>() == 24);
const _: () = assert!(offset_of!(BootServices, handle_protocol) == 152);
const _: () = assert!(offset_of!(LoadedImage, device_handle) == 24);
const _: () = assert!(offset_of!(FileSystem, open_volume) == 8);
const _: () = assert!(offset_of!(File, read) == 32);

// ---------------------------------------------------------------------------
// El sistema de archivos, para traer el blob (D18, D19)
// ---------------------------------------------------------------------------
//
// El kernel sigue con cero drivers: el que lee el disco es el firmware, que ya
// existe y corre antes que nosotros. Por eso el blob se carga **dentro de la
// ventana** (D25) — despues de `ExitBootServices` no queda nadie que sepa leer
// FAT32.

/// `EFI_LOADED_IMAGE_PROTOCOL`. Lo unico que interesa es de que dispositivo nos
/// cargaron: el blob vive al lado del kernel, asi que se busca ahi mismo.
#[repr(C)]
struct LoadedImage {
    revision: u32,
    // repr(C) mete aca los 4 bytes de relleno del puntero que sigue.
    parent_handle: *mut c_void,
    system_table: *mut c_void,
    /// El volumen del que salio esta imagen.
    device_handle: *mut c_void,
    // Y despues hay mas campos que no se usan.
}

const LOADED_IMAGE: Guid = Guid(
    0x5b1b_31a1,
    0x9562,
    0x11d2,
    [0x8e, 0x3f, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b],
);

/// `EFI_SIMPLE_FILE_SYSTEM_PROTOCOL`: abre el volumen y da su directorio raiz.
#[repr(C)]
struct FileSystem {
    revision: u64,
    open_volume: unsafe extern "efiapi" fn(
        this: *mut FileSystem,
        root: *mut *mut File,
    ) -> Status,
}

const SIMPLE_FILE_SYSTEM: Guid = Guid(
    0x964e_5b22,
    0x6459,
    0x11d2,
    [0x8e, 0x39, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b],
);

/// `EFI_FILE_PROTOCOL`. Los que no se usan quedan como punteros opacos para que
/// las posiciones den bien — la misma red que el resto del archivo.
#[repr(C)]
struct File {
    revision: u64,
    /// El nombre va en UTF-16 terminado en cero, que es como UEFI habla.
    open: unsafe extern "efiapi" fn(
        this: *mut File,
        new_handle: *mut *mut File,
        name: *const u16,
        mode: u64,
        attributes: u64,
    ) -> Status,
    close: unsafe extern "efiapi" fn(this: *mut File) -> Status,
    delete: *mut c_void,
    /// Al volver, `size` dice **cuantos bytes se leyeron**. Es lo que se usa
    /// para saber el tamano sin transcribir `EFI_FILE_INFO` y su GUID.
    read: unsafe extern "efiapi" fn(
        this: *mut File,
        size: *mut usize,
        buffer: *mut u8,
    ) -> Status,
    // De aca en adelante: write, get_position, set_position, get_info...
}

/// Abrir solo para leer.
const FILE_MODE_READ: u64 = 1;

/// Como se llama el blob, en UTF-16 y terminado en cero (D21: los nombres son
/// los terminos tecnicos, sin marca).
const BLOB_NAME: [u16; 9] = [
    b'b' as u16,
    b'l' as u16,
    b'o' as u16,
    b'b' as u16,
    b'.' as u16,
    b'b' as u16,
    b'i' as u16,
    b'n' as u16,
    0,
];

/// Lo mas grande que puede ser el blob.
///
/// Es un arreglo estatico y no memoria pedida al firmware, por dos razones que
/// van juntas: pedir memoria **mueve el mapa** y con eso invalida la llave que
/// `ExitBootServices` exige (D25), y un estatico vive adentro de la imagen, que
/// el mapa informa como memoria del kernel — asi que `mem.claim` no se lo puede
/// entregar al agente por accidente.
///
/// 256 KiB alcanzan de sobra para lo que D19 pide: un cargador de unos KB que
/// sabe leer el resto desde el disco. Si el archivo no entra, se dice y no se
/// ejecuta nada: medio cargador es peor que ninguno.
const MAX_BLOB: usize = 256 * 1024;
static mut BLOB: [u8; MAX_BLOB] = [0; MAX_BLOB];

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
const BUFFER_SIZE: usize = 32 * 1024;
static mut BUFFER: [u8; BUFFER_SIZE] = [0; BUFFER_SIZE];

const MAX_REGIONS: usize = 512;
static mut REGIONS: [Region; MAX_REGIONS] = [Region::empty(); MAX_REGIONS];

/// `ExitBootServices` se llama una sola vez en la vida de la maquina (D25).
static mut ALREADY_TAKEN: bool = false;

// ---------------------------------------------------------------------------
// La ventana
// ---------------------------------------------------------------------------

/// Le saca al firmware todo lo que se le va a pedir y le suelta la maquina.
///
/// Al volver de aca, los Boot Services ya no existen y el kernel es dueno del
/// hardware. Nunca falla de forma fatal: si algo sale mal devuelve una `Machine`
/// muda con el motivo, porque el cordon umbilical sigue vivo y hay que poder
/// contar que paso (P5).
///
/// # Safety
///
/// `image` y `systab` tienen que ser los que el firmware paso a `efi_main`.
pub unsafe fn take_machine(image: *mut c_void, systab: *mut SystemTable) -> Machine {
    if ALREADY_TAKEN {
        return Machine::mute("ExitBootServices ya se llamo una vez (D25)");
    }
    if systab.is_null() {
        return Machine::mute("the firmware passed a null System Table");
    }

    // --- Red 2: verificar que los punteros son lo que decimos que son -------
    if (*systab).hdr.signature != SYSTEM_TABLE_SIGNATURE {
        return Machine::mute("the System Table lacks the 'IBI SYST' signature");
    }
    let bs = (*systab).boot_services;
    if bs.is_null() {
        return Machine::mute("el System Table no trae Boot Services");
    }
    if (*bs).hdr.signature != BOOT_SERVICES_SIGNATURE {
        return Machine::mute("the Boot Services lack the 'BOOTSERV' signature");
    }

    // Primero las tablas: es solo recorrer una lista que ya existe, no asigna
    // nada y por lo tanto no mueve el mapa de memoria. Tiene que pasar dentro
    // de la ventana igual (D25), porque despues de salir no hay como volver.
    let tables = find_tables(systab);

    // Y el blob, **antes de pedir el mapa** (D18, D25). Abrir un archivo puede
    // mover la memoria, y la llave que `ExitBootServices` exige tiene que ser la
    // del mapa mas reciente: si el blob se cargara despues, la llave quedaria
    // vieja y la salida fallaria.
    let blob = load_blob(image, bs);

    let buffer = &raw mut BUFFER as *mut u8;

    // Sin valor inicial: el unico camino que sale del bucle de abajo es el que
    // los deja escritos, y asi lo verifica el compilador en vez de nosotros.
    let used_size: usize;
    let descriptor_size: usize;

    // El firmware puede cambiar el mapa entre que lo pedimos y que salimos (una
    // interrupcion suya que asigne memoria alcanza). En ese caso ExitBootServices
    // devuelve INVALID_PARAMETER y la especificacion manda volver a pedir el
    // mapa y reintentar. Tres vueltas y si no, se avisa.
    let mut attempt = 0;
    loop {
        attempt += 1;
        if attempt > 3 {
            return Machine::mute("the memory map changed three times in a row");
        }

        let mut size = BUFFER_SIZE;
        let mut key: usize = 0;
        let mut stride: usize = 0;
        let mut version: u32 = 0;

        let st = ((*bs).get_memory_map)(&mut size, buffer, &mut key, &mut stride, &mut version);

        if st == BUFFER_TOO_SMALL {
            return Machine::mute("the memory map does not fit in 32 KiB");
        }
        if st != SUCCESS {
            return Machine::mute("GetMemoryMap failed");
        }
        if stride < size_of::<Descriptor>() {
            return Machine::mute("el firmware reporto un descriptor imposible");
        }

        // Sin nada entremedio: la llave tiene que seguir fresca.
        let st = ((*bs).exit_boot_services)(image, key);
        if st == SUCCESS {
            // Unico camino que sale del bucle, y por eso el unico que escribe.
            used_size = size;
            descriptor_size = stride;
            break;
        }
        if st != INVALID_PARAMETER {
            return Machine::mute("ExitBootServices failed");
        }
    }

    ALREADY_TAKEN = true;

    // Desde aca no se puede llamar a ningun Boot Service. El buffer es memoria
    // nuestra, asi que interpretarlo ahora es seguro.
    let mut count = normalize(buffer, used_size, descriptor_size);
    count += add_pcie_window(&tables, count);

    Machine {
        regions: core::slice::from_raw_parts(&raw const REGIONS as *const Region, count),
        tables,
        failure: None,
        blob,
    }
}

/// Suma al mapa la ventana de configuracion de PCIe, si el firmware no la puso.
///
/// El mapa de memoria de UEFI **no es la unica cosa que la maquina dice de si
/// misma**: la tabla MCFG de ACPI dice donde se configura PCIe, y en aarch64 el
/// firmware no la repite en el mapa. Con lo cual el kernel publicaba una
/// direccion —`describe pcie` la sirve— que el mismo hacia inalcanzable: fuera
/// del mapa, `mem.claim` la rechaza, y fuera del span, el identity map ni la
/// cubre. El kernel siendo la razon por la que no se puede usar un aparato es
/// exactamente lo que P1 prohibe.
///
/// Va aca y no mas adelante porque el mapa se arma **una sola vez**: metida
/// antes de que nadie lo lea, la ventana entra sola en todo lo que se calcula a
/// partir del mapa, empezando por hasta donde llega el identity map.
///
/// # Safety
///
/// `rsdp` tiene que apuntar a un RSDP de verdad, y la memoria que describe estar
/// mapeada. Lo esta: el identity map del firmware sigue vigente.
unsafe fn add_pcie_window(tables: &kernel_core::Tables, count: usize) -> usize {
    if count >= MAX_REGIONS {
        return 0;
    }
    let Some(pcie) = kernel_core::tables::describe(tables).pcie else { return 0 };

    // Cada bus ocupa 1 MiB de espacio de configuracion: 32 dispositivos por 8
    // funciones por 4 KiB.
    let buses = (pcie.bus_end as u64).saturating_sub(pcie.bus_start as u64) + 1;
    let bytes = buses << 20;

    // Si el firmware ya la informo —en x86_64 la informa, como reservada— no se
    // agrega: dos regiones encimadas serian dos respuestas distintas a la misma
    // pregunta.
    let map = core::slice::from_raw_parts(&raw const REGIONS as *const Region, count);
    if map.iter().any(|r| r.start < pcie.base + bytes && r.end() > pcie.base) {
        return 0;
    }

    let dest = &raw mut REGIONS as *mut Region;
    // La ventana de configuracion son registros: no se cachea. Y esto no sale
    // del mapa de UEFI —que no la informa— sino de la MCFG, asi que el dato de
    // cacheabilidad lo pone quien sabe que es (P4).
    dest.add(count).write(Region {
        start: pcie.base,
        bytes,
        kind: Kind::Mmio,
        caching: Caching::Uncacheable,
    });
    1
}

/// Recorre la Configuration Table anotando donde esta cada cosa conocida.
///
/// Lo que no se reconoce se ignora en silencio: la lista trae cualquier cosa que
/// el firmware haya querido publicar, y no reconocer un GUID no es un error.
///
/// # Safety
///
/// `systab` tiene que estar ya verificado por firma.
unsafe fn find_tables(systab: *mut SystemTable) -> Tables {
    let mut t = Tables::default();

    let list = (*systab).config_table as *const ConfigEntry;
    if list.is_null() {
        return t;
    }

    // La cantidad la dice el firmware. Se acota por las dudas: si ese numero
    // viniera con basura, el bucle se iria a recorrer memoria cualquiera.
    let n = (*systab).config_table_len.min(256);

    for i in 0..n {
        let entry = &*list.add(i);
        let addr = entry.table as u64;
        if addr == 0 {
            continue;
        }

        // Las dos versiones de ACPI y de SMBIOS pueden estar las dos a la vez.
        // El orden de estos `if` hace que la nueva le gane a la vieja sin
        // importar en que orden aparezcan en la lista.
        if entry.guid == ACPI_20 {
            t.acpi = Some(addr);
        } else if entry.guid == ACPI_10 && t.acpi.is_none() {
            t.acpi = Some(addr);
        } else if entry.guid == DTB {
            t.device_tree = Some(addr);
        } else if entry.guid == SMBIOS3 {
            t.smbios = Some(addr);
        } else if entry.guid == SMBIOS && t.smbios.is_none() {
            t.smbios = Some(addr);
        }
    }
    t
}

/// Traduce los descriptores de UEFI al vocabulario del nucleo.
///
/// # Safety
///
/// `buffer` tiene que traer `size` bytes validos de descriptores.
unsafe fn normalize(buffer: *const u8, size: usize, stride: usize) -> usize {
    let dest = &raw mut REGIONS as *mut Region;
    let mut n = 0;
    let mut off = 0;

    // Se avanza con el `stride` que informo el firmware y NO con el tamano de la
    // estructura: la especificacion permite descriptores mas grandes que los 40
    // bytes de hoy, para poder crecer sin romper a nadie. Asumir 40 es un bug
    // clasico que aparece recien en el firmware que decide crecer.
    while off + stride <= size && n < MAX_REGIONS {
        let d = buffer.add(off) as *const Descriptor;

        let region = Region {
            start: (*d).physical_start,
            bytes: (*d).pages.saturating_mul(UEFI_PAGE),
            kind: classify((*d).kind),
            caching: caching_of((*d).attributes),
        };

        if region.bytes > 0 {
            dest.add(n).write(region);
            n += 1;
        }
        off += stride;
    }
    n
}

/// Los quince tipos de memoria de UEFI, traducidos a lo que le importa a quien
/// va a reclamar memoria.
fn classify(kind: u32) -> Kind {
    match kind {
        0 => Kind::Reserved,

        // 1 y 2 son LoaderCode y LoaderData: somos nosotros, el kernel que el
        // firmware cargo.
        1 | 2 => Kind::Kernel,

        // 3 y 4 son BootServicesCode y BootServicesData. Son libres *porque ya
        // salimos*: mientras los Boot Services vivian, era su codigo. Si esta
        // funcion se llamara antes de ExitBootServices, esto seria mentira.
        //
        // OJO (deuda anotada en DISENO.md): la pila sobre la que corre este
        // mismo codigo suele salir de aca. Marcarla libre esta bien para
        // *informar* el mapa, pero `mem.claim` no puede entregarla sin que el
        // kernel se haya mudado antes a una pila propia.
        3 | 4 => Kind::Free,

        // 5 y 6 son RuntimeServicesCode y Data: el firmware los sigue usando
        // despues de soltarnos la maquina.
        5 | 6 => Kind::Firmware,

        7 => Kind::Free,   // ConventionalMemory: la RAM de verdad
        8 => Kind::Broken, // el firmware la reporta defectuosa
        9 => Kind::AcpiTables,
        10 => Kind::FirmwareNvs,
        11 | 12 => Kind::Mmio, // MemoryMappedIO y IOPortSpace
        13 => Kind::Firmware,  // PalCode
        14 => Kind::Persistent,

        other => Kind::Other(other),
    }
}

/// Traduce los atributos de una region de UEFI a si se puede cachear (deuda 11).
///
/// UEFI informa, region por region, cuales de los modos de cache **soporta**:
/// `WB` es write-back (cacheable), `UC` es sin cachear, y estan tambien `WT` y
/// `WC`. Es mas preciso que deducirlo de la clase de memoria, que es lo que se
/// hacia: hay memoria reservada que igual es RAM cacheable, y rangos que parecen
/// RAM y no lo son.
///
/// **Se pregunta si soporta WB antes que si soporta UC, y en ese orden importa:**
/// la RAM comun soporta los dos, y en ese caso lo que se quiere es cachearla.
/// Al reves, toda la memoria de la maquina quedaria sin cache y andaria cien
/// veces mas lento.
///
/// Un cero significa que el firmware no dijo nada de esa region, que no es lo
/// mismo que decir que no se puede cachear.
fn caching_of(attributes: u64) -> Caching {
    /// `EFI_MEMORY_WB`: soporta write-back.
    const WB: u64 = 0x8;
    /// `EFI_MEMORY_UC`: soporta quedar sin cachear.
    const UC: u64 = 0x1;
    /// `EFI_MEMORY_WT` y `EFI_MEMORY_WC`: los otros dos que valen como cache.
    const WT: u64 = 0x4;
    const WC: u64 = 0x2;

    if attributes & (WB | WT | WC) != 0 {
        Caching::WriteBack
    } else if attributes & UC != 0 {
        Caching::Uncacheable
    } else {
        Caching::Unknown
    }
}

/// Trae `blob.bin` de la misma particion de la que salio el kernel.
///
/// # Safety
///
/// Solo **antes** de `ExitBootServices`, y con `image` y `systab` los que dio el
/// firmware. Va antes de pedir el mapa de memoria a proposito: abrir un archivo
/// puede mover el mapa, y la llave que `ExitBootServices` exige tiene que ser la
/// del mapa mas reciente (D25).
unsafe fn load_blob(image: *mut c_void, bs: *mut BootServices) -> Blob {
    // De que volumen salio el kernel. El blob esta al lado, que es lo que hace
    // que `blob.bin` sea reemplazable sin tocar `kernel.efi` (D20).
    let mut loaded: *mut c_void = core::ptr::null_mut();
    if ((*bs).handle_protocol)(image, &LOADED_IMAGE, &mut loaded) != SUCCESS || loaded.is_null() {
        return Blob::Failed("the firmware does not say where it loaded us from");
    }
    let device = (*(loaded as *mut LoadedImage)).device_handle;

    let mut fs: *mut c_void = core::ptr::null_mut();
    if ((*bs).handle_protocol)(device, &SIMPLE_FILE_SYSTEM, &mut fs) != SUCCESS || fs.is_null() {
        return Blob::Failed("ese volumen no ofrece sistema de archivos");
    }
    let fs = fs as *mut FileSystem;

    let mut root: *mut File = core::ptr::null_mut();
    if ((*fs).open_volume)(fs, &mut root) != SUCCESS || root.is_null() {
        return Blob::Failed("could not open the volume");
    }

    let mut file: *mut File = core::ptr::null_mut();
    let opened = ((*root).open)(root, &mut file, BLOB_NAME.as_ptr(), FILE_MODE_READ, 0);
    ((*root).close)(root);
    if opened != SUCCESS || file.is_null() {
        // Que no haya no es un fallo: es una maquina sin blob (D20 lo permite
        // explicitamente — borrable e ignorable).
        return Blob::Absent;
    }

    let dest = &raw mut BLOB as *mut u8;
    let mut size = MAX_BLOB;
    let read = ((*file).read)(file, &mut size, dest);
    if read != SUCCESS {
        ((*file).close)(file);
        return Blob::Failed("blob.bin could not be read");
    }

    // Si lleno el buffer justo, puede haber quedado archivo afuera. Se pregunta
    // en vez de suponer: **medio cargador es peor que ninguno**, porque salta a
    // codigo cortado en la mitad de una instruccion.
    if size == MAX_BLOB {
        let mut extra = 1usize;
        let mut byte = 0u8;
        let more = ((*file).read)(file, &mut extra, &mut byte);
        ((*file).close)(file);
        if more == SUCCESS && extra > 0 {
            return Blob::Failed("blob.bin no entra en el lugar reservado");
        }
        return Blob::Loaded(core::slice::from_raw_parts(dest, size));
    }

    ((*file).close)(file);
    if size == 0 {
        return Blob::Failed("blob.bin is empty");
    }
    Blob::Loaded(core::slice::from_raw_parts(dest, size))
}
