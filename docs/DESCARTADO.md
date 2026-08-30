# Lo que ya se descartó

Registro de caminos considerados y rechazados, con el motivo. **No los vuelvas a
proponer sin un argumento nuevo.**

## Sobre el encuadre del proyecto

| Idea | Por qué se descartó |
|---|---|
| Justificar el proyecto por **latencia** o rendimiento | Es el flanco débil. Los núcleos sin interrupciones de timer ya existen en Linux (`isolcpus`, `nohz_full`, `SCHED_FIFO`); un benchmark contra un Linux bien tuneado da diferencia dentro del ruido. El argumento fuerte es otro: **el sistema deja de adivinar la intención**. Todas las heurísticas de un OS (readahead, LRU, hugepages transparentes, migración NUMA) existen porque POSIX no permite declarar qué vas a hacer. Un agente que emite su plan completo las vuelve innecesarias. |
| "Las abstracciones son metáforas humanas" como argumento central | Es discutible y ataca terreno donde el proyecto pierde. El MMU es silicio, las tablas de páginas son estructura de hardware, la protección es física. Además el aislamiento importa **más** con un operador estocástico, no menos. |
| Quitar el aislamiento porque "el operador es una IA" | Al revés: un agente es un generador estocástico de código, incorrecto de formas menos predecibles y a mucho mayor volumen. |
| Correr el agente **adentro** desde el principio | Implicaría un stack de inferencia completo sobre bare metal, sin CUDA ni PyTorch: un orden de magnitud más de trabajo que todo el resto del proyecto. Queda como puerta abierta vía el blob-cargador (D1, D19). |

## Sobre el transporte

| Idea | Por qué se descartó |
|---|---|
| **Teclado PS/2** como canal | Es entrada de a un carácter, y la salida sería texto VGA que el agente tendría que leer de vuelta. Es reconstruir una terminal para que un agente la use: exactamente la capa antropocéntrica que el proyecto saca. |
| Stack de red **en el kernel** | El agente lo escribe y se lo entrega (D5). El kernel nunca necesita ARP, IP ni TCP. |
| **Abandonar el UART** al cambiar de transporte | Un cordón que se puede cortar no era un cordón (D17). El kernel escucha por ambos. |
| Que el kernel **juzgue** si el transporte nuevo anda (con timeout y vuelta atrás) | Sería una heurística adivinando intención — justo lo que el proyecto elimina. |
| **JSON** como formato del protocolo | Se transportan código máquina y volcados de memoria: obligaría a base64 (+33%) y a escapar strings. CBOR manda bytes crudos y lo parsea cualquier lenguaje (D6). |

## Sobre memoria y fallos

| Idea | Por qué se descartó |
|---|---|
| "Memoria física **sin traducción**" | Imposible en x86_64: el modo de 64 bits exige paginación (`CR0.PG=1`, `CR4.PAE=1`). No existe un modo sin MMU. Lo que sí se logra es presión de TLB casi nula con páginas de 1 GiB. |
| Que **el agente** arme las tablas de páginas por defecto | El formato es lo menos portable que hay (x86_64 4–5 niveles, ARM64 TTBR0/1, RISC-V Sv39/48/57). Rompería D3. Queda disponible: el agente puede armarlas y cargarlas desde su código en `exec`. |
| **Snapshot y restore** para deshacer un `exec` fallido | No es caro: es **imposible**. Un DMA que ya salió escribió; un registro de GPU ya escrito cambió el aparato; un paquete transmitido no se des-transmite. Prometer atomicidad sería mentir (D11). |
| **Direccionar bloques por embeddings** | Un embedding da vecino aproximado; el direccionamiento necesita un mapeo exacto e inyectivo. Se indexa por embedding, se direcciona por hash. |
| IOMMU apagado por defecto, "porque es un guardarraíl" | No lo es: hace cumplir lo que el agente declaró (P6). Y sin él, un DMA mal apuntado es corrupción silenciosa de memoria — el peor bug posible para un agente que depura su propio driver. Es instrumentación tanto como protección (D8). |

## Sobre organización

| Idea | Por qué se descartó |
|---|---|
| Varios agentes con aislamiento | Reintroduce procesos, permisos y planificador — todo lo que D10 saca. El agente se multiplica solo reclamando varios núcleos. |
| Limpiar los handles cuando se cae la conexión | El estado de la máquina no es una sesión: los núcleos siguen corriendo y los handlers atendiendo (D14). |
| Autenticación en el kernel | Con UART, quien tiene el cable ya puede resetear y reflashear: no compraría nada. Cuando el transporte pasa a ser red, la autenticación es del transporte que escribió el agente (D15). |
| Ponerle nombre propio ahora | Es marketing y no desbloquea nada técnico. La especificación usa "kernel" y "blob" (D21). |
| Dejar ARM "para más adelante" | Una frontera de portabilidad que no se prueba es ficción. Y x86 esconde bugs de concurrencia que ARM expone (D22). |

## Sobre hardware, para tener presente

- **NVIDIA**: los command rings, la GMMU y la inicialización de motores no están
  documentados, y desde Turing la placa no arranca sin firmware firmado
  criptográficamente. No se esquiva con ingenio. AMD e Intel sí son viables.
- **NVMe** es la joya: spec pública y estándar entre fabricantes. Un driver sirve para
  todos los SSD del mundo.
- **Intel HDA** (sonido) tiene spec pública completa: buena demo temprana.
