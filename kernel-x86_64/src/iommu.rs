//! El IOMMU de Intel (VT-d), que es quien hace cumplir `dma.allow` (D8).
//!
//! # Que problema resuelve
//!
//! Un dispositivo que hace DMA escribe en la RAM **por su cuenta**, sin pasar
//! por el CPU y sin mirar las tablas de paginas. Un puntero mal puesto en un
//! registro de una placa no da fault: da memoria distinta, en silencio, en
//! cualquier parte. Para un agente que depura el driver que acaba de escribir,
//! ese es el peor error posible — el sintoma aparece lejos de la causa y no se
//! parece a ella.
//!
//! El IOMMU pone una MMU entre el aparato y la memoria: el aparato pide una
//! direccion y el IOMMU la traduce, o la niega. Con eso, un DMA fuera de lo
//! declarado deja de ser corrupcion y pasa a ser un fault anotado, que se puede
//! leer (P5).
//!
//! # No es un guardarrail (P6)
//!
//! El kernel no decide que puede tocar cada aparato: lo declara el agente con
//! `dma.allow`, y el silicio lo hace cumplir. Lo que el kernel no hace es
//! dejarlo abierto "por las dudas": sin nada declarado, un aparato no llega a
//! ninguna parte.
//!
//! # Como se le habla
//!
//! Tres niveles de tablas encadenadas, y todas viven en memoria del kernel:
//!
//! ```text
//!   RTADDR_REG -> tabla raiz  -> tabla de contexto -> tablas de traduccion
//!                 (por bus)      (por dispositivo)   (4 niveles, como las
//!                                                     de paginas del CPU)
//! ```
//!
//! El aparato se nombra con su numero de PCIe —bus, dispositivo y funcion
//! juntos— que es lo que el silicio ve llegar en cada pedido de DMA.

use kernel_core::acpi::Iommu;

/// Los registros que se usan, como desplazamiento desde la base.
const GCMD: u64 = 0x18;
const GSTS: u64 = 0x1C;
const RTADDR: u64 = 0x20;
const CCMD: u64 = 0x28;
const FSTS: u64 = 0x34;
/// Donde arrancan los registros de invalidacion de la cache de traducciones.
/// No esta fijo: lo dice el propio IOMMU en ECAP.
const ECAP: u64 = 0x10;

/// Bit 30 de GCMD: tomar la direccion de la tabla raiz.
const GCMD_SRTP: u32 = 1 << 30;
/// Bit 31: encender la traduccion.
const GCMD_TE: u32 = 1 << 31;

/// Cuantas paginas de tablas se pueden repartir. Cada una son 4 KiB, y alcanzan
/// para unos cuantos reclamos: una traduccion de 4 KiB cuesta tres paginas la
/// primera vez y ninguna las siguientes si cae en el mismo bloque.
const POOL: usize = 32;

#[repr(C, align(4096))]
struct Pages([[u64; 512]; POOL]);

/// Las tablas viven adentro de la imagen del kernel, que el mapa informa como
/// memoria del kernel y por lo tanto nunca se entrega (D12). Si vivieran en
/// memoria reclamable, el agente podria pedir justo las tablas que dicen que
/// puede tocar el agente.
static mut PAGES: Pages = Pages([[0; 512]; POOL]);
static mut USED: usize = 0;

/// La tabla raiz: una entrada por bus, de 16 bytes.
#[repr(C, align(4096))]
struct Root([u64; 512]);
static mut ROOT: Root = Root([0; 512]);

/// Una tabla de contexto por bus. Se arma la del bus que haga falta.
static mut CONTEXT: Pages = Pages([[0; 512]; POOL]);
static mut CONTEXT_FOR: [u16; POOL] = [0xFFFF; POOL];
static mut CONTEXTS: usize = 0;

static mut BASE: u64 = 0;
static mut ENABLED: bool = false;

unsafe fn read32(off: u64) -> u32 {
    core::ptr::read_volatile((BASE + off) as *const u32)
}

unsafe fn write32(off: u64, v: u32) {
    core::ptr::write_volatile((BASE + off) as *mut u32, v);
}

unsafe fn read64(off: u64) -> u64 {
    core::ptr::read_volatile((BASE + off) as *const u64)
}

unsafe fn write64(off: u64, v: u64) {
    core::ptr::write_volatile((BASE + off) as *mut u64, v);
}

/// Una pagina de tabla en blanco, del pozo.
unsafe fn take_page() -> Option<u64> {
    if USED >= POOL {
        return None;
    }
    let p = core::ptr::addr_of_mut!((*core::ptr::addr_of_mut!(PAGES)).0[USED]);
    USED += 1;
    (*p).fill(0);
    Some(p as u64)
}

