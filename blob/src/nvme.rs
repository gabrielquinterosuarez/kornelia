//! El driver de NVMe del blob: lo que convierte al blob en un **cargador** (D19).
//!
//! # Por que el disco y no la red
//!
//! Porque el blob tiene que entrar en la particion y ser chico. Con el disco
//! adentro puede traer un payload tan grande como haga falta y saltar ahi, y la
//! red va en ese payload. Al reves —la red adentro del blob— el blob crece y
//! sigue sin poder cargar nada.
//!
//! Y porque a un NVMe se lo maneja **por clase**: `01.08.02` quiere decir
//! "cualquier NVMe" y este mismo codigo sirve contra el de QEMU y contra el de
//! una maquina de verdad. Con una placa de red eso no existe (ver el driver de
//! red en `client.py`), asi que el disco es el unico driver que se puede
//! escribir una vez.
//!
//! # Como habla con la maquina
//!
//! Dos caminos, y la diferencia importa:
//!
//! - **Los registros del controlador** van por el protocolo (`mem.read` /
//!   `mem.write`), porque hay que tocarlos con el ancho exacto y porque un
//!   acceso que el bus rechaza tiene que volver como dato y no matar la maquina.
//! - **Las colas y los buffers** se tocan **directo**, porque se reclaman con
//!   `user: true` y entonces el blob —que corre sin privilegio (D29)— los tiene
//!   mapeados. Pasar cada byte de una cola por el protocolo seria absurdo: son
//!   64 bytes por comando y no hay nada que el kernel tenga que mediar ahi.
//!
//! Los numeros salen de la especificacion NVMe 1.4, que es publica.

use crate::kernel::{Claim, Session};

// Registros del controlador, dentro de su ventana de memoria.
const CAP: u64 = 0x00; // que sabe hacer (64 bits)
const CC: u64 = 0x14; // configuracion: aca se lo prende y se lo apaga
const CSTS: u64 = 0x1C; // estado: aca contesta si esta listo
const AQA: u64 = 0x24; // cuantas entradas tienen las colas de administracion
const ASQ: u64 = 0x28; // donde esta la cola de pedidos
const ACQ: u64 = 0x30; // y la de respuestas

const CC_ENABLE: u64 = 1;

/// Cuantas entradas tiene cada cola de administracion. Cuatro alcanzan: los
/// pedidos de administracion son un punado y se hacen de a uno.
const ENTRIES: u64 = 4;
/// Una entrada de pedido son 64 bytes; una de respuesta, 16.
const SQ_ENTRY: u64 = 64;
const CQ_ENTRY: u64 = 16;

/// Cuanto se espera algo del controlador antes de darlo por muerto.
///
/// Se cuenta en vueltas y no en tiempo porque el blob corre antes de que exista
/// nada: no tiene reloj ni forma de pedirlo. Es un tope de seguridad, no una
/// medida — lo que de verdad manda es que el controlador conteste.
const SPINS: u32 = 20_000_000;

/// Escribe ocho bytes en memoria propia, sin que el compilador los agrupe.
///
/// # Safety
///
/// `at` tiene que caer en memoria reclamada y alcanzable sin privilegio.
unsafe fn poke64(at: u64, value: u64) {
    core::ptr::write_volatile(at as *mut u64, value);
}

/// Lee ocho bytes de memoria propia.
///
/// # Safety
///
/// Lo mismo que `poke64`, y `at` alineada a 8.
unsafe fn peek64(at: u64) -> u64 {
    core::ptr::read_volatile(at as *const u64)
}

/// Lee cuatro. Hace falta aparte porque no todo lo que se lee cae alineado a
/// ocho, y leer 64 bits de una direccion que no lo esta no es valido.
///
/// # Safety
///
/// Lo mismo, con `at` alineada a 4.
unsafe fn peek32(at: u64) -> u32 {
    core::ptr::read_volatile(at as *const u32)
}

/// Pone en cero un rango de memoria propia.
///
/// De a ocho bytes y con escrituras volatiles: un bucle comun lo convertiria el
/// compilador en una llamada a `memset`, que en un blob no puede existir (ver
/// `blob.ld`).
///
/// # Safety
///
/// `at..at+bytes` tiene que ser memoria reclamada, alcanzable y multiplo de 8.
unsafe fn zero(at: u64, bytes: u64) {
    let mut off = 0;
    while off < bytes {
        poke64(at + off, 0);
        off += 8;
    }
}

