//! El IOMMU de ARM (SMMUv3), que es quien hace cumplir `dma.allow` (D8).
//!
//! # Que problema resuelve
//!
//! El mismo que el VT-d de x86_64, y por eso conviene leer primero
//! `kernel-x86_64/src/iommu.rs`: un aparato que hace DMA escribe en la RAM por
//! su cuenta, sin pasar por el CPU y sin mirar las tablas de paginas, asi que un
//! puntero mal puesto no da fault — da memoria distinta, en silencio.
//!
//! # En que NO se parece al de Intel
//!
//! Aca esta la razon de D22. Los dos hacen lo mismo y no se parecen en nada:
//!
//! - **Al VT-d se le habla por registros; al SMMUv3 por colas en memoria.** Para
//!   decirle "olvidate de lo que tenias cacheado" hay que dejarle un comando en
//!   un anillo y tocarle el registro que dice hasta donde escribimos. No hay una
//!   escritura que invalide: hay un productor y un consumidor.
//! - **La tabla que lo indexa no es por bus sino por aparato.** El silicio ve
//!   llegar un `StreamID` —en PCIe es el mismo numero que usa el bus, bus,
//!   dispositivo y funcion juntos— y con el entra a la **tabla de streams**.
//!   Cada entrada son 64 bytes y describe la traduccion completa de ese aparato.
//! - **Se usa la traduccion de etapa 2**, la que existe para virtualizar. No es
//!   porque haya maquinas virtuales: es la unica de las dos etapas cuya entrada
//!   de stream lleva **directo** la raiz de las tablas del aparato, sin un
//!   descriptor de contexto en el medio.
//!
//! # Como queda armado
//!
//! ```text
//!   STRTAB_BASE -> tabla de streams -> tabla de streams -> tablas de traduccion
//!                  (nivel 1)           (nivel 2, 64          (4 niveles, como
//!                                       aparatos)             las de paginas)
//!
//!   CMDQ_BASE   -> anillo de comandos   (invalidar, sincronizar)
//!   EVENTQ_BASE -> anillo de eventos    (lo que el silicio nego, anotado)
//! ```
//!
//! La tabla de streams va en dos niveles por la misma razon por la que el VT-d
//! arma una tabla de contexto por bus: en una sola pieza habria que reservar
//! 64 bytes por cada aparato que la maquina pueda nombrar —megabytes— para una
//! maquina que suele tener tres.

use kernel_core::acpi::Iommu;

// --- Registros, como desplazamiento desde la base --------------------------
const IDR0: u64 = 0x00;
const IDR1: u64 = 0x04;
const IDR5: u64 = 0x14;
const CR0: u64 = 0x20;
const CR0ACK: u64 = 0x24;
const CR1: u64 = 0x28;
const CR2: u64 = 0x2C;
const GERROR: u64 = 0x60;
const GERRORN: u64 = 0x64;
const STRTAB_BASE: u64 = 0x80;
const STRTAB_BASE_CFG: u64 = 0x88;
const CMDQ_BASE: u64 = 0x90;
const CMDQ_PROD: u64 = 0x98;
const CMDQ_CONS: u64 = 0x9C;
const EVENTQ_BASE: u64 = 0xA0;

/// Los punteros de la cola de eventos viven en la **segunda pagina** del bloque
/// de registros, y no es un capricho del que escribio el manual: asi se le puede
/// dar la cola de eventos a otro nivel de privilegio sin darle el resto del
/// SMMU. Buscarlos en la primera pagina es leer basura.
const EVENTQ_PROD: u64 = 0x1_00A8;
const EVENTQ_CONS: u64 = 0x1_00AC;

/// Bit 0 de CR0: encender la traduccion.
const CR0_SMMUEN: u32 = 1 << 0;
/// Bit 2: hacerle caso a la cola de eventos.
const CR0_EVENTQEN: u32 = 1 << 2;
/// Bit 3: hacerle caso a la cola de comandos.
const CR0_CMDQEN: u32 = 1 << 3;

