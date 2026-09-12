# Los comandos

`lspd` expone el ciclo de vida del daemon, y nada más: las preguntas van por el [protocolo](concepts/protocol.md), sobre el socket, y no por la línea de comandos ([lifecycle.md](concepts/lifecycle.md)).

| Comando | Uso | Qué hace |
|---|---|---|
| `start` | `start [--workspace <path>]` | Arranca el daemon en background, con el workspace que se le dice o el directorio actual, e imprime su pid y su endpoint. Si ya hay uno, no hace nada y retorna 1 ([lifecycle.md](concepts/lifecycle.md)). |
| `stop` | sin argumentos | Manda `shutdown` a todos los language servers activos y termina el proceso. Si no hay daemon, lo dice y retorna 1 ([lifecycle.md](concepts/lifecycle.md)). |
| `status` | sin argumentos | Lista el daemon —pid y endpoint— y sus language servers, cada uno con su estado y cuántas preguntas contestó ([language-servers.md](concepts/language-servers.md)). |

El endpoint no se pasa por argumento: se deriva del workspace ([transport.md](concepts/transport.md)).
