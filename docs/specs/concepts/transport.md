# El transporte

Un socket local, y **nada que configurar**.

## La ruta

### La ruta se deriva, y no hay nada que configurar

| Sistema | Dónde |
|---|---|
| Unix — Linux, Mac | `~/.lspd/<workspace>.sock` |
| Windows | `\\.\pipe\lspd-<workspace>` |

No hay flag, ni variable de entorno, ni archivo de configuración. Quien quiera hablarle al daemon calcula la ruta con la misma regla y llega.

### La regla del nombre es una superficie versionada como cualquier otra

**Es la misma sólo si las dos puntas corren la misma versión de la regla**, y eso no está garantizado por nada: el daemon y el cliente se instalan por caminos distintos. `lspd` sale de esta capa; sus consumidores —`bilinker` y `lattice`— toman `lspd-client` **del remoto de git**, y el `Cargo.lock` que fija el commit no está versionado. Así que un clon nuevo resuelve la rama y coincide, y un checkout que ya existía se queda con el commit que resolvió la primera vez.

Medido el 2026-09-08: la regla del nombre cambió acá, el commit quedó sin publicar unas horas, y en esa ventana el `lspd` instalado abría `worklist-impl-1c8540.sock` mientras el `bilinker` instalado buscaba `impl-1c8540.sock`. Los dos calculaban *"la misma regla"*, cada uno la suya.

> **Un desfasaje de versión en la regla no se ve como un error: se ve como que no hay daemon.**

Y ése es el costo real de haber elegido derivar en vez de configurar. Una ruta configurada es un dato que las dos puntas leen del mismo lugar, y puede estar mal pero está *escrita*. Una ruta derivada es **código**, así que versionarla mal la parte en dos sin que ninguna de las dos se entere — el cliente no encuentra la puerta, y no encontrarla es exactamente lo que pasa cuando el daemon no está.

**No invalida el criterio, y no se cambia por configuración.** Lo que agrega es una condición que antes estaba implícita: la derivación vale mientras el nombre de la puerta sea una superficie versionada como cualquier otra. Cambiarlo es un cambio incompatible entre dos procesos, y el que lo hace tiene que publicar antes de que el otro lado lo necesite.

**Y se deriva de dos cosas, no de una.** Acá había **una sola puerta** —`daemon.sock`, del `HOME` y nada más— y de eso salía que hubiera **un daemon a la vez, con un workspace**.

> **La puerta única no era una decisión sobre concurrencia: era una consecuencia de haber derivado la ruta de una sola cosa.**

Medido el 2026-09-07: `bilinker check` en una capa de accreta falló con `file not found` sobre un archivo que existe, porque el daemon vivo estaba indexando otro proyecto — y ése tenía `jdtls` con **1544 consultas**, o sea alguien trabajando. Preguntarle a un daemon ajeno no devuelve *"no sé"*: devuelve **una negación**.

Con una puerta por workspace eso no se detecta: **no se puede representar.** El que contesta en mi puerta es el mío por construcción.

### Hay una puerta por workspace

`sun_path` son **108 bytes** en Linux, y un workspace real ya son 64 — `/home/anibal/Workspace/accreta/subsystems/worklist/.stratum/impl`. Más `~/.lspd/` y `.sock` queda en ~90: entra por poco, y **uno más profundo lo rompe**.

> **Un nombre legible para leerlo, un hash corto para distinguirlo.**

El hash sale del **path canónico**, así que dos rutas que apuntan al mismo lugar dan la misma puerta — un symlink no parte el daemon en dos.

Y el nombre legible no es decoración: sin él `~/.lspd/` es un directorio de hashes, y **un directorio de hashes no se puede mirar**. Con él, `ls` contesta de qué proyecto es cada puerta.

### El nombre son los dos últimos segmentos visibles, y un hash corto

Acá decía *"el basename para leerlo"*, y **medido el 2026-09-08 cuatro de las cinco puertas de esta máquina se llamaban `impl-<hash>`**. El basename de toda capa de stratum es literalmente `impl` —`subsystems/worklist/.stratum/impl`— así que la parte que existía para distinguir proyectos era la misma en todas.

> **El nombre se arma con los dos últimos segmentos que no estén ocultos.**

```
~/.lspd/worklist-impl-1c8540.sock
```

Saltear los ocultos es lo que saca `.stratum` del medio y deja pegados los dos segmentos que dicen algo: el subsistema y la capa. Son **dos y no más** porque dos entran, y el nombre legible se corta a 40 bytes — que es lo que vuelve al peor caso un número y no una esperanza: 40 más el hash y `.sock` son 52, contra los 108 de `sun_path`.

**Y no hay lista de nombres genéricos que saltear.** Era la otra salida —tratar `impl` como un segmento que no informa— y se descartó: `impl` es vocabulario de stratum, y el transporte de `lspd` no tiene por qué conocerlo. Un segmento oculto, en cambio, es una convención del sistema de archivos: saltearlo no le pide al cliente que sepa de qué proyecto es el workspace que le pasaron.

**El `daemon.pid` va por el mismo camino**, y por el mismo motivo: es lo que permite decir *qué* proceso atiende esta puerta.

### El workspace lo calcula quien llama

**El que llama.** Es el único que lo sabe: es la raíz que le va a preguntar, y es lo mismo que el daemon ya recibe en `--workspace`.

Derivarlo adentro del cliente sería adivinar desde dónde se lo invocó — y el `cwd` de quien pregunta no tiene por qué ser su workspace. Es justo el error que esto viene a borrar, cometido del otro lado del puerto.

**Que no haya configuración es el criterio con que se eligió el transporte**, no una consecuencia. Un socket local es lo único que se puede derivar de nada: existe en un lugar fijo del sistema de archivos, y ese lugar es el mismo para el que escucha y para el que llama.

## Por qué un socket local

### No es TCP en loopback

Sería **un solo camino de código** en vez de dos, y ahí se complica:

| | |
|---|---|
| puerto fijo | colisiona con cualquier otra cosa, y con otro `lspd` de otro usuario en la misma máquina |
| puerto dinámico | hay que publicarlo en algún lado, y ese lado es un archivo — el mismo problema con un salto más |
| puerto configurable | **ya es configuración**, que es lo que se estaba evitando |

Y deja algo escuchando en la máquina. En una laptop corporativa eso es una conversación con alguien, y este daemon no está pidiendo esa conversación: lo que quiere es hablar con procesos del mismo usuario.

### Dos transportes, un protocolo

Named pipes en Windows y sockets Unix en el resto son **dos implementaciones del transporte y una sola del protocolo**. Lo que va por adentro —JSON-RPC 2.0 con framing por líneas, ver [el protocolo](protocol.md)— es idéntico, y ninguna de las dos puntas sabe por cuál de los dos está hablando.

La frontera está donde tiene que estar: en obtener un par de streams de bytes. Todo lo de arriba es genérico sobre `AsyncRead + AsyncWrite`.

### Los dos sistemas se compilan

Unix y Windows se compilan los dos. `lspd` corre en el repo de cualquiera que use uno de sus consumidores, y ese repo puede estar en Windows.
