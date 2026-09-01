# lspd

Multiplexa language servers: mantiene uno vivo por lenguaje y contesta por un socket
local quién llama a qué.

**No es de nadie.** Lo usa [lattice](https://github.com/anibalanto/lattice) para el
proveedor `lsp`, y lo va a usar [bilinker](https://github.com/anibalanto/bilinker)
para el cierre de firma. La especificación vive en
[accreta](https://github.com/anibalanto/accreta), en `subsystems/lspd/`.

```
cargo build --release
lspd start          # arranca en background
lspd status
lspd stop
```

El endpoint se deriva y no se configura: `~/.lspd/daemon.sock` en Unix,
`\\.\pipe\lspd` en Windows.

## Crates

| | |
|---|---|
| `lspd` | el daemon: la tabla de lenguajes, los clientes LSP, el servidor |
| `lspd-client` | el cliente, compartido por los consumidores |
