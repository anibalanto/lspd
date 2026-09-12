# Los language servers

`lspd` no habla ningún lenguaje. Despacha por extensión a un language server, le habla LSP por stdio, y traduce la respuesta al [protocolo](protocol.md).

## La tabla

### La tabla despacha por extensión a un ejecutable

| Extensión | Ejecutable buscado |
|---|---|
| `.rs` | `rust-analyzer` |
| `.ts` `.tsx` `.js` `.jsx` | `typescript-language-server` |
| `.py` | `jedi-language-server`, `pylsp` |
| `.java` | `jdtls` |

**Agregar un lenguaje es agregar una entrada.** Es todo el conocimiento de lenguajes que hay acá, y es a propósito: cualquier cosa más grande que una tabla sería modelo propio, y este daemon no tiene modelo propio.

Si ninguno de los candidatos está en PATH, la respuesta es un error explícito y no un vacío: `LSP for "rust-analyzer" not found: install one of ["rust-analyzer"]`. Un vacío se leería como *"no hay llamadas"*.

### Y la tabla tiene una columna más: qué pide cada servidor en el handshake

La tabla dice qué ejecutable buscar. **Lo que no dice, y hace falta, es qué necesita cada uno para arrancar de verdad**, porque LSP dejó ese pedazo abierto: `initializationOptions` es un campo libre y cada servidor pone ahí lo suyo.

| Servidor | Qué pide, y qué pasa sin eso |
|---|---|
| `rust-analyzer` | `experimental.serverStatusNotification` en las capabilities. Sin eso no avisa cuándo terminó de indexar |
| `jdtls` | `initializationOptions.workspaceFolders`. **Sin eso no importa el proyecto**: cae a su *proyecto invisible*, se queda sin classpath, y resuelve `[]` con el servidor en `READY`. Y `extendedClientCapabilities` con `progressReportProvider` y `classFileContentsSupport` en `false`: sin declararlos pide progreso con requests que nadie implementa, y espera respuesta |

**Sigue siendo una tabla, y por eso entra acá.** Es un dato por servidor —una constante, no una decisión—, y agregar un lenguaje sigue siendo agregar una fila. Lo que cambia es que la fila tiene dos casillas en vez de una.

### Y una más: con cuánta memoria se lo lanza

Un proceso hijo que no recibe un límite hereda el que el runtime del servidor calcule solo, y **eso no es cero: es lo que ese runtime decida por su cuenta.** La JVM que corre `jdtls` fija su heap máximo en **un cuarto de la RAM de la máquina** —7,8 GB en una de 32 GB— y arranca reservando 1 GB. Ninguno de los dos números los eligió `lspd`, y el primero cambia de máquina en máquina.

| Servidor | Con cuánto se lo lanza |
|---|---|
| `jdtls` | `--jvm-arg=-Xmx2G`. Sin eso, el techo es un cuarto de la RAM de la máquina y escala con ella |
| los demás | nada: no exponen un techo, y el suyo no crece con la máquina |

**Un techo que escala con la máquina es un techo que no protege a ninguna.** El daño no es que un servidor use mucho: es que lo que en la máquina del que desarrolla entra justo, en la del que tiene el doble de RAM se lleva puesta la sesión — y la que se lleva puesta es *la otra*, porque nadie prueba en la máquina grande. Un número fijo falla igual en las dos, que es lo que se quiere de un límite.

Y hay una razón de más para que sea explícito: **el que corre `lspd` casi nunca sabe que lo está corriendo.** Se levanta desde un `bilinker check` en el repo de otra empresa. Un cuelgue ahí no lo diagnostica quien lo causó.

> **Es la misma decisión que el `cwd`**, que el spawn ya toma con el mismo argumento: *un dato que el hijo hereda de quien lo lanzó es un dato que nadie eligió*. Acá también se elige.

**Lo que no entra es interpretar lo que el servidor conteste**: eso sí sería modelo propio. `lspd` le dice a cada uno lo que ese servidor necesita oír para arrancar, y de ahí en adelante todos se hablan igual.

> **Es la clase de dato que sólo se descubre corriéndolo.** Ninguno de los dos está en la especificación de LSP, y los dos aparecieron con el servidor prendido devolviendo respuestas bien formadas y vacías.

## El ciclo de un servidor

### Se levantan a demanda y se reusan

