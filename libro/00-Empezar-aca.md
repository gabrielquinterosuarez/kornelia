---
tipo: portada
estado: vivo
---

# El kernel desde abajo

> [!info] Dos nombres provisorios
> El del libro y el del kernel. Mientras no haya otro, en la prosa se escribe **Kornelia** (con mayúscula); `kornelia/` en minúscula solo cuando es la ruta real de una carpeta. Cambiarlo después es un reemplazo sobre el vault y nada más. Ver [[_El-nombre]].

Un libro sobre **cómo funciona un kernel**, escrito desde el silicio hacia arriba, que usa un kernel de verdad —este— como caso de estudio en cada capítulo. La diferencia con los libros que ya existen es una columna: no solo *cómo lo hace Linux*, sino **qué se puede quitar y qué pasa cuando se quita**.

- **Cómo se estudia con esto** → [[El-metodo]]
- **Qué es lo que no entendí todavía** → [[dudas/_Como-se-usa-esta-carpeta|dudas/]]
- **Un término suelto** → [[Glosario]] · **Dos términos que confundo** → [[Falsos-amigos]]
- **La máquina se quedó muda** → [[Indice-de-sintomas]]

---

## Parte 0 · Cómo mirar

Sin herramientas, una práctica es una receta. Este capítulo va primero porque todos los demás lo usan.

- [[00-Como-mirar-una-maquina]] — qué le pregunto a Linux, qué le pregunto al silicio, y cómo se depura algo que se quedó mudo.

## Parte I · La máquina antes del kernel

Acá no hay kernel todavía. Hay silicio, y hace falta entender qué le pide un kernel al silicio antes de entender qué hace el kernel.

- [[01-El-reloj-y-el-transistor]] — por qué la computadora avanza a saltos y qué es un "ciclo".
- [[02-Registros-y-RAM-no-son-lo-mismo]] — la jerarquía, y por qué el kernel se pasa la vida moviendo cosas entre las dos.
- [[03-Que-hace-realmente-una-instruccion]] — traer, decodificar, ejecutar; y por qué el procesador miente sobre el orden.
- [[04-El-bus-tocar-algo-que-no-es-memoria]] — direcciones que no son RAM: MMIO y puertos.
- [[05-Caches-y-la-primera-mentira-util]] — líneas, coherencia, y por qué hay memoria que **no** se puede cachear.
- [[06-El-silicio-tiene-modos]] — privilegio como cosa física: anillos en x86, niveles de excepción en ARM.

## Parte II · Qué es un kernel

- [[07-Del-operador-humano-al-monitor-residente]] — el kernel apareció para no tener que cargar cintas a mano. Historia, con fotos.
- [[08-Monolitico-micro-exo-unikernel]] — la taxonomía real, y dónde cae este kernel.
- [[09-Que-cuesta-una-abstraccion]] — POSIX como acuerdo histórico, no como ley natural.
- [[10-Un-kernel-cuyo-usuario-no-es-humano]] — los seis principios de este proyecto, y qué preguntas contestan.

## Parte III · Del botón de encendido al primer código propio

- [[11-Reset-vector-firmware-BIOS-y-UEFI]] — qué corre antes que todo, y quién le pasa la máquina a quién.
- [[12-Que-te-da-UEFI-y-que-te-saca]] — la ventana antes de `ExitBootServices`, que se cierra una sola vez (D25).
- [[13-Cargar-una-imagen-PE-ELF-y-el-entry-point]] — cómo un archivo se convierte en código corriendo.
- [[14-El-primer-codigo-propio-la-pila]] — por qué la pila del firmware no sirve y hay que traer una.

## Parte IV · La máquina se describe

- [[15-Enumerar-sin-adivinar-ACPI]] — las tablas donde el firmware cuenta qué hay.
- [[16-El-otro-dialecto-device-tree]] — el mismo problema resuelto distinto, en big-endian y sin ACPI.
- [[17-PCIe-buses-funciones-y-BARs]] — cómo se encuentra un [[Aparato|aparato]] y dónde aparecen sus registros.
- [[18-Lo-que-la-maquina-no-dice]] — huecos, rangos `unreported`, y alcanzar algo sin enterarse de qué es (P4).