// --- Comandos ---------------------------------------------------------------
/// Olvidate de la entrada de stream de este aparato.
const CMD_CFGI_STE: u64 = 0x03;
/// Olvidate de un rango de entradas de stream. Con `range = 31`, de todas.
const CMD_CFGI_STE_RANGE: u64 = 0x04;
/// Olvidate de las traducciones de este dominio.
const CMD_TLBI_S12_VMALL: u64 = 0x28;
/// Olvidate de todas las traducciones.
const CMD_TLBI_NSNH_ALL: u64 = 0x30;
/// Avisame cuando terminaste lo anterior.
const CMD_SYNC: u64 = 0x46;

// --- Lo que se le reserva ---------------------------------------------------
/// Cuantos aparatos entran en una tabla de streams de nivel 2. Sesenta y cuatro
/// entradas de 64 bytes son exactamente una pagina, y 6 es uno de los tres
/// valores que el silicio acepta para partir el numero de aparato.
const SPLIT: u32 = 6;

/// Hasta cuantos bits de numero de aparato se cubren. En PCIe el numero tiene
/// 16 bits y no puede tener mas, asi que esto **cubre el bus entero**: no es un
/// tope nuestro disfrazado de tope del bus (P1).
const MAX_SID_BITS: u32 = 16;

/// La tabla de nivel 1: un descriptor de 8 bytes por cada grupo de 64 aparatos.
const L1_ENTRIES: usize = 1 << (MAX_SID_BITS - SPLIT);

/// Cuantas paginas de 4 KiB hay para repartir entre tablas de streams de nivel 2
/// y tablas de traduccion. Es el mismo pozo para las dos cosas porque las dos
/// son paginas: el que primero pida, primero se lleva.
const POOL: usize = 32;

/// Cuantos comandos y cuantos eventos entran en cada anillo, como potencia de 2.
const QUEUE_BITS: u32 = 6;
const QUEUE_ENTRIES: usize = 1 << QUEUE_BITS;

/// El numero de dominio de traduccion. Hay un solo agente (D13), asi que uno
/// alcanza; lo que no puede ser es cero, que en etapa 2 no identifica nada.
const VMID: u64 = 1;

/// Cuantos bits de direccion alcanza un aparato: 64 - 20 = 44, o sea 16 TiB.
///
/// El numero no es de gusto: el silicio exige que el tamano de entrada de la
/// etapa 2 **no sea menor** que el de salida, o sea que `T0SZ` no pase de
/// `64 - OAS`. Pedir menos —39 bits, que alcanzarian de sobra— no se rechaza:
/// se reinterpreta, y la direccion que pide el aparato se recorta en silencio.
/// Se comprueba contra el `OAS` que informa la maquina antes de usarlo (P4).
const S2_T0SZ: u64 = 20;
/// Empezar el recorrido en el nivel 0, que es lo que corresponde a 44 bits.
const S2_SL0: u64 = 2;
/// Cuantos niveles se recorren: del 0 al 3, y el 3 es la hoja de 4 KiB.
const S2_LEVELS: u32 = 4;
/// El techo de lo que un aparato puede alcanzar, en bytes.
const S2_LIMIT: u64 = 1 << (64 - S2_T0SZ);

#[repr(C, align(4096))]
struct Pages([[u64; 512]; POOL]);

/// Todo esto vive adentro de la imagen del kernel, que el mapa informa como
/// memoria del kernel y por lo tanto nunca se entrega (D12). Si viviera en
/// memoria reclamable, el agente podria pedir justo las tablas que dicen lo que
/// puede tocar cada aparato.
static mut PAGES: Pages = Pages([[0; 512]; POOL]);
static mut USED: usize = 0;