```
primera query de un lenguaje
  → detectar ejecutable → spawn vía stdio → handshake LSP → indexando → listo

queries siguientes
  → reusar la conexión

shutdown
  → shutdown de todos los language servers → el daemon termina
```

Un daemon recién arrancado no tiene ninguno levantado, y eso es normal: `status` lo dice.

### Uno por lenguaje es una invariante, y el mapa de clientes es quien la sostiene

**No hay servidor corriendo que el mapa no tenga.** Parece una consecuencia de levantarlos a demanda y no lo es: el diagrama de arriba se lee como si las queries llegaran una atrás de la otra, y no llegan —`check` sobre un repo pregunta en paralelo—, y *"primera query de un lenguaje"* no es un instante sino el tramo entero del handshake, que con `jdtls` son segundos.

Sin la invariante escrita, el mapa parece el censo de lo que corre y no lo es. Hay que sostenerla en dos lugares, y son dos preguntas distintas:

| Qué se sostiene | Contra qué |
|---|---|
| **Se levanta uno solo**, aunque N queries del mismo lenguaje lleguen juntas | que la ventana del handshake deje entrar a la segunda |
| **Un servidor que el mapa no conserva no existe** | que quede vivo un proceso al que ya nadie le puede hablar |

La primera se sostiene reservando el lugar en el mapa **antes** del handshake y no después: lo que se comparte es la promesa del cliente y no el cliente terminado, así que el que llega segundo espera esa misma promesa en vez de arrancar otro proceso. Hacerlo al revés —levantar y después ver si alguien ganó de mano— convierte cada arranque concurrente en un servidor de más.

La segunda es de propiedad, y es la que hace daño. **El proceso es del mapa**, y sacarlo del mapa *es* matarlo: no "además" matarlo, que es lo que se puede olvidar y lo que un `Drop` que nadie ejecuta finge cumplir. Un servidor huérfano no es un proceso ocioso de más — no se reusa, `status` no lo cuenta, y el `shutdown` de este diagrama le pasa por al lado. Con `jdtls` son gigas: cada JVM arranca en 1 GB y crece hasta un cuarto de la RAM de la máquina.

> **Y no es hipotético.** Medido: nueve JVMs huérfanas para un solo daemon, 22 GB, y la sesión de terminal entera muerta con ellas por compartir unidad systemd.

**Es lo que vuelve verdadero al `shutdown` de arriba.** Cerrar *"todos los language servers"* sólo puede significar algo si el mapa los tiene todos.

### Uno por lenguaje no es una pregunta por vez

Son dos cosas distintas y se confunden porque las decide el mismo candado. **La invariante de arriba es sobre procesos**: hay un `jdtls` y no nueve. *"Una pregunta por vez"* es sobre el tráfico hacia ese proceso, y de eso la invariante no dice nada — un servidor único puede tener N preguntas en vuelo.

Hacia cada servidor hay **un solo socket**, y los mensajes no se pueden entreverar. De ahí sale lo único que hay que serializar:

| Qué | Se serializa | Por qué |
|---|---|---|
| **escribir** al servidor | sí | hay un solo socket, y dos mensajes intercalados no son ninguno de los dos |
| **esperar** la respuesta | no | JSON-RPC correlaciona por `id`, así que N respuestas vuelven en cualquier orden y se reconocen |

**Y la espera es donde está el tiempo.** Un candado que cubre la operación entera —mandar la pregunta y esperar la respuesta— no protege un recurso: apaga el solapamiento que el `id` de JSON-RPC existe para permitir. El que se suelta al terminar de escribir deja N preguntas en vuelo sobre un socket, que es lo que el protocolo de abajo ya soportaba sin que nadie lo pidiera.

> **Y por eso la concurrencia se medía plana.** Variar cuántas preguntas iban a la vez, de 1 a 16, no cambiaba el reloj — lo que **no** decía que el servidor no paralelizara: decía que nunca le llegaba más de una a la vez.
>
> **Medido el 2026-09-03**, soltando el candado: un `check` de un archivo sobre un repo Java con 98 bilinks y 391 preguntas pasó de **25,04 s a 3,59 s**, con las mismas 391 preguntas y los mismos 98 resultados. **7×, y ninguno de esos segundos era trabajo.**

### Falta el techo, y el número no sale de acá

