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

/// Una cola: dos anillos en RAM y por donde va cada uno.
#[derive(Clone, Copy)]
struct Queue {
    /// Donde el que manda escribe pedidos.
    sq: Claim,
    /// Y donde el aparato escribe respuestas.
    cq: Claim,
    entries: u64,
    sq_tail: u64,
    cq_head: u64,
    /// El bit que **alterna** en cada vuelta del anillo. Sin el no se puede
    /// distinguir una respuesta nueva de la de la vuelta anterior, que tambien
    /// esta escrita.
    phase: u64,
}

impl Queue {
    const fn blank() -> Self {
        Queue {
            sq: Claim { handle: 0, start: 0 },
            cq: Claim { handle: 0, start: 0 },
            entries: 0,
            sq_tail: 0,
            cq_head: 0,
            phase: 1,
        }
    }
}

/// Cual es cual. La de administracion es siempre la cero, y no lee discos: para
/// eso hay que crearle una de datos, que es lo primero que hace un cargador.
const ADMIN: usize = 0;
const DATA: usize = 1;

/// Un controlador NVMe listo para recibir comandos.
pub struct Nvme {
    /// Como lo nombra el bus. Es lo que hay que decirle al IOMMU.
    pub bdf: u64,
    /// Su ventana de registros.
    window: Claim,
    /// Cada cuanto esta el timbre de la cola siguiente. Lo dice el aparato.
    stride: u64,
    queues: [Queue; 2],
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

        let mut queues = [Queue::blank(); 2];
        queues[ADMIN] = Queue { sq, cq, entries: ENTRIES, sq_tail: 0, cq_head: 0, phase: 1 };
        Some(Nvme { bdf, window, stride, queues, tag: 1 })
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
    /// Manda un comando a una cola y espera su respuesta.
    ///
    /// Nadie interrumpe a nadie: se avisa moviendo la cola del anillo, que es
    /// una escritura a un registro del aparato, y despues se mira hasta que
    /// aparezca la respuesta.
    pub fn command(
        &mut self,
        k: &mut Session,
        which: usize,
        opcode: u8,
        nsid: u32,
        prp1: u64,
        cdw10: u32,
        cdw11: u32,
        cdw12: u32,
    ) -> Option<()> {
        // Se saca una copia y se devuelve al final: asi el resto de `self`
        // —la ventana, el timbre— sigue disponible mientras se usa la cola.
        let mut q = self.queues[which];
        if q.entries == 0 {
            return None;
        }
        let at = q.sq.start + q.sq_tail * SQ_ENTRY;
        // SAFETY: la cola es memoria nuestra y el lugar cae adentro.
        unsafe {
            zero(at, SQ_ENTRY);
            // Los primeros ocho bytes son dos palabras de 32: la primera lleva
            // el opcode abajo y la etiqueta arriba, la segunda el namespace.
            poke64(
                at,
                (opcode as u64) | ((self.tag as u64) << 16) | ((nsid as u64) << 32),
            );
            // A donde escribe (o de donde lee) lo que este comando mueva.
            poke64(at + 24, prp1);
            poke64(at + 40, (cdw10 as u64) | ((cdw11 as u64) << 32));
            poke64(at + 48, cdw12 as u64);
        }

        q.sq_tail = (q.sq_tail + 1) % q.entries;
        let bell = self.bell(which as u64, 0);
        k.write_reg(self.window.handle, bell, q.sq_tail, 4)?;

        let entry = q.cq.start + q.cq_head * CQ_ENTRY;
        let mut spins = 0u32;
        loop {
            // La ultima palabra de la respuesta lleva la etiqueta abajo y el
            // estado arriba; el bit de fase es el de mas abajo del estado.
            // SAFETY: la cola de respuestas es memoria nuestra.
            let status = ((unsafe { peek64(entry + 8) } >> 32) >> 16) & 0xFFFF;
            if (status & 1) == q.phase {
                q.cq_head = (q.cq_head + 1) % q.entries;
                if q.cq_head == 0 {
                    // Dio la vuelta: de aca en adelante el bit vale al reves.
                    q.phase ^= 1;
                }
                let bell = self.bell(which as u64, 1);
                k.write_reg(self.window.handle, bell, q.cq_head, 4)?;
                self.tag = self.tag.wrapping_add(1);
                self.queues[which] = q;
                // Lo que queda del estado dice si lo rechazo.
                return if (status >> 1) == 0 { Some(()) } else { None };
            }
            spins += 1;
            if spins >= SPINS {
                return None;
            }
        }
    }

    /// Le crea una cola de datos. Las de administracion no leen discos.
    ///
    /// Son dos comandos y **el orden importa**: primero la de respuestas, porque
    /// la de pedidos se crea diciendo a cual contesta. Al reves, el controlador
    /// rechaza el segundo.
    pub fn data_queue(&mut self, k: &mut Session) -> Option<()> {
        let size = ((ENTRIES - 1) << 16) as u32 | DATA as u32;

        let cq = claim_shared(k, self.bdf, 4096)?;
        // Opcode 5 = crear cola de respuestas. El bit 0 de cdw11 dice que la
        // cola es un bloque contiguo, que es lo que se acaba de reclamar.
        self.command(k, ADMIN, 0x05, 0, cq.start, size, 1, 0)?;

        let sq = claim_shared(k, self.bdf, 4096)?;
        // Opcode 1 = crear cola de pedidos. Arriba en cdw11 va a que cola de
        // respuestas le contesta.
        self.command(k, ADMIN, 0x01, 0, sq.start, size, ((DATA as u32) << 16) | 1, 0)?;

        self.queues[DATA] = Queue { sq, cq, entries: ENTRIES, sq_tail: 0, cq_head: 0, phase: 1 };
        Some(())
    }