/// La tabla de streams de nivel 1. El silicio se come los bits de abajo de la
/// direccion que le demos, asi que tiene que estar alineada a su tamano: 8 KiB.
///
/// **Se pide el doble de lugar y se alinea a mano, y no es un rodeo.** Un
/// `align` mayor que la pagina es una promesa que el cargador no cumple: el
/// linker deja el simbolo alineado adentro de la imagen, pero UEFI carga la
/// imagen en una direccion alineada a 4 KiB, y ahi una alineacion de 8 KiB se
/// pierde. Lo caro no es que la tabla quede desalineada — es que **el
/// compilador le cree al `align`** y, sabiendo que los bits de abajo "son
/// cero", simplifique la mascara con la que se arma la direccion. La tabla
/// termina apuntando una pagina mas abajo y el SMMU lee ceros donde escribimos.
#[repr(C, align(4096))]
struct StreamL1([u64; 2 * L1_ENTRIES]);
static mut STREAMS: StreamL1 = StreamL1([0; 2 * L1_ENTRIES]);

/// Donde arranca de verdad la tabla, ya alineada. Se calcula una sola vez, en
/// el encendido, y todos la leen de aca: que la direccion sea un dato de
/// runtime es lo que le saca al compilador la suposicion que rompio esto.
static mut STRTAB: u64 = 0;

/// El anillo de comandos: dos palabras por comando.
#[repr(C, align(4096))]
struct Commands([u64; 2 * QUEUE_ENTRIES]);
static mut CMDQ: Commands = Commands([0; 2 * QUEUE_ENTRIES]);

/// El de eventos: cuatro palabras por evento.
#[repr(C, align(4096))]
struct Events([u64; 4 * QUEUE_ENTRIES]);
static mut EVENTQ: Events = Events([0; 4 * QUEUE_ENTRIES]);

static mut BASE: u64 = 0;
static mut ENABLED: bool = false;
/// Cuantos bits de numero de aparato cubre la tabla, ya recortado a lo que la
/// maquina dice que puede nombrar.
static mut SID_BITS: u32 = 0;
/// Cuantos bits de indice tiene cada anillo. Puede ser menos que `QUEUE_BITS` si
/// el silicio no acepta anillos tan grandes.
static mut CMDQ_BITS: u32 = QUEUE_BITS;
static mut EVENTQ_BITS: u32 = QUEUE_BITS;
/// Cuantos accesos nego el silicio desde el arranque. Se cuenta al vaciar la
/// cola: si no se vaciara, se llenaria y los siguientes se perderian.
static mut DENIED: u64 = 0;

unsafe fn read32(off: u64) -> u32 {
    core::ptr::read_volatile((BASE + off) as *const u32)
}

unsafe fn write32(off: u64, v: u32) {
    core::ptr::write_volatile((BASE + off) as *mut u32, v);
}

unsafe fn write64(off: u64, v: u64) {
    core::ptr::write_volatile((BASE + off) as *mut u64, v);
}

/// Que lo escrito hasta aca se vea antes que lo que viene despues.
unsafe fn barrier() {
    core::arch::asm!("dsb sy", options(nomem, nostack, preserves_flags));
}

/// Empuja lo escrito en memoria hasta donde lo ve el SMMU.
///
/// Las tablas y los anillos los escribe el CPU, que tiene cache, y los lee el
/// SMMU, que puede no compartirla. En QEMU no hay caches y esto no cambia nada;
/// en silicio de verdad, sin esto el SMMU lee la version vieja de una tabla que
/// para el CPU ya estaba escrita — y el sintoma seria un permiso que no toma.
///
/// El tamano de la linea de cache no se supone: lo dice `CTR_EL0` (P4).
unsafe fn publish(addr: u64, bytes: usize) {
    let ctr: u64;
    core::arch::asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack));
    let line = 4u64 << ((ctr >> 16) & 0xF);

    let end = addr + bytes as u64;
    let mut a = addr & !(line - 1);
    while a < end {
        core::arch::asm!("dc civac, {}", in(reg) a, options(nostack, preserves_flags));
        a += line;
    }
    barrier();
}