Soltar el candado antes deja pasar tantas preguntas en vuelo como tareas haya preguntando — otra vez un número que nadie eligió, como el `cwd` heredado y el techo de heap de las dos tablas de arriba. El techo se elige, y donde se elige es acá.

**Pero cuántas es del servidor, no del daemon**, y por eso no está escrito: lo que aguanta `rust-analyzer` no dice nada de lo que aguanta `jdtls`, y hasta que la espera deje de estar serializada el número no se puede medir. Va a ser una casilla más por servidor, como las otras dos.

**Lo que sí se sabe es que ese techo no multiplica procesos.** N preguntas en vuelo son N contra *un* servidor, no N servidores — y vale escribirlo porque un conjunto de trabajadores es exactamente la forma en que la invariante de arriba se rompería sin querer: cada uno "asegurándose" el suyo son las nueve JVMs otra vez.

## La readiness

### Terminar el handshake no es estar listo

**Entre el handshake y la primera respuesta útil hay un tramo, y durante ese tramo el servidor contesta vacío.** Medido: `rust-analyzer` sobre un workspace mediano tarda siete minutos en dejar de estar ocupado, y en todo ese rato `definitions` devuelve `[]` con el proceso vivo y el handshake terminado.

Un vacío ahí no significa lo que significa después. Es la misma regla que esta página ya aplica al ejecutable que falta —*"la respuesta es un error explícito y no un vacío; un vacío se leería como no hay llamadas"*— y el mismo tramo que lattice llama `Degraded`. Lo que cambia es de qué lado se resuelve: **lattice lo infiere de que acaba de arrancar el daemon, y eso sólo sirve para quien lo arrancó.** Quien encuentra un daemon ya prendido no tiene de dónde inferirlo, así que la señal la tiene que dar `lspd`.

### Y quien la tiene es el servidor

No se cronometra ni se adivina: los dos servidores que importan lo dicen, cada uno con su extensión, y son las dos notificaciones que el daemon escucha.

| Servidor | Notificación | Dice que está listo cuando |
|---|---|---|
| `rust-analyzer` | `experimental/serverStatus` | `quiescent: true` |
| `jdtls` | `language/status` | `type: ServiceReady` |

La de `rust-analyzer` **hay que pedirla**: sin `experimental.serverStatusNotification` en las capabilities del `initialize`, no la manda. La de `jdtls` viene sola.

### Un servidor que no informa su estado no se puede esperar

`typescript-language-server` y los de Python no mandan ninguna de las dos, y `lspd` no puede inventar la señal — cronometrar el arranque sería adivinar, y adivinar mal en la dirección cara.

Así que la readiness tiene **tres** valores y no dos, y el tercero es el honesto:

| `state` | Qué dice |
|---|---|
| `INDEXING` | el servidor dijo que todavía no está listo |
| `READY` | el servidor dijo que sí |
| `RUNNING` | está arriba, y **este servidor no informa readiness** |

`RUNNING` no es un estado degradado ni un error: es lo que hoy vale para todos, y seguir llamándolo así deja dicho que para ese lenguaje la distinción no se puede dar. **Es la información que el consumidor necesita para saber cuánto vale un vacío**, y esconderla atrás de un `READY` optimista sería volver al problema con otro nombre.

**Que se reusen es la razón de que exista un daemon.** Un `rust-analyzer` tarda decenas de segundos en indexar un proyecto mediano; arrancarlo por consulta haría que preguntar por el call graph cueste más que leer el código.

> Idle timeout por language server: no implementado.

## El disco

### Lo que deja en el disco

```
~/.lspd/
  accreta-impl-1c8540.sock    ← el socket, en Unix: creado al arrancar, borrado al terminar
  accreta-impl-1c8540.pid     ← el pid del proceso
```

**Un archivo por workspace, con el nombre de su puerta**: los dos últimos segmentos visibles del path y un hash corto ([transporte](transport.md)). En Windows el endpoint es un named pipe y no un archivo, así que sólo queda el `.pid`.

**Nada de esto es del proyecto.** El daemon no escribe en el árbol que indexa: arrancarlo desde un comando de sólo lectura es un efecto, y vale la pena que el efecto esté acotado a un directorio del usuario.

Un socket que existe pero no contesta está stale, y quien lo encuentre así arranca uno nuevo.

> `daemon.log`: no implementado — stderr va a `/dev/null`. Para ver logs, lanzar el binario a mano redirigiendo stderr.