/// La tabla de contexto de un bus, armandola si es la primera vez.
unsafe fn context_for(bus: u8) -> Option<u64> {
    let table = &mut *core::ptr::addr_of_mut!(CONTEXT_FOR);
    for i in 0..CONTEXTS {
        if table[i] == bus as u16 {
            return Some(
                core::ptr::addr_of!((*core::ptr::addr_of!(CONTEXT)).0[i]) as u64
            );
        }
    }
    if CONTEXTS >= POOL {
        return None;
    }
    let i = CONTEXTS;
    CONTEXTS += 1;
    table[i] = bus as u16;
    let addr = core::ptr::addr_of_mut!((*core::ptr::addr_of_mut!(CONTEXT)).0[i]);
    (*addr).fill(0);

    // Y engancharla en la raiz: bit 0 presente, y la direccion.
    let root = &mut (*core::ptr::addr_of_mut!(ROOT)).0;
    root[bus as usize * 2] = (addr as u64) | 1;
    Some(addr as u64)
}

/// La entrada de traduccion de un dispositivo, armandola si hace falta.
///
/// Devuelve la raiz de sus tablas de traduccion. Cada dispositivo tiene las
/// suyas: compartirlas seria que permitirle algo a uno se lo permita al otro,
/// y eso ya no seria lo que el agente declaro.
unsafe fn tables_for(device: u32) -> Result<u64, &'static str> {
    let bus = (device >> 8) as u8;
    let slot = (device & 0xFF) as usize;

    let ctx = context_for(bus).ok_or("no hay lugar para otro bus")?;
    let entry = (ctx as *mut u64).add(slot * 2);

    if *entry & 1 != 0 {
        // Ya tenia: se devuelve la raiz que ya estaba.
        return Ok(*entry & 0x000F_FFFF_FFFF_F000);
    }

    let root = take_page().ok_or("no quedan paginas de tablas")?;
    // La parte de arriba: el ancho de direccion —2 son 48 bits, o sea cuatro
    // niveles, los mismos que usa el CPU— y el numero de dominio, que aca es
    // siempre el mismo porque hay un solo agente (D13).
    *entry.add(1) = 2 | (1 << 8);
    // Y la de abajo: la raiz de sus tablas, y presente.
    *entry = root | 1;
    Ok(root)
}

/// Mapea un rango en las tablas de un dispositivo, de a 4 KiB.
///
/// El grano fino es a proposito: el agente declara un rango y **eso** es lo que
/// el aparato alcanza. Redondear para arriba seria entregarle memoria que no
/// pidio, que es justo lo que D8 viene a evitar.
unsafe fn map(root: u64, start: u64, bytes: u64, allow: bool) -> Result<(), &'static str> {
    let first = start & !0xFFF;
    let last = (start + bytes - 1) & !0xFFF;

    let mut addr = first;
    loop {
        let mut table = root;
        // Tres niveles de indice, y el cuarto es la hoja.
        for level in (1..4).rev() {
            let index = ((addr >> (12 + 9 * level)) & 0x1FF) as usize;
            let slot = (table as *mut u64).add(index);
            if *slot & 1 == 0 {
                if !allow {
                    // No estaba mapeado: sacarlo no cuesta nada.
                    return Ok(());
                }
                let next = take_page().ok_or("no quedan paginas de tablas")?;
                // Lectura y escritura: quien decide que puede hacer el aparato
                // es la hoja, no el camino.
                *slot = next | 0b11;
            }
            table = *slot & 0x000F_FFFF_FFFF_F000;
        }
        let index = ((addr >> 12) & 0x1FF) as usize;
        let leaf = (table as *mut u64).add(index);
        // Identity map: el aparato ve las mismas direcciones que el agente
        // (D12), asi que no hay un segundo sistema de coordenadas al escribir
        // un driver.
        *leaf = if allow { addr | 0b11 } else { 0 };

        if addr == last {
            break;
        }
        addr += 4096;
    }
    Ok(())
}

/// Enciende el IOMMU con la tabla raiz vacia (D8).
///
/// Vacia significa que **ningun aparato llega a ninguna parte**: cada entrada
/// dice "no presente", y un DMA contra una entrada asi se niega y se anota. Es
/// el punto de partida contra el que `dma.allow` significa algo.
///
/// # Safety
///
/// Solo despues de `ExitBootServices`.
pub unsafe fn install(hw: &Iommu) -> Result<(), &'static str> {
    BASE = hw.base;
    enable()
}