/// Una pagina en blanco, del pozo.
unsafe fn take_page() -> Option<u64> {
    if USED >= POOL {
        return None;
    }
    let p = core::ptr::addr_of_mut!((*core::ptr::addr_of_mut!(PAGES)).0[USED]);
    USED += 1;
    (*p).fill(0);
    publish(p as u64, 4096);
    Some(p as u64)
}

// ---------------------------------------------------------------------------
// La cola de comandos
// ---------------------------------------------------------------------------

/// Deja un comando en el anillo y le avisa al silicio que lo mire.
///
/// El anillo tiene un indice de productor y uno de consumidor, y el bit de
/// arriba del indice dice cuantas vueltas lleva dado: sin el, un anillo lleno y
/// uno vacio se verian iguales.
unsafe fn push(dw0: u64, dw1: u64) -> Result<(), &'static str> {
    let mask = (1u32 << CMDQ_BITS) - 1;
    let wrap = 1u32 << CMDQ_BITS;

    let prod = read32(CMDQ_PROD);
    // Esperar lugar: lleno es "el productor le lleva una vuelta al consumidor".
    let mut rounds = 0;
    loop {
        let cons = read32(CMDQ_CONS);
        if (prod & mask) != (cons & mask) || (prod & wrap) == (cons & wrap) {
            break;
        }
        rounds += 1;
        if rounds > 1_000_000 {
            return Err("the SMMU command queue does not drain");
        }
        core::hint::spin_loop();
    }

    let slot = (prod & mask) as usize * 2;
    let q = &mut (*core::ptr::addr_of_mut!(CMDQ)).0;
    q[slot] = dw0;
    q[slot + 1] = dw1;
    publish(core::ptr::addr_of!(q[slot]) as u64, 16);

    // Y recien ahora avisar. Al reves, el silicio podria leer la ranura antes de
    // que este escrita.
    let next = if (prod & mask) == mask { (prod ^ wrap) & !mask } else { prod + 1 };
    write32(CMDQ_PROD, next);
    Ok(())
}

/// Deja un `SYNC` y espera a que el silicio lo consuma.
///
/// Es la unica forma de saber que las invalidaciones de arriba ya ocurrieron:
/// dejar el comando no es que se haya ejecutado, y creer que si es el error que
/// hace que un permiso recien dado no ande una vez de cada tantas.
unsafe fn sync() -> Result<(), &'static str> {
    push(CMD_SYNC, 0)?;
    let prod = read32(CMDQ_PROD);
    let mut rounds = 0;
    while read32(CMDQ_CONS) != prod {
        rounds += 1;
        if rounds > 10_000_000 {
            return Err("el SMMU no termino los comandos");
        }
        core::hint::spin_loop();
    }
    Ok(())
}

/// Que se olvide de todo lo que tenia cacheado sobre un aparato.
unsafe fn invalidate(sid: u32) -> Result<(), &'static str> {
    push(CMD_CFGI_STE | ((sid as u64) << 32), 1)?;
    push(CMD_TLBI_S12_VMALL | (VMID << 32), 0)?;
    sync()
}

// ---------------------------------------------------------------------------
// La tabla de streams
// ---------------------------------------------------------------------------

