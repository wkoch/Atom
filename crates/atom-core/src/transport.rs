#![forbid(unsafe_code)]

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::error::CoreError;

pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

/// Lê uma mensagem NDJSON (uma linha, limite 16 MiB).
pub async fn read_message<R>(reader: &mut R) -> Result<Option<String>, CoreError>
where
    R: AsyncBufReadExt + Unpin,
{
    let mut line = String::new();
    let n = reader
        .read_line(&mut line)
        .await
        .map_err(|e| CoreError::Io(e.to_string()))?;
    if n == 0 {
        return Ok(None);
    }
    if line.len() > MAX_MESSAGE_BYTES {
        return Err(CoreError::Protocol(atom_protocol::RpcError::new(
            atom_protocol::ErrorCode::INVALID_REQUEST,
            "mensagem > 16 MiB",
        )));
    }
    let t = line.trim_end_matches(['\r', '\n']).to_string();
    if t.is_empty() {
        return Ok(Some(String::new()));
    }
    Ok(Some(t))
}

/// Escreve uma mensagem NDJSON.
pub async fn write_message<W>(writer: &mut W, msg: &str) -> Result<(), CoreError>
where
    W: AsyncWriteExt + Unpin,
{
    writer
        .write_all(msg.as_bytes())
        .await
        .map_err(|e| CoreError::Io(e.to_string()))?;
    writer
        .write_all(b"\n")
        .await
        .map_err(|e| CoreError::Io(e.to_string()))?;
    writer
        .flush()
        .await
        .map_err(|e| CoreError::Io(e.to_string()))
}

pub fn to_line(v: &serde_json::Value) -> Result<String, CoreError> {
    serde_json::to_string(v).map_err(|e| CoreError::Internal(e.to_string()))
}

/// Caminho do socket do perfil (spec protocol §1).
#[cfg(unix)]
pub fn socket_path(runtime: &std::path::Path, profile: &str) -> std::path::PathBuf {
    runtime.join(format!("atom-{profile}.sock"))
}

pub async fn read_stdio_line<R>(reader: &mut BufReader<R>) -> Option<String>
where
    R: tokio::io::AsyncRead + Unpin,
{
    match read_message(reader).await {
        Ok(v) => v.filter(|s| !s.is_empty()),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    #[tokio::test]
    async fn roundtrip_line() {
        let (mut a, b) = tokio::io::duplex(1024);
        write_message(&mut a, r#"{"a":1}"#).await.unwrap();
        let mut r = BufReader::new(b);
        assert_eq!(
            read_message(&mut r).await.unwrap(),
            Some(r#"{"a":1}"#.to_string())
        );
    }

    #[tokio::test]
    async fn empty_line_and_eof() {
        let (mut a, b) = tokio::io::duplex(1024);
        write_message(&mut a, "").await.unwrap();
        drop(a);
        let mut r = BufReader::new(b);
        // Linha vazia vira None em read_stdio_line, mas Some("") em read_message.
        assert_eq!(read_stdio_line(&mut r).await, None);
        assert_eq!(read_message(&mut r).await.unwrap(), None);
    }

    #[test]
    fn to_line_serializes() {
        let v = serde_json::json!({"m": "atom.call"});
        assert_eq!(to_line(&v).unwrap(), r#"{"m":"atom.call"}"#);
    }

    #[cfg(unix)]
    #[test]
    fn socket_path_namespaced() {
        let p = socket_path(std::path::Path::new("/run"), "dev");
        assert_eq!(p, std::path::PathBuf::from("/run/atom-dev.sock"));
    }
}