/// Los bits de GCMD que son **estado** y no una orden de una sola vez.
///
/// GCMD no es una lista de botones: es el estado entero, y el silicio compara
/// lo que se le escribe contra lo que ya habia para saber que cambio. Escribir
/// una orden sola, sin arrastrar el estado, apaga todo lo demas — asi que pedir
/// "tomate la tabla raiz" apagaria la traduccion de paso.
///
/// Es un error facil de no ver: el IOMMU obedece las dos cosas, la pedida y la
/// no pedida, y despues todo pasa sin traducir. O sea, se ve como si anduviera.
const GCMD_STATE: u32 = 1 << 31 | 1 << 26 | 1 << 25;

/// Enciende el IOMMU con las tablas ya armadas.
unsafe fn enable() -> Result<(), &'static str> {
    let root = core::ptr::addr_of!(ROOT) as u64;
    write64(RTADDR, root);

    // Tomar la tabla raiz, arrastrando el estado que ya tenia. Y esperar a que
    // el silicio confirme que la tomo: un encendido que no ocurrio se ve igual
    // que uno que si.
    let state = read32(GSTS) & GCMD_STATE;
    write32(GCMD, state | GCMD_SRTP);
    let mut rounds = 0;
    while read32(GSTS) & (1 << 30) == 0 {
        rounds += 1;
        if rounds > 1_000_000 {
            return Err("el IOMMU no tomo la tabla raiz");
        }
        core::hint::spin_loop();
    }

    invalidate();

    let state = read32(GSTS) & GCMD_STATE;
    write32(GCMD, state | GCMD_TE);
    let mut rounds = 0;
    while read32(GSTS) & GCMD_TE == 0 {
        rounds += 1;
        if rounds > 1_000_000 {
            return Err("el IOMMU no encendio la traduccion");
        }
        core::hint::spin_loop();
    }
    ENABLED = true;
    Ok(())
}

/// Le dice al IOMMU que se olvide de lo que tenia cacheado.
///
/// Sin esto, un permiso recien dado puede no verse: el silicio se acuerda de
/// haber negado esa direccion y sigue negandola.
unsafe fn invalidate() {
    // Cache de contexto, global: bit 63 pide la invalidacion y los bits 61-60
    // en 01 dicen que es de todo.
    write64(CCMD, (1u64 << 63) | (1u64 << 61));
    let mut rounds = 0;
    while read64(CCMD) & (1 << 63) != 0 && rounds < 1_000_000 {
        rounds += 1;
        core::hint::spin_loop();
    }

    // Y la de traducciones. Sus registros no estan en un lugar fijo: el
    // desplazamiento lo publica el propio IOMMU en ECAP (P4).
    let iotlb = ((read64(ECAP) >> 8) & 0x3FF) * 16 + 8;
    write64(iotlb, (1u64 << 63) | (1u64 << 60));
    let mut rounds = 0;
    while read64(iotlb) & (1 << 63) != 0 && rounds < 1_000_000 {
        rounds += 1;
        core::hint::spin_loop();
    }
}

/// Declara que un aparato puede —o ya no puede— tocar un rango de memoria.
///
/// # Safety
///
/// El rango tiene que estar mapeado. Lo que el aparato haga adentro es asunto
/// del agente (P2).
pub unsafe fn set_access(
    hw: &Iommu,
    device: u32,
    start: u64,
    bytes: u64,
    allow: bool,
) -> Result<(), &'static str> {
    if bytes == 0 {
        return Err("un rango vacio no se puede declarar");
    }
    BASE = hw.base;

    if !ENABLED {
        return Err("el IOMMU no esta encendido");
    }
    let root = tables_for(device)?;
    map(root, start, bytes, allow)?;
    // Y que se olvide de lo que tenia cacheado: sin esto, el silicio se acuerda
    // de haber negado esa direccion y la sigue negando.
    invalidate();
    Ok(())
}

/// Lo que el IOMMU anoto: si hubo un DMA que no estaba permitido.
///
/// Es la parte de P5 que le toca al silicio. El bit 1 de FSTS se prende cuando
/// hay al menos un fault anotado, y el agente lo puede mirar para saber que su
/// driver apunto a donde no debia — en vez de encontrarse memoria distinta sin
/// explicacion.
pub fn faults() -> Option<u64> {
    // SAFETY: `set_access` dejo la direccion, y el identity map la cubre.
    unsafe {
        if BASE == 0 {
            return None;
        }
        Some(read32(FSTS) as u64)
    }
}

/// Si la traduccion esta encendida, segun el silicio y no segun nosotros.
pub fn is_on() -> bool {
    unsafe { BASE != 0 && read32(GSTS) & GCMD_TE != 0 }
}