/// La entrada de 64 bytes de un aparato, armando la tabla de nivel 2 si es la
/// primera vez que aparece uno de ese grupo.
unsafe fn entry_for(sid: u32) -> Result<*mut u64, &'static str> {
    if sid >= (1 << SID_BITS) {
        return Err("this machine names no device with that number");
    }
    let top = (STRTAB as *mut u64).add((sid >> SPLIT) as usize);
    let bottom = (sid & ((1 << SPLIT) - 1)) as usize;

    // Los cinco bits de abajo del descriptor son el `span`: cuantas entradas
    // tiene la tabla de abajo. En cero, no hay tabla — y un aparato que cae ahi
    // se aborta y **queda anotado**, que es justo el estado inicial que D8 pide.
    if *top & 0x1F == 0 {
        let page = take_page().ok_or("no pages left for the stream table")?;
        *top = (page & 0x000F_FFFF_FFFF_FFC0) | (SPLIT as u64 + 1);
        publish(top as u64, 8);
    }
    let table = *top & 0x000F_FFFF_FFFF_FFC0;
    Ok((table as *mut u64).add(bottom * 8))
}

/// La raiz de las tablas de traduccion de un aparato, armandolas si hace falta.
///
/// Cada aparato tiene las suyas: compartirlas seria que permitirle algo a uno se
/// lo permita al otro, y eso ya no seria lo que el agente declaro.
unsafe fn tables_for(sid: u32, oas: u64) -> Result<u64, &'static str> {
    let ste = entry_for(sid)?;

    // Bit 0: la entrada vale. Si ya valia, sus tablas son las que ya estaban.
    if *ste & 1 != 0 {
        return Ok(*ste.add(3) & 0x000F_FFFF_FFFF_FFF0);
    }

    let root = take_page().ok_or("no table pages left")?;

    // Como se recorren las tablas: cuantos bits de direccion, en que nivel se
    // empieza, y con que atributos las lee el propio SMMU.
    let vtcr = S2_T0SZ
        | (S2_SL0 << 6)
        | (1 << 8)      // lee las tablas cacheando
        | (1 << 10)
        | (3 << 12)     // y compartidas con los nucleos
        | (oas << 16);  // cuantos bits de direccion fisica maneja (P4)

    // Primero el resto de la entrada y despues la palabra que la hace valer: al
    // reves, el silicio podria mirarla a medio escribir.
    *ste.add(1) = 1 << 44; // que la compartibilidad la ponga quien pide
    *ste.add(2) = VMID
        | (vtcr << 32)
        | (1 << 51)  // direcciones de 64 bits
        | (1 << 54)  // recorridos de tabla protegidos
        | (1 << 58); // y que anote lo que niegue: sin esto, negar seria mudo
    *ste.add(3) = root & 0x000F_FFFF_FFFF_FFF0;
    for i in 4..8 {
        *ste.add(i) = 0;
    }
    publish(ste as u64 + 8, 56);
    // Vale (bit 0) y traduce en etapa 2 (los tres bits de arriba en 0b110).
    *ste = 1 | (0b110 << 1);
    publish(ste as u64, 8);

    Ok(root)
}

// ---------------------------------------------------------------------------
// Las tablas de traduccion
// ---------------------------------------------------------------------------

/// Los dos bits de abajo en 0b11: apunta a otra tabla, o es una pagina en el
/// ultimo nivel.
const IS_TABLE: u64 = 0b11;
/// Access Flag: en cero, el primer acceso da fault en vez de andar.
const AF: u64 = 1 << 10;
/// Compartida con los nucleos.
const SHAREABLE: u64 = 0b11 << 8;
/// Lectura y escritura para el aparato.
const S2AP_RW: u64 = 0b11 << 6;
/// Memoria normal, cacheable de los dos lados.
const MEMATTR: u64 = 0b1111 << 2;

