# El ciclo de vida del daemon

Arrancarlo, pararlo y ver cómo está es lo único que `lspd` expone como comando: las preguntas van por el [protocolo](protocol.md), no por la línea de comandos.

## Los tres comandos

### `lspd start` arranca uno, y no dos

```
lspd start [--workspace <path>]
```

| Argumento | Default | Descripción |
|---|---|---|
| `--workspace` | cwd | Raíz del workspace. Los language servers se inicializan con este directorio. |

Arranca el daemon en background. Si ya hay uno corriendo, **no hace nada y retorna 1** — arrancar dos sobre el mismo socket dejaría al segundo sin poder escuchar, y decirlo es más útil que fallar al bindear.

```
lspd started  pid=12345  endpoint=~/.lspd/daemon.sock
```

`endpoint` se imprime porque es lo que un consumidor va a mirar cuando algo no conecta, y cambia por sistema operativo. No se pasa: se deriva. Ver [el transporte](transport.md).

### `lspd stop` cierra los language servers y termina

Manda `shutdown` a todos los language servers activos y termina el proceso. Si no hay daemon, lo dice y retorna 1.

### `lspd status` dice qué servidores hay, y en qué estado

```
$ lspd status

lspd  pid=12345  endpoint=~/.lspd/daemon.sock

language servers:
  rust-analyzer               READY     queries=147
  jdtls                       INDEXING  queries=3
  typescript-language-server  RUNNING   queries=32
```

Un daemon recién arrancado no tiene ninguno: se levantan **por lenguaje y a demanda**, la primera vez que llega una pregunta sobre un archivo de ese lenguaje. `(ninguno arrancado todavía)` es un estado normal y no un problema.

Los tres estados están en [los language servers](language-servers.md#un-servidor-que-no-informa-su-estado-se-reporta-running). El que hay que saber leer es el tercero: **`RUNNING` no es peor que `READY`, es que ese servidor no informa readiness** y por eso `lspd` no la afirma. Un `INDEXING` con `queries` arriba de cero es normal y es lo que este comando existe para mostrar — son las preguntas que se contestaron con `-32001`, y dicen cuándo conviene volver.

## Quién lo arranca

### Arrancarlo a mano no es lo que hace un consumidor

`lspd start` existe para la persona que quiere arrancarlo a mano. **Un consumidor no lo usa**: pregunta `ping`, y si no hay nadie levanta el binario con la política que le convenga. Lattice lo hace apenas el proveedor `lsp` hace falta; bilinker no lo hace nunca —degrada a *no verificado*— y las dos son decisiones de ellos y no de acá.

### El binario tiene que estar donde el consumidor lo encuentre

**El binario tiene que estar donde el consumidor pueda encontrarlo**: al lado de su propio ejecutable —el caso de un build local— o en PATH. Es la única cosa que `lspd` le pide al que lo usa, y es consecuencia de vivir en su propia capa: antes compartía el `target/` de lattice y aparecía solo.

El `lattice daemon` de lattice es este mismo ciclo de vida, con el nombre que los usuarios de lattice ya tenían.