    /// Lee bloques del disco **directo a memoria del agente**, por DMA.
    ///
    /// Es lo que hace que un cargador tenga sentido: nadie mueve esos bytes a
    /// mano, y menos por el cable. Con una sola direccion en el comando se llega
    /// hasta una pagina, asi que se parte de a paginas — una lista de punteros
    /// permitiria mas por comando y todavia no hace falta.
    pub fn read_into(
        &mut self,
        k: &mut Session,
        dest: u64,
        lba: u64,
        count: u64,
        ns: &Namespace,
    ) -> Option<()> {
        let per_page = 4096 / ns.block_bytes;
        let mut done = 0;
        while done < count {
            let left = count - done;
            let chunk = if left < per_page { left } else { per_page };
            let at = dest + done * ns.block_bytes;
            let block = lba + done;
            // Opcode 2 = leer. El bloque va partido en dos palabras, y cdw12
            // lleva **cuantos menos uno**: pedir cero bloques es pedir uno.
            self.command(
                k,
                DATA,
                0x02,
                1,
                at,
                block as u32,
                (block >> 32) as u32,
                (chunk - 1) as u32,
            )?;
            done += chunk;
        }
        Some(())
    }

    /// Le pregunta al disco cuanto mide y de que tamano son sus bloques.
    pub fn namespace(&mut self, k: &mut Session, nsid: u32) -> Option<Namespace> {
        let buf = claim_shared(k, self.bdf, 4096)?;
        // Opcode 6 = Identify, cdw10 = 0 pide la ficha de un namespace.
        self.command(k, ADMIN, 0x06, nsid, buf.start, 0, 0, 0)?;

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

// --- El formato del payload en el disco -------------------------------------
//
// D19 dice que el blob es un cargador y que el resto vive "en el bloque tal".
// Esto es ese acuerdo, y lo escribe `client.py`: una cabecera en el bloque 0 y
// el payload a continuacion.
//
// No hay sistema de archivos, y no es una carencia: un cargador que entiende
// FAT32 es mucho mas grande que uno que lee bloques por numero, y el payload lo
// escribe el mismo que escribe el cargador. Nombres de archivo no hacen falta
// cuando hay una sola cosa que traer.

/// `KORNELIA`, como se lee de a ocho bytes en little-endian.
const HEADER_MAGIC: u64 = 0x4149_4C45_4E52_4F4B;
/// La version del acuerdo que este cargador entiende.
const HEADER_VERSION: u32 = 1;
/// En que bloque empieza el payload.
const PAYLOAD_LBA: u64 = 1;

/// Lee un byte de memoria propia.
///
/// # Safety
///
/// `at` tiene que caer en memoria reclamada y alcanzable.
unsafe fn peek8(at: u64) -> u8 {
    core::ptr::read_volatile(at as *const u8)
}

/// Lo que el cargador trajo del disco.
pub struct Payload {
    /// Donde empieza a ejecutar.
    pub entry: u64,
    /// Cuanto mide, para poder informarlo.
    pub bytes: u64,
}

impl Nvme {
    /// Trae el payload del disco a memoria. **Esto es el cargador de D19.**
    ///
    /// Devuelve donde quedo, listo para saltar ahi.
    pub fn load(&mut self, k: &mut Session, ns: &Namespace) -> Option<Payload> {
        // La cabecera vive en el bloque cero.
        let head = claim_shared(k, self.bdf, 4096)?;
        self.read_into(k, head.start, 0, 1, ns)?;

        // SAFETY: el aparato acaba de escribir ahi por DMA, y es memoria nuestra.
        let magic = unsafe { peek64(head.start) };
        if magic != HEADER_MAGIC {
            // Un disco sin grabar son ceros, y de ahi no se puede concluir nada:
            // decir que no hay payload es distinto de decir que fallo.
            return None;
        }
        let version = unsafe { peek32(head.start + 8) };
        if version != HEADER_VERSION {
            // Negarse es lo correcto: un formato que no se entiende, leido como
            // si se entendiera, termina en un salto a cualquier lado.
            return None;
        }
        let size = unsafe { peek64(head.start + 16) };
        let entry = unsafe { peek64(head.start + 24) };
        let want = unsafe { peek64(head.start + 32) };
        if size == 0 || entry >= size {
            return None;
        }

        // Memoria para el payload, alcanzable sin privilegio: el blob corre
        // `supervised` (D29) y lo que cargue corre igual que el. Que este
        // declarada al IOMMU es lo que permite que el disco escriba ahi solo.
        let room = (size + 4095) / 4096 * 4096;
        let where_to = claim_shared(k, self.bdf, room)?;

        let blocks = (size + ns.block_bytes - 1) / ns.block_bytes;
        self.read_into(k, where_to.start, PAYLOAD_LBA, blocks, ns)?;

        // Y comprobar que llego entero. Sin esto, media lectura se ve como una
        // lectura buena hasta que el salto termina en cualquier lado.
        //
        // Es una suma y no un hash: alcanza para distinguir "se leyo entero" de
        // "se leyo la mitad", que es lo unico que puede fallar aca. Un disco no
        // miente a proposito.
        let mut sum: u64 = 0;
        let mut i = 0;
        while i < size {
            // SAFETY: cae adentro de lo que se reclamo y se acaba de llenar.
            sum = sum.wrapping_add(unsafe { peek8(where_to.start + i) } as u64);
            i += 1;
        }
        if sum != want {
            return None;
        }

        Some(Payload { entry: where_to.start + entry, bytes: size })
    }
}