## Parte V · Memoria

- [[19-Memoria-fisica-el-mapa-y-los-huecos]]
- [[20-Tablas-de-paginas-de-verdad]] — cuatro niveles en x86_64, y lo mismo del otro lado.
- [[21-TLB-invalidacion-y-barreras]] — la caché que casi nadie nombra y que cuelga máquinas.
- [[22-Identity-map-la-mentira-mas-simple]] — por qué este kernel elige mapear todo uno a uno (D12).
- [[23-Asignadores-y-por-que-aca-no-hay]] — qué hace un `malloc` de kernel; qué queda cuando se saca (P2).
- [[24-Alineacion-la-promesa-que-el-cargador-no-cumple]]

## Parte VI · Privilegio y el mundo de usuario

- [[25-Como-se-baja-de-privilegio]] — `iretq` y `eret`: la instrucción con la que el kernel se hace a un lado.
- [[26-La-llamada-al-sistema]] — la puerta de vuelta: `syscall`, `svc`, `int 0x80`.
- [[27-La-ABI-la-pone-el-target-no-el-silicio]] — el bug que costó caro: `extern "C"` en UEFI es la convención de Windows.
- [[28-Que-es-un-proceso-y-que-queda-sin-procesos]] — D13: un solo agente, que se multiplica reclamando núcleos.

## Parte VII · Cuando algo sale mal

- [[29-Excepcion-interrupcion-trap-fault-abort]] — cinco palabras para cosas distintas. Ver también [[Falsos-amigos]].
- [[30-Capturar-un-fault-IDT-y-vectores]]
- [[31-La-pila-que-sobrevive]] — IST en x86_64, `SP_EL1` en aarch64: cómo se atiende una excepción cuando la pila es el problema.
- [[32-Los-faults-como-datos]] — `oops`/`panic` de Linux contra P5: el fault vuelve como respuesta.
- [[33-Recuperar-un-acceso-que-el-bus-rechaza]] — el kernel también toca memoria que puede fallar.

## Parte VIII · Interrupciones y tiempo

- [[34-Del-cable-al-numero-PIC-APIC-GIC]]
- [[35-MSI-interrupciones-sin-cable]]
- [[36-Nivel-contra-flanco]] — el pulso que se perdía, y cómo se encontró.
- [[37-El-reloj-contadores-y-no-saber-la-frecuencia]]
- [[38-Dormir-en-vez-de-girar]] — `hlt`, `wfi`, y el timbre del cable serie.
- [[39-Plazos-y-cortes]] — `deadline_ms`, el NMI, y por qué el FIQ no sirvió en esta máquina.

## Parte IX · Muchos núcleos

- [[40-Arrancar-el-segundo-nucleo]] — INIT/SIPI y un trampolín de 16→32→64 bits; PSCI del otro lado.
- [[41-Estado-por-nucleo]]
- [[42-Ordenamiento-de-memoria]] — por qué x86 esconde bugs que ARM muestra.
- [[43-Candados-atomicos-y-el-bug-de-una-en-cuatrocientas]]
- [[44-Mandar-trabajo-buzones-e-IPI]]

## Parte X · Hablar con el hardware

- [[45-Un-registro-no-es-RAM]] — el ancho del acceso, y las dos máquinas que no fallan igual.
- [[46-DMA-el-aparato-lee-memoria-solo]]
- [[47-IOMMU-VT-d-y-SMMUv3]] — hacen lo mismo y no se parecen en nada.
- [[48-Colas-en-memoria-el-patron-de-NVMe]]
- [[49-Escribir-un-driver]] — el modelo de Linux contra los once verbos (D4).

## Parte XI · Persistencia y arranque propio

