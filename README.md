# lspd

Multiplexa language servers: mantiene uno vivo por lenguaje y contesta por un socket
local quién llama a qué.

**No es de nadie.** Lo usa [lattice](https://github.com/anibalanto/lattice) para el
proveedor `lsp`, y lo va a usar [bilinker](https://github.com/anibalanto/bilinker)
para el cierre de firma. La especificación vive acá, en
[`docs/specs/`](docs/specs/): un archivo por concepto en `concepts/`, y los comandos
en [`commands.md`](docs/specs/commands.md).

```
cargo build --release
lspd start          # arranca en background
lspd status
lspd stop
```

El endpoint se deriva y no se configura: `~/.lspd/daemon.sock` en Unix,
`\\.\pipe\lspd` en Windows.

## Lo que no es

**No tiene modelo del proyecto.** No sabe qué es una capa, ni un bilink, ni un nodo de
un grafo. Recibe `(archivo, línea, columna)` y devuelve posiciones. Todo lo que
signifique algo lo pone quien pregunta.

**No decide cuándo arrancar.** Se lo arranca; quién y con qué política es del
consumidor. Y no lleva nombre propio porque no tiene modelo propio: una tabla de
lenguajes y un socket.

## Crates

| | |
|---|---|
| `lspd` | el daemon: la tabla de lenguajes, los clientes LSP, el servidor |
| `lspd-client` | el cliente, compartido por los consumidores |
