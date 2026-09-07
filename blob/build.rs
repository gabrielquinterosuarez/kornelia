//! Lo unico que hace: avisarle a cargo que el enlazador tambien tiene fuente.
//!
//! Sin esto, cambiar `blob.ld` no reconstruye nada — cargo mira los `.rs` y el
//! script se le pasa al enlazador por la linea de comandos, asi que su
//! contenido le resulta invisible. El sintoma es peor que un error: se sigue
//! usando el binario viejo y parece que el cambio no hizo efecto.
fn main() {
    println!("cargo:rerun-if-changed=blob.ld");
}