/// Mapea un rango en las tablas de un aparato, de a 4 KiB.
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
        // Los niveles de arriba son indices, y el ultimo es la hoja.
        for level in (1..S2_LEVELS).rev() {
            let index = ((addr >> (12 + 9 * level)) & 0x1FF) as usize;
            let slot = (table as *mut u64).add(index);
            if *slot & 1 == 0 {
                if !allow {
                    // No estaba mapeado: sacarlo no cuesta nada.
                    return Ok(());
                }
                let next = take_page().ok_or("no table pages left")?;
                *slot = next | IS_TABLE;
                publish(slot as u64, 8);
            }
            table = *slot & 0x000F_FFFF_FFFF_F000;
        }
        let index = ((addr >> 12) & 0x1FF) as usize;
        let leaf = (table as *mut u64).add(index);
        // Identity map: el aparato ve las mismas direcciones que el agente
        // (D12), asi que no hay un segundo sistema de coordenadas al escribir un
        // driver.
        *leaf = if allow { addr | IS_TABLE | AF | SHAREABLE | S2AP_RW | MEMATTR } else { 0 };
        publish(leaf as u64, 8);

        if addr == last {
            break;
        }
        addr += 4096;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Encendido
// ---------------------------------------------------------------------------

/// Espera a que el silicio confirme que CR0 quedo como se pidio.
///
/// CR0 no se aplica al escribirlo: hay un registro aparte que dice que quedo
/// puesto de verdad. Un encendido que no ocurrio se ve igual que uno que si.
unsafe fn commit(cr0: u32) -> Result<(), &'static str> {
    write32(CR0, cr0);
    let mut rounds = 0;
    while read32(CR0ACK) & cr0 != cr0 {
        rounds += 1;
        if rounds > 1_000_000 {
            return Err("el SMMU no confirmo el encendido");
        }
        core::hint::spin_loop();
    }
    Ok(())
}

/// Enciende el SMMU con la tabla de streams vacia (D8).
///
/// Vacia significa que **ningun aparato llega a ninguna parte**: sin descriptor
/// de nivel 1, el aparato no tiene entrada, y un pedido asi se aborta y **se
/// anota en la cola de eventos**. Es el punto de partida contra el que
/// `dma.allow` significa algo — y ademas se puede leer, que es P5 aplicado a lo
/// que hacen los aparatos.
///
/// # Safety
///
/// Solo despues de `ExitBootServices`.
pub unsafe fn install(hw: &Iommu) -> Result<(), &'static str> {
    BASE = hw.base;

    // Antes de programarlo, preguntarle si puede hacer lo que se le va a pedir.
    // Suponerlo seria escribir registros que quiza no existen y creer que
    // anduvo (P4).
    let idr0 = read32(IDR0);
    if idr0 & 1 == 0 {
        return Err("este SMMU no traduce en etapa 2");
    }
    if (idr0 >> 27) & 0b11 == 0 {
        return Err("this SMMU has no two-level stream table");
    }
    let idr1 = read32(IDR1);
    SID_BITS = (idr1 & 0x3F).min(MAX_SID_BITS);
    CMDQ_BITS = QUEUE_BITS.min((idr1 >> 21) & 0x1F);
    EVENTQ_BITS = QUEUE_BITS.min((idr1 >> 16) & 0x1F);
    if SID_BITS <= SPLIT || CMDQ_BITS < 2 || EVENTQ_BITS < 2 {
        return Err("this SMMU is smaller than what the kernel knows how to build");
    }
    // Cuantos bits de direccion fisica maneja: 0 son 32, 1 son 36, 2 son 40,
    // 3 son 42, 4 son 44. Tienen que ser al menos los 44 que se le declaran:
    // el silicio no acepta una entrada mas chica que su salida.
    let oas = (read32(IDR5) & 0b111) as u64;
    if oas < 4 {
        return Err("este SMMU direcciona menos de lo que el kernel le declara");
    }

    // Apagado para configurarlo, y sin errores viejos colgando: el firmware
    // pudo haberlo usado para leer el disco.
    commit(0)?;
    write32(GERRORN, read32(GERROR));
    // Como lee el silicio las tablas y los anillos: cacheando y compartido con
    // los nucleos, que es lo que corresponde a memoria normal del kernel.
    write32(CR1, (3 << 10) | (1 << 8) | (1 << 6) | (3 << 4) | (1 << 2) | 1);
    write32(CR2, 0);

    // Alineada a mano a su propio tamano: ver el comentario de `StreamL1`.
    let size = (L1_ENTRIES * 8) as u64;
    STRTAB = (core::ptr::addr_of!(STREAMS) as u64).next_multiple_of(size);
    core::ptr::write_bytes(STRTAB as *mut u8, 0, size as usize);
    publish(STRTAB, size as usize);
    write64(STRTAB_BASE, STRTAB);
    // En dos niveles (formato 1), partiendo el numero de aparato en `SPLIT`.
    write32(STRTAB_BASE_CFG, (1 << 16) | (SPLIT << 6) | SID_BITS);

    let cmdq = core::ptr::addr_of!(CMDQ) as u64;
    write64(CMDQ_BASE, (cmdq & 0x000F_FFFF_FFFF_FFE0) | CMDQ_BITS as u64);
    write32(CMDQ_PROD, 0);
    write32(CMDQ_CONS, 0);
    commit(CR0_CMDQEN)?;

    // Recien ahora se le puede hablar. Que se olvide de toda configuracion y de
    // toda traduccion que tuviera de antes.
    push(CMD_CFGI_STE_RANGE, 31)?;
    push(CMD_TLBI_NSNH_ALL, 0)?;
    sync()?;

    let eventq = core::ptr::addr_of!(EVENTQ) as u64;
    write64(EVENTQ_BASE, (eventq & 0x000F_FFFF_FFFF_FFE0) | EVENTQ_BITS as u64);
    write32(EVENTQ_PROD, 0);
    write32(EVENTQ_CONS, 0);
    commit(CR0_CMDQEN | CR0_EVENTQEN)?;

    commit(CR0_CMDQEN | CR0_EVENTQEN | CR0_SMMUEN)?;
    barrier();
    ENABLED = true;
    Ok(())
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
        return Err("an empty range cannot be declared");
    }
    if start.saturating_add(bytes) > S2_LIMIT {
        return Err("the device cannot reach addresses that high");
    }
    BASE = hw.base;
    if !ENABLED {
        return Err("el SMMU no esta encendido");
    }

    let oas = (read32(IDR5) & 0b111) as u64;
    let root = tables_for(device, oas)?;
    map(root, start, bytes, allow)?;
    // Y que se olvide de lo que tenia cacheado: sin esto, el silicio se acuerda
    // de haber negado esa direccion y la sigue negando.
    invalidate(device)
}

