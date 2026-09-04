---
tipo: meta
estado: vivo
---

# El nombre

**Kornelia es provisorio.** El del libro —*El kernel desde abajo*— también.

Mientras no haya otro:

- En prosa: **Kornelia**, con mayúscula, sin comillas ni cursiva.
- Como ruta o nombre de crate: `kornelia/`, `kernel-core`, tal como están en el repo.
- Nunca "el kernel Kornelia" (redundante) ni "KorneliaOS" (no es un sistema operativo).

## Cuando se decida el nombre

Cambiar el vault es un reemplazo de texto sobre `libro/`. Cambiar el repo es otra cosa y
no lo toca este libro.

```bash
grep -rl 'Kornelia' libro/ | xargs sed -i 's/Kornelia/NuevoNombre/g'
./libro/scripts/check-citas.py    # las citas no cambian, pero confirma que nada se rompió
```

## Candidatos

*(Anotalos acá cuando aparezcan, con qué te gusta y qué no. Un nombre que se elige
apurado se cambia dos veces.)*

| Nombre | A favor | En contra |
|---|---|---|
| Kornelia | Ya está en todos los commits. | No dice nada del proyecto. |