- [[50-initramfs-modulos-y-el-huevo-y-la-gallina]]
- [[51-El-blob-y-la-ventana-de-rescate]] — D18: mecanismo sin contenido, y por qué avisa y espera.

## Parte XII · Lo que este kernel no tiene

Cada capa vacía es una decisión, y explicarla obliga a entender la capa (P2).

- [[52-Sin-scheduler]] · [[53-Sin-sistema-de-archivos]] · [[54-Sin-red]] · [[55-Sin-usuarios-ni-identidad]]
- [[56-Cada-capa-vacia-es-una-decision]]

## Parte XIII · Diseñar un kernel

- [[57-Como-se-toma-una-decision-y-como-se-anota]] — D1–D29 como caso de estudio.
- [[58-Una-prueba-que-no-puede-pasar-por-accidente]]
- [[59-Fronteras-verificadas]] — portabilidad como chequeo, no como intención (D23).
- [[60-Preguntas-abiertas]]

---

## Los conceptos

El libro se lee por capítulos; **se consulta por conceptos**. Cada uno es una ficha autónoma con el mismo molde: qué problema resuelve, cómo funciona, cómo lo hace Linux, cómo lo hace Kornelia y **qué se quitó**, cómo se ve roto, y una práctica.

#### El silicio

[[Flip-flop]] · [[Ciclo]] · [[Registro]] · [[Ejecucion-fuera-de-orden]] · [[Jerarquia-de-memoria]] · [[Cache]] · [[Bus]] · [[Modo-privilegiado]]

#### Memoria

[[MMU]] · [[Memoria-virtual]] · [[Tabla-de-paginas]] · [[Pagina]] · [[TLB]] · [[Espacio-de-direcciones]]

#### Aparatos

[[Aparato]] · [[MMIO]] · [[PCIe]] · [[BAR]] · [[DMA]] · [[IOMMU]] · [[NVMe]] · [[UART]] · [[Driver]]

#### Interrupciones y fallos

[[Interrupcion]] · [[Handler]] · [[MSI]] · [[Fault]] · [[Oops-y-panic]] · [[Syscall]]

#### Arranque y descripción de la máquina

[[ABI]] · [[Firmware]] · [[UEFI]] · [[ACPI]] · [[Device-tree]] · [[ELF-y-PE]]

#### Herramientas y entorno

[[Maquina-virtual]] · [[QEMU]] · [[Procfs-y-sysfs]] · [[Modulo-de-kernel]]

> [!tip] Si no sabés por dónde empezar
> [[Falsos-amigos]] primero. Casi toda la confusión inicial en sistemas es vocabulario, no conceptos: núcleo/kernel/core, interrupción/excepción/trap/fault, y las cuatro clases de dirección que parecen la misma.

## Dónde voy

> [!tip] Estas tablas necesitan el plugin **Dataview**
> Hasta que esté instalado se ven como bloques de código. Instalación en [[El-metodo]].

### Capítulos

```dataview
TABLE WITHOUT ID file.link AS "Capítulo", parte AS "Parte", estado AS "Estado", dificultad AS "Dif."
FROM "capitulos"
WHERE tipo = "capitulo"
SORT file.name ASC
```

### Conceptos que todavía no entiendo

```dataview
TABLE WITHOUT ID file.link AS "Concepto", estado AS "Estado"
FROM "conceptos"
WHERE estado != "entendido"
SORT file.name ASC
```

### Dudas abiertas

```dataview
TABLE WITHOUT ID file.link AS "Duda", abierta AS "Desde", sobre AS "Sobre"
FROM "dudas"
WHERE estado = "abierta"
SORT abierta DESC
```

### Prácticas pendientes

```dataview
TABLE WITHOUT ID file.link AS "Práctica", contra AS "Contra", estado AS "Estado"
FROM "practicas"
WHERE estado != "hecha"
SORT file.name ASC
```

### Conceptos sin práctica

Un concepto sin práctica es una creencia: se lee, se asiente y no se comprueba.

```dataview
LIST
FROM "conceptos"
WHERE !practicas
```