/// Un controlador NVMe listo para recibir comandos.
pub struct Nvme {
    /// Como lo nombra el bus. Es lo que hay que decirle al IOMMU.
    pub bdf: u64,
    /// Su ventana de registros.
    window: Claim,
    /// La cola de pedidos y la de respuestas, en memoria del agente.
    sq: Claim,
    cq: Claim,
    /// Cada cuanto esta el timbre de la cola siguiente. Lo dice el aparato.
    stride: u64,
    /// Por donde va cada anillo, y con que bit se reconoce lo nuevo.
    sq_tail: u64,
    cq_head: u64,
    phase: u64,
    /// Con que se etiqueta cada comando.
    tag: u16,
}

/// Cuanto mide el disco.
pub struct Namespace {
    pub blocks: u64,
    pub block_bytes: u64,
}

impl Nvme {
    /// Apaga el controlador, le da sus colas y lo prende.
    ///
    /// Es la secuencia que manda la especificacion y no se puede acortar: hay
    /// que verlo apagado antes de configurarlo, porque los registros de las
    /// colas solo se leen cuando pasa de apagado a prendido.
    pub fn start(k: &mut Session, bdf: u64, at: u64) -> Option<Nvme> {
        // 16 KiB: los registros entran en la primera pagina, pero los timbres de
        // las colas viven a partir de 0x1000 y hay uno por cola.
        let window = k.claim_at(at, 16384)?;

        let cap = k.read_reg(window.handle, CAP, 8)?;
        // Cada cuanto esta el timbre siguiente lo dice el aparato, no la
        // costumbre: con separacion distinta de 4 los timbres caen en otro lado.
        let stride = 4u64 << ((cap >> 32) & 0xF);

        // 1. Apagarlo y esperar a que lo confirme.
        k.write_reg(window.handle, CC, 0, 4)?;
        wait_ready(k, &window, 0)?;

        // 2. Las dos colas. Se piden alcanzables sin privilegio para poder
        //    escribirlas directo, y se le declaran al IOMMU para que el aparato
        //    llegue: las dos cosas hacen falta y ninguna sirve sola.
        let sq = claim_shared(k, bdf, 4096)?;
        let cq = claim_shared(k, bdf, 4096)?;

        // 3. Decirle donde estan y de que tamano. AQA lleva las dos cantidades
        //    menos uno, cada una en su mitad.
        k.write_reg(window.handle, AQA, ((ENTRIES - 1) << 16) | (ENTRIES - 1), 4)?;
        k.write_reg(window.handle, ASQ, sq.start, 8)?;
        k.write_reg(window.handle, ACQ, cq.start, 8)?;

        // 4. Prenderlo. Los numeros del medio son los tamanos de entrada por
        //    omision (64 y 16 bytes) y el conjunto de comandos NVM.
        k.write_reg(window.handle, CC, CC_ENABLE | (6 << 16) | (4 << 20), 4)?;
        wait_ready(k, &window, 1)?;

        Some(Nvme {
            bdf,
            window,
            sq,
            cq,
            stride,
            sq_tail: 0,
            cq_head: 0,
            phase: 1,
            tag: 1,
        })
    }

    /// Donde esta el timbre de una cola. `which` es 0 para pedidos, 1 para
    /// respuestas.
    fn bell(&self, queue: u64, which: u64) -> u64 {
        0x1000 + (2 * queue + which) * self.stride
    }

}

/// Espera a que `CSTS.RDY` diga lo que se le pidio a `CC.EN`.
fn wait_ready(k: &mut Session, window: &Claim, want: u64) -> Option<()> {
    let mut spins = 0u32;
    while spins < SPINS {
        let csts = k.read_reg(window.handle, CSTS, 4)?;
        if csts & 1 == want {
            return Some(());
        }
        // Bit 1: se murio y no hay nada que esperar.
        if csts & 2 != 0 {
            return None;
        }
        spins += 1;
    }
    None
}

