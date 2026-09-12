# El protocolo

## El framing

### JSON-RPC 2.0, con un objeto por línea

JSON-RPC 2.0 con **framing newline-delimited**: cada mensaje es un objeto JSON en una línea, terminado en `\n`. Va sobre el [transporte](transport.md) que corresponda al sistema operativo, y no sabe cuál es.

```json
{"jsonrpc":"2.0","id":1,"method":"callers",
 "params":{"file":"/abs/path/src/check.rs","line":261,"col":4}}
```

## Las preguntas

### Los métodos

| Método | Params | Resultado |
|---|---|---|
| `callees` | `{file, line, col}` | `[CalleeInfo]` — llamadas salientes |
| `callers` | `{file, line, col}` | `[CalleeInfo]` — llamadas entrantes |
| `symbol_at` | `{file, line, col}` | `SymbolInfo` o `null` |
| `definitions` | `{file, line, col}` | `[DefinitionInfo]` — dónde está declarado lo que se menciona ahí |
| `status` | — | `[LspStatus]` |
| `ping` | — | `"pong"` |
| `shutdown` | — | `null`, y cierra la conexión |

`CalleeInfo` es `{symbol, name, file, line, col}`; `callees` y `callers` comparten esquema. `SymbolInfo` es `{symbol, name, kind}`. `LspStatus` es `{name, state, queries}`, con `state` en `INDEXING | READY | RUNNING` — ver [los language servers](language-servers.md#un-servidor-que-no-informa-su-estado-no-se-puede-esperar). `DefinitionInfo` es `{name, file, line, col, end_line, end_col}`.

### `definitions` es la única pregunta que no es del call graph

La pide bilinker para su cierre de firma: dónde están declarados los tipos que una firma menciona.

Es **un salto y ninguna interpretación**. `lspd` devuelve ubicaciones; no lee el contenido, no lo hashea y no sabe para qué se lo piden. Las tres formas que LSP permite en la respuesta —una, varias, o links— se aplanan a ubicaciones antes de salir: cuál usó el servidor de atrás no es un dato de quien pregunta.

Y la dedup la hace el language server: las tres menciones de `Persona` en `Persona hijoMenor(Persona padre, Persona madre)` resuelven a la misma declaración, así que el vecindario tiene **un** `Persona` y no tres.

### `line` y `col` son 0-based

**`line` y `col` son 0-based**, como en LSP y a diferencia del resto del ecosistema. La conversión es de quien pregunta: traducirla acá sería ponerle al daemon una convención que no es suya.

## Lo que viaja

### Hacia abajo viaja lo mismo, y por eso hay que escribirlo

Esta página es la frontera de arriba —entre quien pregunta y el daemon—, y hay otra abajo: el LSP que `lspd` habla con cada language server. **Lo que cruza la de abajo es lo mismo que cruzó la de arriba: un path y una posición.** Nunca el contenido del archivo.

Decirlo acá parece de más, porque de arriba nunca viajó un contenido y no hay de dónde sacarlo. Pero es exactamente ahí donde se agrega: la traducción de `{file, line, col}` a una pregunta LSP es el único lugar del sistema donde alguien puede decidir leer el archivo, y una spec que sólo describe la frontera de arriba no tiene dónde decir que no.

**Y no es una preferencia de eficiencia: es que la petición con el contenido adentro puede mentir.** Una petición que lleva una copia del archivo es una foto. Si se resuelve más tarde —y encolada siempre se resuelve más tarde—, se resuelve contra un archivo que pudo cambiar, y la respuesta se atribuye al estado de hoy. Nada en el sistema puede detectar esa mentira, porque la petición trae su propia versión de la verdad. Una que lleva `{path, línea, columna}` **no puede estar desincronizada**: no tiene con qué, y se resuelve contra el disco en el momento en que se la atiende.

Hay una segunda consecuencia del mismo hecho: **N preguntas sobre el mismo archivo son N punteros y no N copias.** Lo que se encole deja de crecer con el tamaño de los archivos que alguien esté consultando.

### El servidor ya tiene el archivo

`lspd` arranca a cada language server **parado en el workspace** y le declara sus raíces —ver [los language servers](language-servers.md)—, así que lee del mismo filesystem que el daemon. Mandarle un archivo que puede abrir solo es trabajo que no hace falta.

**`did_open` existe en LSP porque un editor tiene buffers sin guardar**, y ahí el cliente es la única fuente de verdad de lo que el usuario está viendo. `lspd` no es un editor: no tiene buffers, y quien le pregunta mira contenido commiteado. **El disco es la fuente de verdad de las dos puntas**, así que un `did_open` acá no sincroniza nada — paga por una diferencia que no existe.

Es el mismo dato que esta capa ya usó del otro lado, cuando decidió arrancar al servidor parado en el workspace: **corre en esta máquina, al lado.**

> **Y no se saca por rápido: medido, no cambió el reloj.** El 2026-09-03, sobre un repo Java con 391 preguntas por corrida, sacar el `did_open` dio **25,04 s → 24,98 s**. Todo el tiempo estaba en otro lado —ver [los language servers](language-servers.md) § *"Uno por lenguaje no es una pregunta por vez"*—, y los ~66 ms de una pregunta nunca fueron el servidor reprocesando el documento.
>
> Queda escrito porque es lo que evita que vuelva. *"No costaba nada"* es un argumento para reponerlo, y no alcanza: **la razón por la que no está es que la petición no puede llevar una copia de la verdad.**

## Cuándo se puede contestar

### `ping` no es diagnóstico

Es cómo se decide si hay daemon. Un consumidor que quiera arrancarlo si no está pregunta `ping` y mira si contesta; no hay archivo de pid que consultar ni proceso que enumerar.

**Y contesta antes de que los language servers estén listos.** Eso es deliberado y es la distinción que lattice llama `Degraded`: el daemon está, pero el servidor de atrás sigue indexando. Confundir *"todavía no sé"* con *"no hay"* es la equivocación más cara que puede cometer un consumidor, y por eso el protocolo no las junta en un booleano.

**Pero no alcanza con no juntarlas: hay que contestar la diferencia.** Un `ping` que dice `pong` mientras el servidor indexa deja al consumidor con un vacío que no puede interpretar, y pedirle que además consulte `status` antes de cada pregunta no cierra el caso — **los servidores se levantan a demanda**, así que en la primera pregunta de un lenguaje no hay todavía un `state` que consultar. El único momento en que la distinción se puede dar es al contestar.

Así que **una pregunta que llega mientras el servidor está `INDEXING` se contesta con error**, no con `[]`. Ver `-32001` más abajo.

## Los errores

### Un error es del método, no del transporte

Una respuesta con `error` es una respuesta: el daemon está vivo y esa pregunta no se pudo contestar. Que el socket no exista o que se corte es otra cosa, y la distingue el cliente.

**Y el cliente conserva el código.** Una tabla de códigos que llega al consumidor aplastada a un mensaje no es una tabla: obliga a matchear texto, y el texto se reescribe. `lspd-client` entrega el código junto al mensaje, que es lo que permite tratar `-32001` distinto de `-32000` sin leer prosa.

| | |
|---|---|
| `-32700` | el JSON no parsea |
| `-32601` | no existe ese método |
| `-32602` | los params no tienen la forma que el método espera |
| `-32000` | el language server falló o no hay soporte para ese lenguaje |
| `-32001` | el language server está `INDEXING`: todavía no puede contestar |

### `-32001` dice "volvé a preguntar", y no se espera

**`-32001` no es una falla, y por eso tiene código propio.** Dice *"volvé a preguntar"*, y `status` dice cuándo. Meterlo en `-32000` lo haría indistinguible de un servidor roto, que es la confusión de siempre corrida un casillero.

**Y no se espera.** Sería lo cómodo —bloquear hasta que el servidor esté listo y contestar bien— y está mal por dos motivos: los siete minutos de `rust-analyzer` no entran en el timeout de ningún cliente, y tapar el vacío con una espera es lo mismo que taparlo con un reintento. El daemon contesta lo que sabe en el momento en que se lo preguntan.

## Qué no está en el protocolo

### No hay streaming ni suscripciones

**Nada de streaming ni de suscripciones.** Una pregunta, una respuesta, y el consumidor pregunta cuando necesita. Un grafo de llamadas se expande nodo a nodo —no se enumera— así que no hay para qué empujar.

### El workspace no viaja en cada pregunta

**Ni el workspace.** Se lo fija al arrancar y no viaja en cada pregunta: un daemon indexa un workspace, y preguntarle por otro es arrancar otro.
