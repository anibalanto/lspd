# El ciclo de vida del daemon

Arrancarlo, pararlo y ver cómo está es lo único que `lspd` expone como comando: las preguntas van por el [protocolo](protocol.md), no por la línea de comandos.

## Los tres comandos

### `lspd start` arranca uno, y no dos

```
lspd start [--workspace <path>] [--wait [--lang <lenguaje>]... [--stall <segundos>]]
```

| Argumento | Default | Descripción |
|---|---|---|
| `--workspace` | cwd | Raíz del workspace. Los language servers se inicializan con este directorio. |
| `--wait` | — | Deja los servidores del workspace listos antes de volver. |
| `--lang` | los marcadores del workspace | Qué servidores calentar: `rust`, `java`, `typescript` o `python`. Repetible. Sólo con `--wait`. |
| `--stall` | 120 | Cuántos segundos se espera a un servidor `INDEXING` que no reporta progreso. Sólo con `--wait`. |

Arranca el daemon en background. Si ya hay uno corriendo, sin `--wait` **no hace nada y retorna 1** — arrancar dos sobre el mismo socket dejaría al segundo sin poder escuchar, y decirlo es más útil que fallar al bindear.

```
lspd started  pid=12345  endpoint=~/.lspd/accreta-impl-1c8540.sock
```

`endpoint` se imprime porque es lo que un consumidor va a mirar cuando algo no conecta, y cambia por sistema operativo. No se pasa: se deriva. Ver [el transporte](transport.md).

El daemon arranca en su propio grupo de procesos: una señal que la terminal le manda a `lspd start` no le llega a él.

### `lspd start --wait` deja los servidores listos

Con `--wait`, `start` hace tres cosas y vuelve con 0 cuando terminó:

1. **Levanta el daemon, o toma el que ya contesta** en la puerta de ese workspace. Un daemon vivo no es un error: imprime `lspd ya estaba corriendo` con su pid y su endpoint, y sigue.
2. **Calienta los servidores** con [`warm`](protocol.md#warm-arranca-los-servidores-y-no-espera-el-handshake): los de `--lang`, o los que digan los marcadores del workspace.
3. **Espera a que cada uno esté `READY`**, consultando `status`.

```
$ lspd start --wait
lspd started  pid=12345  endpoint=~/.lspd/accreta-impl-1c8540.sock
  rust-analyzer               INDEXING  0s
  rust-analyzer               INDEXING  30s
  rust-analyzer               READY     412s
```

**El avance va por stderr**: una línea por servidor cada vez que cambia de estado, y otra cada 30 segundos mientras siga `INDEXING`, con el tiempo que va desde que empezó la espera.

**Un servidor `RUNNING` no se espera.** No informa readiness, así que no hay a qué esperar: cuenta como listo apenas aparece.

Si los marcadores no dicen ningún lenguaje, `start --wait` lo dice por stderr, no calienta nada y retorna 0.

### `--lang` reemplaza la detección

`--lang <lenguaje>`, repetible, dice qué servidores calentar, y con él los marcadores del workspace no se miran. Es para el workspace cuyos marcadores no están en la raíz, o para no pagar un servidor que no se va a usar. Un lenguaje que no está en [la tabla](language-servers.md#la-tabla-despacha-por-extensión-a-un-ejecutable) es un error de uso, antes de tocar el daemon.

### Un servidor que no arranca hace fallar la espera, y no la corta

Un servidor que no arranca —el ejecutable no está, o su proceso termina antes de quedar listo, en el handshake o indexando— es un error de ese lenguaje: `start --wait` dice cuál y por qué por stderr, sigue esperando a los demás, y al final **retorna 1**. En CI eso tiene que verse, y no degradar en silencio.

Cuando el ejecutable no está, el porqué es el error que devuelve `warm`. Cuando el proceso termina después de arrancar, el daemon lo saca de su mapa y no guarda el porqué: `start --wait` lo ve desaparecer de `status` y dice eso.

### Un servidor que deja de reportar progreso deja de esperarse, y el daemon sigue

La espera no tiene tope total: **mientras un servidor `INDEXING` reporte progreso, se lo espera lo que haga falta.** Lo que la corta es el silencio. Un servidor que sigue `INDEXING` y lleva `--stall` segundos sin reportar progreso —lo que `status` dice en `since_progress_ms`— deja de esperarse: `start --wait` dice cuál y hace cuánto, sigue esperando a los demás, **retorna 1 y deja el daemon vivo**. Lo que ya indexó sirve a la corrida siguiente, y apagarlo es de `stop`.

La ventana por defecto es de 120 segundos. Medido el 2026-09-16, con caché fría: el silencio más largo de `rust-analyzer` mientras indexa fue de 6,8 s sobre el impl de lspd, 2,3 s sobre el de bilinker y 2,0 s sobre el de lattice.

Un daemon que no dice `since_progress_ms` no permite ver el silencio, y con él la espera no se corta.

En una terminal, la espera se corta con Ctrl-C, que tampoco apaga el daemon: corre en otro grupo de procesos.

### `lspd stop` cierra los language servers y termina

Manda `shutdown` a todos los language servers activos y termina el proceso. Si no hay daemon, lo dice y retorna 1.

### `lspd status` dice qué servidores hay, y en qué estado

```
$ lspd status

lspd  pid=12345  endpoint=~/.lspd/accreta-impl-1c8540.sock

language servers:
  rust-analyzer               READY     queries=147  progreso hace 212s
  jdtls                       INDEXING  queries=3    progreso hace 1s
  typescript-language-server  RUNNING   queries=32   progreso hace 540s
```

Un daemon recién arrancado no tiene ninguno: se levantan **por lenguaje y a demanda**, la primera vez que llega una pregunta sobre un archivo de ese lenguaje. `(ninguno arrancado todavía)` es un estado normal y no un problema.

Los tres estados están en [los language servers](language-servers.md#un-servidor-que-no-informa-su-estado-no-se-puede-esperar). El que hay que saber leer es el tercero: **`RUNNING` no es peor que `READY`, es que ese servidor no informa readiness** y por eso `lspd` no la afirma. Un `INDEXING` con `queries` arriba de cero es normal y es lo que este comando existe para mostrar — son las preguntas que se contestaron con `-32001`, y dicen cuándo conviene volver.

**`progreso hace` dice cuánto pasó desde la última señal de avance del servidor**: un `$/progress`, o una de las notificaciones de readiness. Un servidor que todavía no mandó ninguna cuenta desde que arrancó. Es lo que distingue un `INDEXING` que avanza de uno estancado.

## Quién lo arranca

### Arrancarlo no es del daemon

`lspd start` existe para la persona que quiere arrancarlo a mano. **Un consumidor no lo usa**: pregunta `ping`, y si no hay nadie levanta el binario con la política que le convenga. Lattice lo hace apenas el proveedor `lsp` hace falta; el adaptador de bilinker lo hace cuando la puerta de su workspace no contesta, y lo espera. Las dos son decisiones de ellos y no de acá: `lspd-client` les da el mecanismo —levantarlo, y esperar a que sus servidores estén listos—, y cuándo usarlo es de cada uno.

### El binario tiene que estar donde el consumidor lo encuentre

**El binario tiene que estar donde el consumidor pueda encontrarlo**: al lado de su propio ejecutable —el caso de un build local— o en PATH. Es la única cosa que `lspd` le pide al que lo usa, y es consecuencia de vivir en su propia capa: antes compartía el `target/` de lattice y aparecía solo.

El `lattice daemon` de lattice es este mismo ciclo de vida, con el nombre que los usuarios de lattice ya tenían.