/// Memoria que el aparato tambien toca: pedida, alcanzable, limpia y declarada.
///
/// Los cuatro pasos van juntos **siempre**, y por eso van en una sola funcion.
/// Sin limpiar se leen respuestas que nadie escribio —una entrada vieja tiene el
/// bit de fase puesto— y sin declarar el IOMMU la bloquea, que se ve igual que
/// un aparato que no contesta.
fn claim_shared(k: &mut Session, bdf: u64, bytes: u64) -> Option<Claim> {
    let claim = k.claim(bytes, 4096)?;
    // SAFETY: recien reclamada, nuestra, y alcanzable sin privilegio.
    unsafe { zero(claim.start, bytes) };
    k.dma_allow(bdf, claim.handle)?;
    Some(claim)
}

impl Nvme {
    /// Manda un comando y espera la respuesta. Necesita el canal para el timbre.
    pub fn command(
        &mut self,
        k: &mut Session,
        opcode: u8,
        nsid: u32,
        prp1: u64,
        cdw10: u32,
    ) -> Option<()> {
        let slot = self.sq_tail;
        let at = self.sq.start + slot * SQ_ENTRY;
        // SAFETY: la cola es memoria nuestra y el slot cae adentro.
        unsafe {
            zero(at, SQ_ENTRY);
            // Los primeros ocho bytes son dos palabras de 32: la primera lleva
            // el opcode abajo y la etiqueta arriba, la segunda el namespace.
            // Etiquetar cada comando es lo que permite reconocer su respuesta.
            poke64(
                at,
                (opcode as u64) | ((self.tag as u64) << 16) | ((nsid as u64) << 32),
            );
            // A donde escribe lo que devuelva.
            poke64(at + 24, prp1);
            poke64(at + 40, cdw10 as u64);
        }

        self.sq_tail = (slot + 1) % ENTRIES;
        let bell = self.bell(0, 0);
        k.write_reg(self.window.handle, bell, self.sq_tail, 4)?;

        // Y esperar. La respuesta se reconoce por un bit que **alterna** en cada
        // vuelta del anillo: no alcanza con mirar si hay algo escrito, porque lo
        // de la vuelta anterior tambien esta escrito.
        let entry = self.cq.start + self.cq_head * CQ_ENTRY;
        let mut spins = 0u32;
        loop {
            // La ultima palabra de la respuesta lleva la etiqueta abajo y el
            // estado arriba; el bit de fase es el de mas abajo del estado.
            // SAFETY: la cola de respuestas es memoria nuestra.
            let status = ((unsafe { peek64(entry + 8) } >> 32) >> 16) & 0xFFFF;
            if (status & 1) == self.phase {
                self.cq_head = (self.cq_head + 1) % ENTRIES;
                if self.cq_head == 0 {
                    // Dio la vuelta: de aca en adelante el bit vale al reves.
                    self.phase ^= 1;
                }
                let bell = self.bell(0, 1);
                k.write_reg(self.window.handle, bell, self.cq_head, 4)?;
                self.tag = self.tag.wrapping_add(1);
                // Los bits de arriba del estado dicen si lo rechazo.
                return if (status >> 1) & 0x3FF == 0 { Some(()) } else { None };
            }
            spins += 1;
            if spins >= SPINS {
                return None;
            }
        }
    }

    /// Le pregunta al disco cuanto mide y de que tamano son sus bloques.
    pub fn namespace(&mut self, k: &mut Session, nsid: u32) -> Option<Namespace> {
        let buf = claim_shared(k, self.bdf, 4096)?;
        // Opcode 6 = Identify, cdw10 = 0 pide la ficha de un namespace.
        self.command(k, 0x06, nsid, buf.start, 0)?;

        // SAFETY: el aparato acaba de escribir ahi por DMA, y es memoria nuestra.
        let blocks = unsafe { peek64(buf.start) };
        // Cual de los formatos esta en uso, y de ahi el tamano de bloque: viene
        // como potencia de dos, no como numero de bytes.
        let which = ((unsafe { peek64(buf.start + 24) } >> 16) & 0xF) as u64;
        // Cada descriptor de formato son cuatro bytes, asi que el de indice
        // impar no cae alineado a ocho: se lee de a cuatro.
        let lbaf = unsafe { peek32(buf.start + 128 + which * 4) } as u64;
        let shift = (lbaf >> 16) & 0xFF;
        if shift < 9 || shift > 16 {
            return None;
        }
        Some(Namespace { blocks, block_bytes: 1 << shift })
    }
}
