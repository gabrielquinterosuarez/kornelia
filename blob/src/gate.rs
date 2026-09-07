//! Las dos puertas por las que el blob habla con el kernel.
//!
//! El blob corre **sin privilegio** (D29), asi que no puede llamar a una funcion
//! del kernel ni aunque supiera donde esta. Lo que hay son dos instrucciones que
//! provocan una excepcion a proposito, y el kernel las distingue:
//!
//! - la de **servicio** dice "atendeme esto y devolveme el control", y el codigo
//!   sigue en la instruccion siguiente;
//! - la de **fin** dice "termine", con lo que quiera dejar.
//!
//! Los bytes de las dos los publica `describe exec` para que el agente no los
//! tenga horneados (P4). Aca **si** estan horneados, y es la unica forma: esto
//! es codigo maquina compilado para una arquitectura concreta, que no puede
//! preguntar nada antes de poder preguntar. Un blob de aarch64 ya sabe que es de
//! aarch64.

/// Le pide **un** verbo al kernel y sigue corriendo.
///
/// `req` son los bytes del pedido en CBOR y `out` es donde el kernel deja la
/// respuesta. Devuelve cuantos bytes ocupa, o cero si no se pudo — que incluye
/// el caso de que no entrara en `cap`.
///
/// Los cuatro argumentos van en los registros de la ABI de C **del kernel**, que
/// se compila para UEFI: en x86_64 eso es la ABI de Windows (`rcx`, `rdx`, `r8`,
/// `r9`), no la de Linux. Por eso van puestos a mano en vez de dejar que el
/// compilador los acomode: este crate se compila para bare-metal, donde
/// `extern "C"` significa la otra cosa, y el sintoma de equivocarse serian
/// cuatro punteros nulos vistos desde adentro del kernel.
#[cfg(target_arch = "x86_64")]
pub unsafe fn service(req: *const u8, len: usize, out: *mut u8, cap: usize) -> usize {
    let used: u64;
    // La ventanilla del kernel guarda `r10` y `r11` y devuelve el resultado en
    // `rax`, pero **no** guarda los cuatro de los argumentos: la ABI los deja en
    // manos del que llama, asi que se declaran como pisados.
    core::arch::asm!(
        "int 0x81",
        inlateout("rcx") req as u64 => _,
        inlateout("rdx") len as u64 => _,
        inlateout("r8") out as u64 => _,
        inlateout("r9") cap as u64 => _,
        lateout("rax") used,
    );
    used as usize
}

/// En aarch64 hay una sola ABI de C y los cuatro argumentos ya van en `x0`-`x3`.
/// La ventanilla preserva todo salvo `x0`, que trae el resultado.
#[cfg(target_arch = "aarch64")]
pub unsafe fn service(req: *const u8, len: usize, out: *mut u8, cap: usize) -> usize {
    let used: u64;
    core::arch::asm!(
        "svc #1",
        inlateout("x0") req as u64 => used,
        in("x1") len as u64,
        in("x2") out as u64,
        in("x3") cap as u64,
    );
    used as usize
}

/// Termina el blob, dejando `value` para que se lo pueda ver desde afuera.
///
/// El kernel informa **el primer registro** y nada mas: es lo unico que puede
/// contar sin saber que hace el blob, y alcanza para comprobar desde afuera que
/// corrio de verdad.
#[cfg(target_arch = "x86_64")]
pub fn finish(value: u64) -> ! {
    unsafe {
        core::arch::asm!(
            "int 0x80",
            in("rax") value,
            options(noreturn),
        )
    }
}

#[cfg(target_arch = "aarch64")]
pub fn finish(value: u64) -> ! {
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x0") value,
            options(noreturn),
        )
    }
}