/// Cuantos accesos nego el silicio: la parte de P5 que le toca al hardware.
///
/// El SMMU no tiene un contador — anota cada negacion como un evento en un
/// anillo. Asi que contarlos es vaciarlo: si no se vaciara, se llenaria y las
/// negaciones siguientes se perderian, que es justo lo contrario de lo que
/// sirve. Cada evento son 32 bytes con la causa, el aparato y la direccion; por
/// ahora solo se cuentan.
pub fn faults() -> Option<u64> {
    // SAFETY: `install` dejo la direccion, y el identity map la cubre.
    unsafe {
        if BASE == 0 || !ENABLED {
            return None;
        }
        let mask = (1u32 << EVENTQ_BITS) - 1;
        let wrap = 1u32 << EVENTQ_BITS;
        let prod = read32(EVENTQ_PROD);
        let cons = read32(EVENTQ_CONS);
        if prod != cons {
            let n = if (prod & wrap) == (cons & wrap) {
                (prod & mask).wrapping_sub(cons & mask)
            } else {
                (1 << EVENTQ_BITS) - (cons & mask) + (prod & mask)
            };
            DENIED += n as u64;
            write32(EVENTQ_CONS, prod);
        }
        Some(DENIED)
    }
}

/// Si la traduccion esta encendida, segun el silicio y no segun nosotros.
pub fn is_on() -> bool {
    unsafe { BASE != 0 && read32(CR0ACK) & CR0_SMMUEN != 0 }
}
