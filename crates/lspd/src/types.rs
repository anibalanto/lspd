use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalleeInfo {
    pub symbol: String,
    pub name: String,
    pub file: String,
    pub line: u32,
    pub col: u32,
}

/// Dónde está declarado algo.
///
/// Es lo que contesta `definitions`, y lo que un consumidor necesita para ir a
/// buscar el texto: el archivo y el rango, en línea/columna 0-based como LSP.
/// **`lspd` no lee el contenido** — devuelve ubicaciones y nada más.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefinitionInfo {
    pub name: String,
    pub file: String,
    pub line: u32,
    pub col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolInfo {
    pub symbol: String,
    pub name: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LspStatus {
    pub name: String,
    pub state: String,
    pub queries: u64,
    /// Milisegundos desde la última señal de avance del servidor, o desde que arrancó.
    pub since_progress_ms: u64,
}

/// Lo que `warm` dice de cada lenguaje que le pidieron.
///
/// `name` es el mismo que en [`LspStatus`], para que quien calienta pueda seguir el
/// arranque en `status`. `error` está sólo en el que no pudo arrancar.
#[derive(Debug, Clone, Serialize)]
pub struct WarmInfo {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RpcRequest {
    pub jsonrpc: String,
    pub id: serde_json::Value,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct RpcResponse {
    pub jsonrpc: String,
    pub id: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

impl RpcResponse {
    pub fn ok(id: serde_json::Value, result: serde_json::Value) -> Self {
        Self { jsonrpc: "2.0".into(), id, result: Some(result), error: None }
    }

    pub fn parse_error(msg: String) -> Self {
        Self::err(serde_json::Value::Null, -32700, msg)
    }

    pub fn invalid_params(id: serde_json::Value, msg: String) -> Self {
        Self::err(id, -32602, msg)
    }

    pub fn method_not_found(id: serde_json::Value, method: &str) -> Self {
        Self::err(id, -32601, format!("method not found: {method}"))
    }

    /// El language server falló, no está instalado, o el lenguaje no tiene soporte.
    ///
    /// **`-32000`, el de la tabla del protocolo**, y no el `-32603` genérico de
    /// JSON-RPC: es el código que el cliente nombra `FAILED`, y un consumidor que
    /// clasifica por código tiene que recibir ése.
    pub fn server_error(id: serde_json::Value, msg: String) -> Self {
        Self::err(id, -32000, msg)
    }

    /// El language server está `INDEXING`: todavía no puede contestar.
    ///
    /// **Código propio y no `server_error`**, porque no es una falla: dice *"volvé a
    /// preguntar"*, y `status` dice cuándo. Mezclarlo con un servidor roto haría
    /// indistinguibles *"todavía no sé"* y *"no puedo"*, que es la misma confusión
    /// que este código existe para cerrar, corrida un casillero.
    pub fn not_ready(id: serde_json::Value, msg: String) -> Self {
        Self::err(id, -32001, msg)
    }

    fn err(id: serde_json::Value, code: i32, message: String) -> Self {
        Self { jsonrpc: "2.0".into(), id, result: None, error: Some(RpcError { code, message }) }
    }
}

#[cfg(test)]
mod codigos {
    use super::*;

    /// **La tabla del protocolo es la que manda.** Un language server que falló, que
    /// no está instalado, o un lenguaje sin soporte se contesta `-32000`: es el
    /// código que el cliente nombra `FAILED`, y un consumidor que lo clasifica por
    /// código no puede recibir otro.
    #[test]
    fn a_server_error_is_the_protocol_s_minus_32000() {
        let r = RpcResponse::server_error(serde_json::json!(1), "LSP for \"jdtls\" not found".into());
        assert_eq!(r.error.expect("es un error").code, -32000);
    }

    #[test]
    fn not_ready_is_minus_32001() {
        let r = RpcResponse::not_ready(serde_json::json!(1), "INDEXING".into());
        assert_eq!(r.error.expect("es un error").code, -32001);
    }
}
