//! La pila propia del kernel.
//!
//! # Por que hace falta
//!
//! La pila sobre la que el firmware nos llama sale de memoria que UEFI
//! clasifica como `BootServicesData`, y ese tipo pasa a ser **RAM libre** en
//! cuanto se llama a `ExitBootServices`. El mapa lo informa asi porque es la
//! verdad (P4).
//!
//! El problema aparece con `mem.claim`: el agente pediria memoria libre, el
//! kernel se la entregaria con toda razon, y el agente escribiria encima de la
//! pila del kernel. Corrupcion silenciosa, que es el peor caso posible — no
//! falla donde se escribio, falla despues y en otro lado.
//!
//! # La solucion
//!
//! Esta pila es un arreglo estatico, y por lo tanto vive **adentro de la imagen
//! del kernel**, que UEFI cargo como `LoaderData` y el mapa informa como
//! `Kind::Kernel`. Esa clase no se entrega nunca. El kernel se muda aca apenas
//! deja de necesitar al firmware, y desde entonces no pisa nada que pueda
//! reclamarse.
//!
//! No se da por sentado que haya salido bien: `verify` comprueba contra el mapa
//! real que la pila cayo donde tenia que caer.

/// 64 KiB.
///
/// De sobra para lo que hay: el protocolo trabaja sobre buffers estaticos y no
/// sobre la pila, y el recorrido de CBOR tiene el anidamiento con techo. El
/// numero es generoso a proposito — desbordar la pila de un kernel no da un
/// error, da memoria de al lado pisada.
const SIZE: usize = 64 * 1024;

/// Alineada a 16 porque las dos arquitecturas lo exigen: x86_64 por la ABI de
/// System V y aarch64 porque el hardware falla si SP no esta alineado.
#[repr(C, align(16))]
struct Stack([u8; SIZE]);

static mut STACK: Stack = Stack([0; SIZE]);

/// La direccion mas alta de la pila, que es por donde se empieza: crece hacia
/// abajo.
///
/// Apunta uno mas alla del final a proposito. No se escribe ahi: tanto `push`
/// como `str` con predecremento restan **antes** de escribir.
pub fn top() -> u64 {
    base() + SIZE as u64
}

/// La direccion mas baja. Pasar de aca es desbordar.
pub fn base() -> u64 {
    &raw const STACK as *const u8 as u64
}

/// Cuantos bytes ocupa.
pub const fn size() -> u64 {
    SIZE as u64
}
